//! Tracked context arguments.
//!
//! A recursive component called with a whole Rust value rekeys every
//! descendant when an unrelated field changes. A [`Context`] separates the
//! invocation identity (the key) from the observed value (a logical unit): the
//! invocation is reused while the key is unchanged, and the value change is
//! observed through a tracked unit read instead of a new argument identity.

use std::hash::{Hash, Hasher};
use std::marker::PhantomData;

use irrigation_core::descriptor::UnitKey;

use irrigation_runtime::helpers::read_unit_value;

/// A context argument whose invocation identity is its key.
///
/// The value lives in `unit`; [`Context::read`] performs a tracked read, so a
/// value change re-executes the invocation body without rekeying it.
#[derive(Clone, Debug)]
pub struct Context<K, V> {
    key: K,
    unit: UnitKey,
    marker: PhantomData<fn() -> V>,
}

impl<K, V> Context<K, V> {
    /// Builds a context argument for `key`, reading its value from `unit`.
    #[must_use]
    pub fn new(key: K, unit: UnitKey) -> Self {
        Self {
            key,
            unit,
            marker: PhantomData,
        }
    }

    /// The identity key.
    #[must_use]
    pub fn key(&self) -> &K {
        &self.key
    }

    /// The unit holding the context value.
    #[must_use]
    pub fn unit(&self) -> &UnitKey {
        &self.unit
    }
}

impl<K, V> Context<K, V>
where
    V: Clone + PartialEq + Send + Sync + 'static,
{
    /// Reads the context value, recording the observation.
    ///
    /// # Panics
    ///
    /// Panics outside a computation or when the unit was never defined.
    #[must_use]
    pub fn read(&self) -> V {
        read_unit_value(self.unit.clone(), |_content| None)
    }
}

impl<K: PartialEq, V> PartialEq for Context<K, V> {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl<K: Eq, V> Eq for Context<K, V> {}

impl<K: Hash, V> Hash for Context<K, V> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key.hash(state);
    }
}
