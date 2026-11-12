//! The public facade for the irrigation incremental system.
//!
//! User code imports this crate and the macros. Everything below the facade —
//! the node kernel, persistent storage, collection operators, and macro
//! lowering — is an implementation detail that generated code also targets.
//!
//! # Identity
//!
//! Identity is what survives an edit. The framework allocates it where the thing
//! happens, never from the data a component happens to hold, so no host invents
//! an id:
//!
//! * a token is the occurrence at a position, and keeps its id when an edit
//!   re-spells it; an AST node is the extent it covers over those tokens;
//! * a component invocation **is** its argument tuple (`(component, arguments)`);
//! * an entity emitted from a component body is the expression site that emitted
//!   it ([`emission_id`]), stable across re-executions of the invocation and
//!   retired with it;
//! * a keyed collection entry is its key (`define_keyed`, [`Map`], [`Set`]);
//! * a shared node is its content ([`irrigation_core::ops::shared_node`] /
//!   `emit_shared`).
//!
//! The consequence is the rule every component author needs:
//!
//! **arguments are identity.** A component keeps the identities it emitted
//! exactly while its arguments are unchanged. Pass occurrences — an AST node
//! reference, a [`TreeNodeId`], a `u64` that names a source entity, a View
//! handle — never a spelling or a value that is merely
//! data, unless that value *is* what the invocation computes. Data that is not
//! identity belongs in a read (a View dependency) or in the emitted payload,
//! not in an argument.
//!
//! The three authoring styles follow from that:
//!
//! * a root `#[component]` has no arguments, so its sites are stable;
//! * a callable `#[component]` gets [`emission_id`] identities, stable whenever
//!   its arguments are;
//! * a `GeneralComponent` producer writes View content directly and therefore
//!   owns its identities: key them by the source occurrence (`TreeNodeId`,
//!   `TokenId`, `AstNodeRef`), or publish a whole value when the elements have
//!   no identity of their own. `ChangeBatch` names exactly the units that moved.
//!
//! Content-addressed identity is right when the content *is* the name — a
//! canonical type node, a JSON path — and wrong when the content is only a
//! spelling: keying by a name re-creates the entity whenever the name changes.

#![warn(missing_docs)]
#![forbid(unsafe_code)]

extern crate self as irrigation;

pub use irrigation_collections::{
    BuildSlot, ChildSlot, Context, DerivedList, DerivedListField, DerivedListRef,
    DerivedListWriter, DerivedSet, DerivedSetField, DerivedSetRef, DerivedSetWriter, EdgeKey,
    EdgePayload, EquiJoin, Field, Graph, GraphEditor, GraphField, GraphRef, GraphTopology,
    GraphWriter, Join, JoinSpec, KeySet, List, ListOutputExt, Map, MapEditor, MapEntry,
    MapEntryWriter, MapField, MapRef, MapStorage, MapWriter, NodeKey, Reduced, ReducedEditor,
    ReducedField, ReducedRef, Reduction, ScalarField, ScalarWriter, Set, SetMemberWriter,
    SourceList, SourceListEditor, SourceListField, SourceListRef, SourceSet, SourceSetEditor,
    SourceSetField, SourceSetRef, TreeAccess, TreeBuild, TreeEditor, TreeImportReport, TreeNode,
    TreeNodeId, TreeOutputExt, TreeOutputTxn, TreePatch, TreePatchNode, TreeRef, TreeStorage,
    TreeValue, tree_content_vtable, tree_eq, tree_fields, tree_slots, tree_units,
};
pub use irrigation_collections::{
    Cardinality, ChildLayout, LeafKind, LeafLayout, TreeLayout, TreeSchema, TypeLayout,
    VariantLayout,
};
pub use irrigation_collections::{Count, Max, Min, Sum, Union};
pub use irrigation_core::change::Delta;
pub use irrigation_core::descriptor::{
    ContentVtable, FieldDescriptor, FieldKind, Structure, UnitKey, UnitKind, ViewDescriptor,
    ViewKind, WriteScope,
};
pub use irrigation_core::error::{ComputeError, Result};
pub use irrigation_core::frame::{CancelToken, Dep};
pub use irrigation_core::hash::hash_one;
pub use irrigation_core::ids::{
    ComponentId, FieldId, NodeId, Revision, ScopeId, SiteId, Version, ViewId, const_hash,
};
pub use irrigation_core::key::{AnyKey, any_key};
pub use irrigation_core::ops::{emission_id, eval, scope};
pub use irrigation_core::value::{Value, value_of};
pub use irrigation_macros::{
    component, general_component, incremental_data, irrigation, tree_view, view,
};
pub use irrigation_runtime::Param;
pub use irrigation_runtime::field::{
    EntityKey, ScalarEditor, ScalarRef, ViewFieldHandle, membership_unit, structure_unit,
    value_unit,
};
pub use irrigation_runtime::fixed_point::{
    FixedPoint, FixedPointProblem, Lattice, solve as solve_fixed_point, solve_from,
    solve_from_with_budget,
};
pub use irrigation_runtime::general::{
    AsyncAdapter, AsyncGeneralComponent, Change, ChangeBatch, ComponentFuture, DependencyUpdates,
    GeneralComponent, GeneralCx, GeneralHandle, GeneralInputs, LocalAdapter, LocalGeneralComponent,
    Request, TypedAdapter, TypedGeneralComponent, TypedRequest, drive as drive_future,
};
pub use irrigation_runtime::helpers::{
    WriteGroup, claim_field, claim_ports, claim_unit, define_unit, ensure_view, node_of,
    observe_clone, observe_local, observe_ref, plan_ports, provide_ports, read_unit_value,
    require_absent, require_condition, require_value, retract_unit, try_read_unit_value,
    values_equal, view_content, wait_for_port,
};
pub use irrigation_runtime::{
    ChangeSummary, ComponentSpec, DependencyExplanation, DerivedView, Edit, EditableView, Explain,
    Explanation, IdentityReport, IncrementalData, Installed, InstalledGeneral, InstalledProducer,
    MemoryReport, NodeInfo, PortProvider, RecipeSelector, ReclaimStats, RootComponent,
    RouteSelector, RoutingRecipe, RoutingStamp, Runtime, RuntimeConfig, RuntimeStats, Snapshot,
    SourceView, Subscription, TaskCounts, ViewSchema, WriteArbitration, call_component,
};
pub use irrigation_store::AggSeq;
pub use irrigation_store::delta::{
    AppliedDelta, DeltaStore, FieldStore, ListDelta, ListEdit, MapDelta, MapEdit, SetDelta, SetEdit,
};
pub use irrigation_store::field::{Scalar, ViewField};
pub use irrigation_store::graph::GraphKey;
pub use irrigation_store::persistent::{PMap, PSeq, PSet};

/// The authoring prelude.
pub mod prelude {
    pub use crate::{
        ChangeBatch, ComputeError, DependencyUpdates, Edit, EditableView, GeneralComponent,
        GeneralCx, Result, Runtime, RuntimeConfig, Snapshot, SourceView, Subscription, ViewSchema,
    };
    pub use crate::{
        DerivedList, DerivedListField, DerivedListRef, DerivedSet, DerivedSetField, DerivedSetRef,
        Field, Graph, GraphField, GraphRef, List, Map, MapField, MapRef, ReducedField, ScalarField,
        Set, SourceList, SourceListField, SourceListRef, SourceSet, SourceSetField, SourceSetRef,
    };
    pub use crate::{component, general_component, tree_view, view};
}
