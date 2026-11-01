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
pub mod gate;
pub mod fixed_point;
pub mod general;
pub mod helpers;
pub mod inner;
pub mod param;
pub mod runtime;
pub mod scheduler;
pub mod snapshot;
pub mod state;
pub mod task;

pub use component::{ComponentSpec, RootComponent, call_component};
pub use edit::Edit;
pub use field::{ScalarEditor, ScalarRef, ViewFieldHandle};
pub use fixed_point::{FixedPoint, FixedPointProblem, Lattice, solve, solve_with_budget};
pub use general::{
    AsyncAdapter, AsyncGeneralComponent, Change, ChangeBatch, ComponentFuture, DependencyUpdates,
    GeneralComponent, GeneralCx, GeneralHandle, GeneralInputs, Request, TypedAdapter,
    TypedGeneralComponent, TypedRequest, drive,
};
pub use helpers::{define_unit, read_unit_value, try_read_unit_value, view_content};
pub use inner::{DefinitionDraft, MemoryReport, ReclaimStats, RuntimeConfig};
pub use param::Param;
pub use runtime::{
    DependencyExplanation, Explain, Explanation, NodeInfo, Runtime, RuntimeStats, Subscription,
    TaskCounts,
};
pub use snapshot::{
    ChangeSummary, DerivedView, EditableView, Snapshot, SourceView, ViewSchema,
};
