//! Type-erased immutable values stored by computation nodes.

use std::any::Any;
use std::sync::Arc;

/// A shared, immutable node value.
///
/// Values are `Arc`-shared so publishing a new revision never copies payloads.
/// Concrete types are recovered with [`value_downcast`] or by downcasting the
/// `Arc` directly; every read site knows the type it stored.
pub type Value = Arc<dyn Any + Send + Sync>;

/// Wraps a concrete value for storage in a node.
#[must_use]
pub fn value_of<T: Any + Send + Sync>(value: T) -> Value {
    Arc::new(value)
}

/// Borrows the concrete value if it has type `T`.
#[must_use]
pub fn value_downcast<T: Any + Send + Sync>(value: &Value) -> Option<&T> {
    value.downcast_ref::<T>()
}

/// Clones the concrete value if it has type `T`.
#[must_use]
pub fn value_clone<T: Any + Clone + Send + Sync>(value: &Value) -> Option<T> {
    value.downcast_ref::<T>().cloned()
}

/// Returns the number of strong references to the payload.
///
/// Used by tests and diagnostics to show that unchanged values are shared
/// rather than copied.
#[must_use]
pub fn value_strong_count(value: &Value) -> usize {
    Arc::strong_count(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_concrete_values() {
        let value = value_of(41u32);
        assert_eq!(value_downcast::<u32>(&value), Some(&41));
        assert_eq!(value_clone::<u32>(&value), Some(41));
        assert_eq!(value_downcast::<u64>(&value), None);
    }

    #[test]
    fn cloning_a_value_shares_the_payload() {
        let value = value_of(String::from("shared"));
        let clone = Arc::clone(&value);
        assert_eq!(value_strong_count(&value), 2);
        drop(clone);
        assert_eq!(value_strong_count(&value), 1);
    }
}
