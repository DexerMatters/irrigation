//! What a declared tree says about itself.
//!
//! `#[tree_view]` generates typed accessors, so Rust code that names a tree also
//! knows how to read it. That is not enough for code that has to build or walk a
//! tree it was not written against: a parser publishing an AST it declared in a
//! different crate, a lowering pass writing an intermediate representation, an
//! inspector rendering whatever the host installed.
//!
//! A [`TreeLayout`] is that description as data: the types in declaration order,
//! each variant's leaf slots (with the payload kind each holds) and child slots
//! (with the cardinality and target type each holds). It carries no grammar, no
//! parser, and no runtime dependency — it is a description of one `tree_view`
//! declaration and nothing else.

use irrigation_runtime::ViewSchema;

/// The full description of one declared tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TreeLayout {
    /// Types in declaration order; the root type is at `root`.
    pub types: &'static [TypeLayout],
    /// Index of the root type in [`Self::types`].
    pub root: u16,
}

/// One declared enum type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TypeLayout {
    /// Type name, as declared.
    pub name: &'static str,
    /// Variants in declaration order.
    pub variants: &'static [VariantLayout],
}

/// One variant of one declared type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VariantLayout {
    /// Variant name, as declared.
    pub name: &'static str,
    /// Scalar fields, in declaration order.
    pub leaves: &'static [LeafLayout],
    /// Child fields, in declaration order.
    pub children: &'static [ChildLayout],
}

/// One scalar field of a variant.
///
/// The slot is the one its generated accessor reads: leaf fields are counted in
/// declaration order, separately from child fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeafLayout {
    /// Accessor name, as declared.
    pub name: &'static str,
    /// Leaf slot inside the node.
    pub slot: u16,
    /// Payload kind stored in that slot.
    pub kind: LeafKind,
}

/// One child field of a variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChildLayout {
    /// Accessor name, as declared.
    pub name: &'static str,
    /// Child slot inside the node.
    pub slot: u16,
    /// How many children the slot holds.
    pub cardinality: Cardinality,
    /// Index of the child type in [`TreeLayout::types`].
    pub target: u16,
}

/// How many children one child slot holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cardinality {
    /// Exactly one child.
    One,
    /// Zero or one child.
    Optional,
    /// Any number of children.
    Many,
}

/// The payload kind a leaf slot stores.
///
/// These are the storage kinds, not the Rust types: `u32` and `usize` leaves are
/// both [`Self::UInt`], and their accessors narrow on read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LeafKind {
    /// Text.
    Text,
    /// A signed integer.
    Int,
    /// An unsigned integer.
    UInt,
    /// A float.
    Float,
    /// A boolean.
    Bool,
}

impl TreeLayout {
    /// The index of the type named `name`.
    #[must_use]
    pub fn type_index(&self, name: &str) -> Option<u16> {
        self.types
            .iter()
            .position(|layout| layout.name == name)
            .and_then(|index| u16::try_from(index).ok())
    }

    /// The description of one type.
    #[must_use]
    pub fn type_layout(&self, index: u16) -> Option<&'static TypeLayout> {
        self.types.get(usize::from(index))
    }

    /// The root type's description.
    #[must_use]
    pub fn root_layout(&self) -> Option<&'static TypeLayout> {
        self.type_layout(self.root)
    }

    /// The variant named `name` inside the type at `type_index`.
    #[must_use]
    pub fn variant(&self, type_index: u16, name: &str) -> Option<(u16, &'static VariantLayout)> {
        let layout = self.type_layout(type_index)?;
        let index = layout
            .variants
            .iter()
            .position(|variant| variant.name == name)?;
        let index = u16::try_from(index).ok()?;
        layout.variants.get(usize::from(index)).map(|v| (index, v))
    }
}

impl TypeLayout {
    /// The variant named `name`.
    #[must_use]
    pub fn variant(&self, name: &str) -> Option<(u16, &'static VariantLayout)> {
        let index = self
            .variants
            .iter()
            .position(|variant| variant.name == name)?;
        let index = u16::try_from(index).ok()?;
        self.variants.get(usize::from(index)).map(|v| (index, v))
    }
}

impl VariantLayout {
    /// The leaf field named `name`.
    #[must_use]
    pub fn leaf(&self, name: &str) -> Option<&'static LeafLayout> {
        self.leaves.iter().find(|leaf| leaf.name == name)
    }

    /// The child field named `name`.
    #[must_use]
    pub fn child(&self, name: &str) -> Option<&'static ChildLayout> {
        self.children.iter().find(|child| child.name == name)
    }

    /// The number of child slots the variant declares.
    #[must_use]
    pub fn child_slots(&self) -> u16 {
        self.children.len() as u16
    }
}

/// A tree View that describes its declaration.
///
/// Generated by `#[tree_view]`. Code that publishes or walks a tree without
/// naming its accessors takes a `S: TreeSchema` and reads `S::LAYOUT`; code that
/// names the tree keeps using the generated accessors. Both address the same
/// slots, which is what the layout agreement test pins.
pub trait TreeSchema: ViewSchema {
    /// The declaration, as data.
    const LAYOUT: &'static TreeLayout;
}
