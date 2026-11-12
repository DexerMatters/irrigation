//! Type-erased keys for dynamically discovered units.
//!
//! Map entries, set members, and graph nodes are discovered at run time, so
//! their identities cannot be compile-time constants. [`AnyKey`] keeps the
//! concrete key type for equality while allowing the runtime to store units in
//! one table. A hash bucket always re-checks equality: hashes never establish
//! identity on their own.

use std::any::Any;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::hash::{FxHashMap, FxHasher};

/// A dynamically typed identity for one logical unit.
pub trait AnyKey: Any + Send + Sync + 'static {
    /// Returns the key as `dyn Any` for downcasting to the concrete type.
    fn as_any(&self) -> &dyn Any;
    /// Returns a stable hash used only for bucketing.
    fn key_hash(&self) -> u64;
    /// Compares against another erased key of the same concrete type.
    fn key_eq(&self, other: &dyn AnyKey) -> bool;
    /// Formats the key for diagnostics.
    fn key_display(&self) -> String;
}

impl<K> AnyKey for K
where
    K: Any + Hash + Eq + Send + Sync + std::fmt::Debug,
{
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn key_hash(&self) -> u64 {
        let mut hasher = FxHasher::default();
        self.hash(&mut hasher);
        hasher.finish()
    }

    fn key_eq(&self, other: &dyn AnyKey) -> bool {
        other
            .as_any()
            .downcast_ref::<K>()
            .is_some_and(|other| self == other)
    }

    fn key_display(&self) -> String {
        format!("{self:?}")
    }
}

/// Erases a concrete key.
#[must_use]
pub fn any_key<K: Any + Hash + Eq + Send + Sync + std::fmt::Debug>(key: K) -> Arc<dyn AnyKey> {
    Arc::new(key)
}

type Bucket<V> = Vec<(Arc<dyn AnyKey>, V)>;

/// A hash-bucketed table whose buckets verify equality on the real key.
///
/// This is the collision-safe form the plan requires: the hash chooses a
/// bucket, the key decides identity.
#[derive(Clone)]
pub struct KeyTable<V> {
    buckets: FxHashMap<u64, Bucket<V>>,
    len: usize,
}

impl<V> Default for KeyTable<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V> KeyTable<V> {
    /// Creates an empty table.
    #[must_use]
    pub fn new() -> Self {
        Self {
            buckets: FxHashMap::default(),
            len: 0,
        }
    }

    /// Number of stored entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` when no entry is stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Looks up an entry, verifying the concrete key.
    #[must_use]
    pub fn get(&self, key: &dyn AnyKey) -> Option<&V> {
        self.buckets
            .get(&key.key_hash())?
            .iter()
            .find(|(candidate, _)| candidate.key_eq(key))
            .map(|(_, value)| value)
    }

    /// Looks up an entry mutably, verifying the concrete key.
    pub fn get_mut(&mut self, key: &dyn AnyKey) -> Option<&mut V> {
        self.buckets
            .get_mut(&key.key_hash())?
            .iter_mut()
            .find(|(candidate, _)| candidate.key_eq(key))
            .map(|(_, value)| value)
    }

    /// Inserts an entry, returning the previous value when the key existed.
    pub fn insert(&mut self, key: Arc<dyn AnyKey>, value: V) -> Option<V> {
        let bucket = self.buckets.entry(key.key_hash()).or_default();
        if let Some((_, slot)) = bucket
            .iter_mut()
            .find(|(candidate, _)| candidate.key_eq(key.as_ref()))
        {
            return Some(std::mem::replace(slot, value));
        }
        bucket.push((key, value));
        self.len += 1;
        None
    }

    /// Removes an entry, returning it when present.
    pub fn remove(&mut self, key: &dyn AnyKey) -> Option<(Arc<dyn AnyKey>, V)> {
        let bucket = self.buckets.get_mut(&key.key_hash())?;
        let position = bucket
            .iter()
            .position(|(candidate, _)| candidate.key_eq(key))?;
        let entry = bucket.remove(position);
        self.len -= 1;
        if bucket.is_empty() {
            self.buckets.remove(&key.key_hash());
        }
        Some(entry)
    }

    /// Iterates over every stored entry.
    pub fn iter(&self) -> impl Iterator<Item = (&Arc<dyn AnyKey>, &V)> {
        self.buckets
            .values()
            .flat_map(|bucket| bucket.iter().map(|(k, v)| (k, v)))
    }

    /// Removes all entries.
    pub fn clear(&mut self) {
        self.buckets.clear();
        self.len = 0;
    }
}

impl<V> fmt::Debug for KeyTable<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyTable").field("len", &self.len).finish()
    }
}

impl<V: fmt::Debug> fmt::Display for KeyTable<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KeyTable({} entries)", self.len)
    }
}

#[cfg(test)]
#[path = "../tests/unit/key/tests.rs"]
mod tests;
