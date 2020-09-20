mod lower;
#[cfg(test)]
mod tests;

use crate::{
    arena::{Arena, Idx},
    source_id::FileAstId,
    type_ref::TypeRef,
    DefDatabase, FileId, InFile, Name,
};
use mun_syntax::{ast, AstNode};
use std::{
    any::type_name,
    fmt,
    fmt::Formatter,
    hash::{Hash, Hasher},
    marker::PhantomData,
    ops::{Index, Range},
    sync::Arc,
};

/// An `ItemTree` is a derivative of an AST that only contains the items defined in the AST.
#[derive(Debug, Eq, PartialEq)]
pub struct ItemTree {
    top_level: Vec<ModItem>,
    data: ItemTreeData,
}

impl ItemTree {
    /// Constructs a new `ItemTree` for the specified `file_id`
    pub fn item_tree_query(db: &dyn DefDatabase, file_id: FileId) -> Arc<ItemTree> {
        let syntax = db.parse(file_id);
        let item_tree = lower::Context::new(db, file_id).lower_module_items(&syntax.tree());
        Arc::new(item_tree)
    }

    /// Returns a slice over all items located at the top level of the `FileId` for which this
    /// `ItemTree` was constructed.
    pub fn top_level_items(&self) -> &[ModItem] {
        &self.top_level
    }

    /// Returns the source location of the specified item. Note that the `file_id` of the item must
    /// be the same `file_id` that was used to create this `ItemTree`.
    pub fn source<S: ItemTreeNode>(&self, db: &dyn DefDatabase, item: ItemTreeId<S>) -> S::Source {
        let root = db.parse(item.file_id);

        let id = self[item.value].ast_id();
        let map = db.ast_id_map(item.file_id);
        let ptr = map.get(id);
        ptr.to_node(&root.syntax_node())
    }
}

#[derive(Default, Debug, Eq, PartialEq)]
struct ItemTreeData {
    functions: Arena<Function>,
    structs: Arena<Struct>,
    fields: Arena<Field>,
    type_aliases: Arena<TypeAlias>,
}

/// Trait implemented by all item nodes in the item tree.
pub trait ItemTreeNode: Clone {
    type Source: AstNode + Into<ast::ModuleItem>;

    /// Returns the AST id for this instance
    fn ast_id(&self) -> FileAstId<Self::Source>;

    /// Looks up an instance of `Self` in an item tree.
    fn lookup(tree: &ItemTree, index: Idx<Self>) -> &Self;

    /// Downcasts a `ModItem` to a `FileItemTreeId` specific to this type
    fn id_from_mod_item(mod_item: ModItem) -> Option<LocalItemTreeId<Self>>;

    /// Upcasts a `FileItemTreeId` to a generic ModItem.
    fn id_to_mod_item(id: LocalItemTreeId<Self>) -> ModItem;
}

/// The typed Id of an item in an `ItemTree`
pub struct LocalItemTreeId<N: ItemTreeNode> {
    index: Idx<N>,
    _p: PhantomData<N>,
}

impl<N: ItemTreeNode> Clone for LocalItemTreeId<N> {
    fn clone(&self) -> Self {
        Self {
            index: self.index,
            _p: PhantomData,
        }
    }
}
impl<N: ItemTreeNode> Copy for LocalItemTreeId<N> {}

impl<N: ItemTreeNode> PartialEq for LocalItemTreeId<N> {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index
    }
}
impl<N: ItemTreeNode> Eq for LocalItemTreeId<N> {}

impl<N: ItemTreeNode> Hash for LocalItemTreeId<N> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.index.hash(state)
    }
}

impl<N: ItemTreeNode> fmt::Debug for LocalItemTreeId<N> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        self.index.fmt(f)
    }
}

/// Represents the Id of an item in the ItemTree of a file.
pub type ItemTreeId<N> = InFile<LocalItemTreeId<N>>;

macro_rules! mod_items {
    ( $( $typ:ident in $fld:ident -> $ast:ty ),+ $(,)?) => {
        #[derive(Debug,Copy,Clone,Eq,PartialEq,Hash)]
        pub enum ModItem {
            $(
                $typ(LocalItemTreeId<$typ>),
            )+
        }

        $(
            impl From<LocalItemTreeId<$typ>> for ModItem {
                fn from(id: LocalItemTreeId<$typ>) -> ModItem {
                    ModItem::$typ(id)
                }
            }
        )+

        $(
            impl ItemTreeNode for $typ {
                type Source = $ast;

                fn ast_id(&self) -> FileAstId<Self::Source> {
                    self.ast_id
                }

                fn lookup(tree: &ItemTree, index: Idx<Self>) -> &Self {
                    &tree.data.$fld[index]
                }

                fn id_from_mod_item(mod_item: ModItem) -> Option<LocalItemTreeId<Self>> {
                    if let ModItem::$typ(id) = mod_item {
                        Some(id)
                    } else {
                        None
                    }
                }

                fn id_to_mod_item(id: LocalItemTreeId<Self>) -> ModItem {
                    ModItem::$typ(id)
                }
            }

            impl Index<Idx<$typ>> for ItemTree {
                type Output = $typ;

                fn index(&self, index: Idx<$typ>) -> &Self::Output {
                    &self.data.$fld[index]
                }
            }
        )+
    };
}

mod_items! {
    Function in functions -> ast::FunctionDef,
    Struct in structs -> ast::StructDef,
    TypeAlias in type_aliases -> ast::TypeAliasDef,
}

macro_rules! impl_index {
    ( $($fld:ident: $t:ty),+ $(,)? ) => {
        $(
            impl Index<Idx<$t>> for ItemTree {
                type Output = $t;

                fn index(&self, index: Idx<$t>) -> &Self::Output {
                    &self.data.$fld[index]
                }
            }
        )+
    };
}

impl_index!(fields: Field);

impl<N: ItemTreeNode> Index<LocalItemTreeId<N>> for ItemTree {
    type Output = N;
    fn index(&self, id: LocalItemTreeId<N>) -> &N {
        N::lookup(self, id.index)
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Function {
    pub name: Name,
    pub is_extern: bool,
    pub params: Box<[TypeRef]>,
    pub ret_type: TypeRef,
    pub ast_id: FileAstId<ast::FunctionDef>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Struct {
    pub name: Name,
    pub fields: Fields,
    pub ast_id: FileAstId<ast::StructDef>,
    pub kind: StructDefKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeAlias {
    pub name: Name,
    pub type_ref: Option<TypeRef>,
    pub ast_id: FileAstId<ast::TypeAliasDef>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum StructDefKind {
    /// `struct S { ... }` - type namespace only.
    Record,
    /// `struct S(...);`
    Tuple,
    /// `struct S;`
    Unit,
}

/// A set of fields
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fields {
    Record(IdRange<Field>),
    Tuple(IdRange<Field>),
    Unit,
}

/// A single field of an enum variant or struct
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: Name,
    pub type_ref: TypeRef,
}

/// A range of Ids
pub struct IdRange<T> {
    range: Range<u32>,
    _p: PhantomData<T>,
}

impl<T> IdRange<T> {
    fn new(range: Range<Idx<T>>) -> Self {
        Self {
            range: range.start.into_raw().into()..range.end.into_raw().into(),
            _p: PhantomData,
        }
    }
}

impl<T> Iterator for IdRange<T> {
    type Item = Idx<T>;
    fn next(&mut self) -> Option<Self::Item> {
        self.range.next().map(|raw| Idx::from_raw(raw.into()))
    }
}

impl<T> fmt::Debug for IdRange<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple(&format!("IdRange::<{}>", type_name::<T>()))
            .field(&self.range)
            .finish()
    }
}

impl<T> Clone for IdRange<T> {
    fn clone(&self) -> Self {
        Self {
            range: self.range.clone(),
            _p: PhantomData,
        }
    }
}

impl<T> PartialEq for IdRange<T> {
    fn eq(&self, other: &Self) -> bool {
        self.range == other.range
    }
}

impl<T> Eq for IdRange<T> {}
