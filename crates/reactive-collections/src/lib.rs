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

pub mod graph;
pub mod join;
pub mod list;
pub mod map;
pub mod reduce;
pub mod set;
pub mod context;
pub mod tree;

pub use graph::{EdgeKey, EdgePayload, Graph, GraphEditor, GraphRef, GraphTopology, NodeKey};
pub use join::{EquiJoin, JoinSpec};
pub use list::{DerivedList, DerivedListRef, SourceList, SourceListEditor, SourceListRef};
pub use map::{Map, MapEditor, MapRef};
pub use reduce::{Reduced, ReducedEditor, ReducedRef, Reduction};
pub use set::{DerivedSet, DerivedSetRef, SourceSet, SourceSetEditor, SourceSetRef};
pub use context::Context;
pub use tree::{
    BuildSlot, ChildSlot, TreeAccess, TreeBuild, TreeEditor, TreeImportReport, TreeRef,
    TreeStorage, TreeNode, TreeNodeId, TreeValue, slots as tree_slots, tree_content_vtable,
    tree_eq, tree_fields,
};
