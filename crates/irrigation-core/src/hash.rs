//! Fast non-cryptographic hashing used for internal indexes.
//!
//! Hashes never establish semantic equality: every keyed table resolves
//! collisions by comparing the real key. Fx hashing is supplied by
//! `rustc-hash`.

use std::hash::{Hash, Hasher};

pub use rustc_hash::{FxBuildHasher, FxHashMap, FxHashSet, FxHasher};

/// Hashes one value with [`FxHasher`].
#[must_use]
pub fn hash_one<T: Hash + ?Sized>(value: &T) -> u64 {
    let mut hasher = FxHasher::default();
    value.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
#[path = "../tests/unit/hash/tests.rs"]
mod tests;
