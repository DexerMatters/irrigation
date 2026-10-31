//! Fast non-cryptographic hashing used for internal indexes.
//!
//! Hashes never establish semantic equality: every keyed table resolves
//! collisions by comparing the real key.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hash, Hasher};

const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

/// An FxHash-style hasher: cheap, deterministic, and good enough for indexes.
#[derive(Default, Clone, Copy, Debug)]
pub struct FxHasher {
    hash: u64,
}

impl FxHasher {
    #[inline]
    fn add(&mut self, word: u64) {
        self.hash = (self.hash.rotate_left(5) ^ word).wrapping_mul(SEED);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for chunk in &mut chunks {
            let mut word = [0u8; 8];
            word.copy_from_slice(chunk);
            self.add(u64::from_le_bytes(word));
        }
        let rest = chunks.remainder();
        if !rest.is_empty() {
            let mut word = [0u8; 8];
            word[..rest.len()].copy_from_slice(rest);
            self.add(u64::from_le_bytes(word));
        }
    }

    #[inline]
    fn write_u8(&mut self, value: u8) {
        self.add(u64::from(value));
    }

    #[inline]
    fn write_u16(&mut self, value: u16) {
        self.add(u64::from(value));
    }

    #[inline]
    fn write_u32(&mut self, value: u32) {
        self.add(u64::from(value));
    }

    #[inline]
    fn write_u64(&mut self, value: u64) {
        self.add(value);
    }

    #[inline]
    fn write_usize(&mut self, value: usize) {
        self.add(value as u64);
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }
}

/// `HashMap` using [`FxHasher`].
pub type FxHashMap<K, V> = HashMap<K, V, BuildHasherDefault<FxHasher>>;

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
