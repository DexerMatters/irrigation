//! The public facade for the reactive incremental system.
//!
//! User code imports this crate and the macros. Everything below the facade —
//! the node kernel, persistent storage, collection operators, and macro
//! lowering — is an implementation detail that generated code also targets.

#![warn(missing_docs)]
#![forbid(unsafe_code)]

extern crate self as reactive;

pub use reactive_collections::{
    DerivedList, DerivedListRef, DerivedSet, DerivedSetRef, Map, MapEditor, MapRef, SourceList,
    SourceListEditor, SourceListRef, SourceSet, SourceSetEditor, SourceSetRef,
};
pub use reactive_collections::{EdgeKey, EdgePayload, Graph, GraphEditor, GraphRef, GraphTopology, NodeKey};
pub use reactive_core::error::{ComputeError, Result};
pub use reactive_core::frame::{CancelToken, Dep};
pub use reactive_core::hash::hash_one;
pub use reactive_core::ids::{
    ComponentId, FieldId, NodeId, Revision, ScopeId, SiteId, Version, ViewId, const_hash,
};
pub use reactive_core::key::{AnyKey, any_key};
pub use reactive_core::ops::eval;
pub use reactive_core::value::{Value, value_of};
pub use reactive_macros::{component, reactive, tree_view, view};
pub use reactive_runtime::field::{
    EntityKey, ScalarEditor, ScalarRef, ViewFieldHandle, membership_unit, structure_unit, value_unit,
};
pub use reactive_runtime::helpers::{
    ensure_view, node_of, observe_clone, observe_local, observe_ref, values_equal,
};
pub use reactive_runtime::Param;
pub use reactive_runtime::fixed_point::{
    FixedPoint, FixedPointProblem, Lattice, solve as solve_fixed_point,
};
pub use reactive_runtime::{
    ComponentSpec, DerivedView, Edit, EditableView, Explain, RootComponent, Runtime, RuntimeConfig,
    MemoryReport, ReclaimStats, RuntimeStats, Snapshot, SourceView, Subscription, ViewSchema, call_component, define_unit,
    read_unit_value, view_content,
};
pub use reactive_core::descriptor::{
    ContentVtable, FieldDescriptor, FieldKind, UnitKey, UnitKind, ViewDescriptor, ViewKind,
};
pub use reactive_store::field::{Scalar, ViewField};
pub use reactive_store::persistent::{PMap, PSeq, PSet};

/// Builds the zero-sized handle for a View type.
#[must_use]
pub fn view_handle<V: ViewSchema + Default>() -> V {
    V::default()
}
