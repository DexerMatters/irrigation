//! Core vocabulary for the reactive incremental runtime.
//!
//! This crate owns the pieces that every other crate agrees on:
//!
//! * opaque identities ([`NodeId`], [`ScopeId`], [`SiteId`], ...);
//! * the structured error type ([`ComputeError`]);
//! * type-erased values ([`Value`]);
//! * the ambient computation frame that records observations ([`frame`]);
//! * the narrow interface the runtime exposes to generated code ([`RuntimeOps`]);
//! * change contracts for view deltas ([`change`]).
//!
//! It deliberately contains no scheduling, storage, or macro logic.

#![warn(missing_docs)]
#![forbid(unsafe_code)]

pub mod change;
pub mod descriptor;
pub mod error;
pub mod frame;
pub mod hash;
pub mod ids;
pub mod key;
pub mod ops;
pub mod value;

pub use error::{ComputeError, Result};
pub use frame::{CancelToken, Dep, Frame, FrameGuard};
pub use ids::{
    ComponentId, FieldId, NodeId, Revision, ScopeId, SiteId, Version, ViewId, const_hash,
};
pub use key::{AnyKey, KeyTable, any_key};
pub use ops::{RuntimeOps, eval, read_unit, scope, stage};
pub use value::{Value, value_downcast, value_of};
