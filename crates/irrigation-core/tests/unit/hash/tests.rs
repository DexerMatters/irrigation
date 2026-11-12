//! Unit tests for `src/hash.rs`.

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
