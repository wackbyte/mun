use crate::{
    garbage_collector::{GarbageCollector, UnsafeTypeInfo},
    DispatchTable,
};
use abi::AssemblyInfo;
use anyhow::anyhow;
use libloader::{MunLibrary, TempLibrary};
use log::error;
use memory::mapping::{Mapping, MemoryMapper};
use std::{
    path::{Path, PathBuf},
    ptr::NonNull,
    sync::Arc,
};

/// An assembly is a hot reloadable compilation unit, consisting of one or more Mun modules.
pub struct Assembly {
    library_path: PathBuf,
    library: TempLibrary,
    legacy_libs: Vec<TempLibrary>,
    info: AssemblyInfo,
    allocator: Arc<GarbageCollector>,
}

impl Assembly {
    /// Loads an assembly and its information for the shared library at `library_path`. The
    /// resulting `Assembly` is ensured to be linkable.
    pub fn load(library_path: &Path, gc: Arc<GarbageCollector>) -> Result<Self, anyhow::Error> {
        let mut library = MunLibrary::new(library_path)?;

        let version = library.get_abi_version();
        if abi::ABI_VERSION != version {
            return Err(anyhow::anyhow!(
                "ABI version mismatch. munlib is `{}` but runtime is `{}`",
                version,
                abi::ABI_VERSION
            ));
        }

        let allocator_ptr = Arc::into_raw(gc.clone()) as *mut std::ffi::c_void;
        library.set_allocator_handle(allocator_ptr);

        let info = library.get_info();
        let assembly = Assembly {
            library_path: library_path.to_path_buf(),
            library: library.into_inner(),
            legacy_libs: Vec::new(),
            info,
            allocator: gc,
        };

        Ok(assembly)
    }

    /// Tries to link the `assemblies`, resulting in a new [`DispatchTable`] on success. This leaves
    /// the original `dispatch_table` intact, in case of linking errors.
    pub(super) fn link_all<'a>(
        assemblies: impl Iterator<Item = &'a mut Assembly>,
        dispatch_table: &DispatchTable,
    ) -> Result<DispatchTable, anyhow::Error> {
        let assemblies: Vec<&'a mut _> = assemblies.collect();

        // Clone the dispatch table, such that we can roll back if linking fails
        let mut dispatch_table = dispatch_table.clone();

        // Insert all assemblies' functions into the dispatch table
        for assembly in assemblies.iter() {
            for function in assembly.info().symbols.functions() {
                dispatch_table.insert_fn(function.prototype.name(), function.clone());
            }
        }

        let mut to_link: Vec<_> = assemblies
            .into_iter()
            .flat_map(|asm| asm.info.dispatch_table.iter_mut())
            // Only take signatures into account that do *not* yet have a function pointer assigned
            // by the compiler.
            .filter(|(ptr, _)| ptr.is_null())
            .collect();

        let mut retry = true;
        while retry {
            retry = false;
            let mut failed_to_link = Vec::new();

            // Try to link outstanding entries
            for (dispatch_ptr, fn_prototype) in to_link.into_iter() {
                // Ensure that the function is in the runtime dispatch table
                if let Some(fn_def) = dispatch_table.get_fn(fn_prototype.name()) {
                    // Ensure that the function's signature is the same.
                    if fn_prototype.signature != fn_def.prototype.signature {
                        return Err(anyhow!("Failed to link: function '{}' is missing. A function with the same name does exist, but the signatures do not match (expected: {}, found: {}).", fn_prototype.name(), fn_prototype, fn_def.prototype));
                    }

                    *dispatch_ptr = fn_def.fn_ptr;
                    retry = true;
                } else {
                    failed_to_link.push((dispatch_ptr, fn_prototype));
                }
            }

            // Move all failed entries, for (potentially) another try
            to_link = failed_to_link;
        }

        if !to_link.is_empty() {
            for (_, fn_prototype) in to_link {
                error!(
                    "Failed to link: function `{}` is missing.",
                    fn_prototype.name()
                );
            }

            return Err(anyhow!("Failed to link due to missing dependencies."));
        }

        Ok(dispatch_table)
    }

    /// Swaps the assembly's shared library and its information for the library at `library_path`.
    pub fn swap(
        &mut self,
        library_path: &Path,
        runtime_dispatch_table: &mut DispatchTable,
    ) -> Result<(), anyhow::Error> {
        let mut new_assembly = Assembly::load(library_path, self.allocator.clone())?;

        let old_types: Vec<UnsafeTypeInfo> = self
            .info
            .symbols
            .types()
            .iter()
            .map(|ty| {
                // Safety: `ty` is a shared reference, so is guaranteed to not be `ptr::null()`.
                UnsafeTypeInfo::new(unsafe {
                    NonNull::new_unchecked(*ty as *const abi::TypeInfo as *mut _)
                })
            })
            .collect();

        let new_types: Vec<UnsafeTypeInfo> = new_assembly
            .info
            .symbols
            .types()
            .iter()
            .map(|ty| {
                // Safety: `ty` is a shared reference, so is guaranteed to not be `ptr::null()`.
                UnsafeTypeInfo::new(unsafe {
                    NonNull::new_unchecked(*ty as *const abi::TypeInfo as *mut _)
                })
            })
            .collect();

        let mapping = Mapping::new(&old_types, &new_types);
        let deleted_objects = self.allocator.map_memory(mapping);

        // Remove the old assembly's functions
        for function in self.info.symbols.functions() {
            runtime_dispatch_table.remove_fn(function.prototype.name());
        }

        new_assembly.link(runtime_dispatch_table);

        // Retain all existing legacy libs
        new_assembly.legacy_libs.append(&mut self.legacy_libs);

        std::mem::swap(self, &mut new_assembly);
        let old_assembly = new_assembly;

        if !deleted_objects.is_empty() {
            // Retain the previous assembly
            self.legacy_libs.push(old_assembly.into_library());
        }

        Ok(())
    }

    /// Returns the assembly's information.
    pub fn info(&self) -> &AssemblyInfo {
        &self.info
    }

    /// Returns the path corresponding to the assembly's library.
    pub fn library_path(&self) -> &Path {
        self.library_path.as_path()
    }

    /// Converts the `Assembly` into a `TempLibrary`, consuming the input in the process.
    pub fn into_library(self) -> TempLibrary {
        self.library
    }
}
