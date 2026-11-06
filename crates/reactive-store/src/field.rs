//! Field storage contracts used by generated View content types.
//!
//! A View's content is a plain struct of persistent values. Each declared field
//! type implements [`ViewField`], which tells the runtime how to create an
//! empty field and how to install or retract one logical unit inside it. The
//! macro-generated content type delegates to these operations, so the runtime
//! never needs to know whether a field is a map, a list, or a scalar.

use std::marker::PhantomData;

use reactive_core::key::AnyKey;
use reactive_core::value::Value;

use reactive_core::descriptor::FieldKind;

/// One field of a View's content.
pub trait ViewField: 'static {
    /// The persistent storage type for this field.
    type Storage: Clone + Send + Sync + 'static;

    /// The field shape reported in the View descriptor.
    const KIND: FieldKind;

    /// The empty field.
    fn empty() -> Self::Storage;

    /// Installs `value` under `key`, returning the new storage.
    ///
    /// Scalar fields ignore the key: they have exactly one logical unit.
    fn define(storage: &Self::Storage, key: &dyn AnyKey, value: Value) -> Self::Storage;

    /// Retracts `key`, returning the new storage.
    fn retract(storage: &Self::Storage, key: &dyn AnyKey) -> Self::Storage;

    /// Reports whether `key` is currently defined.
    fn contains(storage: &Self::Storage, key: &dyn AnyKey) -> bool;

    /// Reads one value out of the storage, if present.
    fn get(storage: &Self::Storage, key: &dyn AnyKey) -> Option<Value>;

    /// Projects this field's structure (a map's key set, a list's element-id
    /// sequence, a set's members), when the family has one.
    fn structure(_storage: &Self::Storage) -> Option<reactive_core::descriptor::Structure> {
        None
    }

    /// Number of display elements, when the family can answer cheaply.
    ///
    /// This is a count of what a person would see (tokens, map keys, tree
    /// nodes), not a count of logical units. Scalar fields have no elements.
    fn len(_storage: &Self::Storage) -> Option<usize> {
        None
    }
}

/// A single-valued field.
///
/// The unit key for a scalar is the unit struct `()`, which keeps generated
/// code uniform with collection fields.
#[derive(Debug)]
pub struct Scalar<T>(PhantomData<fn() -> T>);

impl<T> Default for Scalar<T> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<T> ViewField for Scalar<T>
where
    T: Clone + Default + PartialEq + Send + Sync + 'static,
{
    type Storage = T;

    const KIND: FieldKind = FieldKind::Scalar;

    fn empty() -> Self::Storage {
        T::default()
    }

    fn define(_storage: &Self::Storage, _key: &dyn AnyKey, value: Value) -> Self::Storage {
        value.downcast_ref::<T>().cloned().unwrap_or_default()
    }

    fn retract(storage: &Self::Storage, _key: &dyn AnyKey) -> Self::Storage {
        storage.clone()
    }

    fn contains(_storage: &Self::Storage, _key: &dyn AnyKey) -> bool {
        true
    }

    fn get(storage: &Self::Storage, _key: &dyn AnyKey) -> Option<Value> {
        Some(reactive_core::value::value_of(storage.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_field_round_trips_values() {
        let empty = <Scalar<u32> as ViewField>::empty();
        assert_eq!(empty, 0);
        let defined = <Scalar<u32> as ViewField>::define(&empty, &(), value_of(7u32));
        assert_eq!(defined, 7);
        assert_eq!(
            <Scalar<u32> as ViewField>::get(&defined, &())
                .and_then(|value| value.downcast_ref::<u32>().copied()),
            Some(7)
        );
    }

    use reactive_core::value::value_of;
}
