//! Persistent, structurally shared collections and View schemas.
//!
//! Every collection here is immutable: an edit returns a new value that shares
//! almost all of its structure with the old one. That is what lets a snapshot
//! retain revision `r` while the runtime advances to `r + 1`, and what lets a
//! subscriber receive the old and new value without a copy.
//!
//! Element identity is always explicit and separate from position: sequences
//! store stable [`EntityId`]s, maps key by the user's key, and trees address
//! nodes by [`EntityId`]. Balancing nodes are private implementation detail and
//! never become identities.

#![warn(missing_docs)]
#![forbid(unsafe_code)]

pub mod aggregate;
pub mod delta;
pub mod field;
pub mod graph;
pub mod persistent;

pub use aggregate::AggSeq;
pub use delta::{
    AppliedDelta, DeltaStore, FieldStore, ListDelta, ListEdit, MapDelta, MapEdit, SetDelta, SetEdit,
};
pub use field::{Scalar, ViewField};
pub use graph::{Edge, EdgeId, GraphDelta, GraphEdit, GraphKey, GraphStore};
pub use persistent::{PMap, PSeq, PSet};
