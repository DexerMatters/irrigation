//! The incremental runtime: revisions, demand, validation, and publication.
//!
//! # Contract
//!
//! Within one [`Runtime`]:
//!
//! * every declared View type identifies one logical View instance;
//! * every declared component type identifies one registered generator;
//! * the observable result at revision `r` equals a deterministic from-scratch
//!   evaluation at revision `r`;
//! * unchanged output units keep their change version and do not notify
//!   consumers.

#![warn(missing_docs)]
#![forbid(unsafe_code)]

pub mod component;
pub mod demand;
pub mod edit;
pub mod field;
pub mod fixed_point;
pub mod gate;
pub mod general;
pub mod helpers;
pub mod inner;
#[cfg(feature = "observe")]
pub mod observe;
pub mod param;
pub mod runtime;
pub mod scheduler;
pub mod snapshot;
pub mod state;
pub mod task;

pub use component::{ComponentSpec, RootComponent, call_component};
pub use edit::Edit;
pub use field::{ScalarEditor, ScalarRef, ViewFieldHandle};
pub use fixed_point::{
    FixedPoint, FixedPointProblem, Lattice, solve, solve_from, solve_from_with_budget,
    solve_with_budget,
};
pub use general::{
    AsyncAdapter, AsyncGeneralComponent, Change, ChangeBatch, ComponentFuture, DependencyUpdates,
    GeneralComponent, GeneralCx, GeneralHandle, GeneralInputs, LocalAdapter, LocalGeneralComponent,
    Request, TypedAdapter, TypedGeneralComponent, TypedRequest, drive,
};
pub use helpers::{
    WriteGroup, claim_field, claim_ports, claim_unit, define_unit, plan_ports, provide_ports,
    read_unit_value, require_condition, require_value, require_value_await, retract_unit,
    try_read_unit_value, view_content, wait_for_port,
};
pub use inner::{DefinitionDraft, MemoryReport, ReclaimStats, RuntimeConfig, WriteArbitration};
pub use param::Param;
pub use runtime::{
    DependencyExplanation, Explain, Explanation, IdentityReport, Installed, InstalledGeneral,
    InstalledProducer, NodeInfo, PortProvider, RecipeSelector, RouteSelector, RoutingRecipe,
    RoutingStamp, Runtime, RuntimeStats, Subscription, TaskCounts,
};
pub use snapshot::{
    ChangeSummary, DerivedView, EditableView, IncrementalData, Snapshot, SourceView, ViewSchema,
};

#[cfg(feature = "observe")]
pub use observe::{DemandCause, ObserveLevel, RuntimeEvent, RuntimeObserver, error_kind};
