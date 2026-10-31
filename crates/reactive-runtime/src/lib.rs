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
pub mod edit;
pub mod field;
pub mod helpers;
pub mod inner;
pub mod param;
pub mod runtime;
pub mod snapshot;
pub mod state;

pub use component::{ComponentSpec, RootComponent, call_component};
pub use edit::Edit;
pub use field::{ScalarEditor, ScalarRef, ViewFieldHandle};
pub use helpers::{define_unit, read_unit_value, view_content};
pub use inner::{DefinitionDraft, RuntimeConfig};
pub use param::Param;
pub use runtime::{Explain, NodeInfo, Runtime, RuntimeStats, Subscription};
pub use snapshot::{DerivedView, EditableView, Snapshot, SourceView, ViewSchema};
