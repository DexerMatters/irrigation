//! Incremental collection Views and their operators.
//!
//! Every collection keeps element identity separate from position:
//!
//! * a source list stores stable element ids in its structure unit and each
//!   payload in its own unit;
//! * a map observes key presence and the key's value separately, so a payload
//!   edit does not invalidate readers of other keys;
//! * a set observes membership of one value, not the whole collection.
//!
//! Derived views are populated by component definitions instead of edits. A
//! derived map is defined one key at a time, which is what makes `Types[k]`
//! fine-grained. Derived lists and sets are defined as whole values, because a
//! producer that reconstructs a sequence has no occurrence identity to offer;
//! the plan calls that boundary out explicitly.

#![warn(missing_docs)]
#![forbid(unsafe_code)]

pub mod context;
pub mod field;
pub mod graph;
mod key_set;
pub use key_set::KeySet;
pub mod join;
pub mod list;
pub mod map;
pub mod reduce;
pub mod reductions;
pub mod set;
pub mod tree;

pub use context::Context;
pub use field::{
    DerivedListField, DerivedListWriter, DerivedSetField, DerivedSetWriter, Field, GraphField,
    GraphWriter, MapEntry, MapEntryWriter, MapField, MapWriter, ReducedField, ReducedWriter,
    ScalarField, ScalarWriter, SetMemberWriter, SourceListField, SourceSetField,
};
pub use graph::{EdgeKey, EdgePayload, Graph, GraphEditor, GraphRef, GraphTopology, NodeKey};
pub use join::{EquiJoin, FieldJoinState, Join, JoinSpec};
pub use list::{
    DerivedList, DerivedListRef, List, ListOutputExt, SourceList, SourceListEditor, SourceListRef,
};
pub use map::{Map, MapEditor, MapRef, MapStorage};
pub use reduce::{Reduced, ReducedEditor, ReducedRef, Reduction};
pub use reductions::{Count, Max, Min, Sum, Union};
pub use set::{DerivedSet, DerivedSetRef, Set, SourceSet, SourceSetEditor, SourceSetRef};
pub use tree::{
    BuildSlot, ChildSlot, TreeAccess, TreeBuild, TreeEditor, TreeImportReport, TreeNode,
    TreeNodeId, TreeOutputExt, TreeOutputTxn, TreePatch, TreePatchNode, TreeRef, TreeStorage,
    TreeValue, slots as tree_slots, tree_content_vtable, tree_eq, tree_fields, units as tree_units,
};
