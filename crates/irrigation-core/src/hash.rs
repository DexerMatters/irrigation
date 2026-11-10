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
mod tests {
    use super::*;

    #[test]
    fn hash_is_stable_and_sensitive() {
        assert_eq!(hash_one(&"abc"), hash_one(&"abc"));
        assert_ne!(hash_one(&"abc"), hash_one(&"abd"));
        assert_ne!(hash_one(&1u64), hash_one(&2u64));
    }

    #[test]
    fn byte_hashes_distinguish_prefixes() {
        assert_ne!(hash_one(&[1u8, 2, 3]), hash_one(&[1u8, 2, 3, 0]));
    }
}
