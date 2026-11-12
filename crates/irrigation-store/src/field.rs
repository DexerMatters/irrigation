//! Field storage contracts used by generated View content types.
//!
//! A View's content is a plain struct of persistent values. Each declared field
//! type implements [`ViewField`], which tells the runtime how to create an
//! empty field and how to install or retract one logical unit inside it. The
//! macro-generated content type delegates to these operations, so the runtime
//! never needs to know whether a field is a map, a list, or a scalar.

use std::marker::PhantomData;

use irrigation_core::key::AnyKey;
use irrigation_core::value::Value;

use irrigation_core::descriptor::FieldKind;

/// One field of a View's content.
pub trait ViewField: 'static {
    /// The persistent storage type for this field.
    type Storage: Clone + Send + Sync + 'static;

    /// The field shape reported in the View descriptor.
    const KIND: FieldKind;

    /// The empty field.
    fn empty() -> Self::Storage;

    /// Whether two storage values of this field hold the same answer.
    ///
    /// The default is `false`, which is the safe direction: a caller that uses it
    /// to skip publishing an equal value falls back to publishing, so a family
    /// that does not implement this behaves exactly as before. Families whose
    /// storage is comparable override it.
    fn equal_storage(_left: &Value, _right: &Value) -> bool {
        false
    }

    /// Installs `value` under `key`, returning the new storage.
    ///
    /// Scalar fields ignore the key: they have exactly one logical unit.
    fn define(storage: &Self::Storage, key: &dyn AnyKey, value: Value) -> Self::Storage;

    /// Installs a batch of logical units, returning the new storage.
    ///
    /// The default is the same single-unit operation repeated. Families that
    /// maintain an index can override it to apply the batch once.
    fn define_many(
        storage: &Self::Storage,
        drafts: &[(irrigation_core::descriptor::UnitKey, Value)],
    ) -> Self::Storage {
        let mut next = storage.clone();
        for (key, value) in drafts {
            let entity = key.entity.as_deref().unwrap_or(&());
            if key.kind == irrigation_core::descriptor::UnitKind::Membership {
                // A bare presence statement is not a payload draft. `false`
                // removes the entity; `true` does nothing by default. Sets
                // override this because their content spells a member as the
                // entity itself.
                if value.downcast_ref::<bool>() == Some(&false) {
                    next = Self::retract(&next, entity);
                }
            } else {
                next = Self::define(&next, entity, value.clone());
            }
        }
        next
    }

    /// Retracts `key`, returning the new storage.
    fn retract(storage: &Self::Storage, key: &dyn AnyKey) -> Self::Storage;

    /// Reports whether `key` is currently defined.
    fn contains(storage: &Self::Storage, key: &dyn AnyKey) -> bool;

    /// Reads one value out of the storage, if present.
    fn get(storage: &Self::Storage, key: &dyn AnyKey) -> Option<Value>;

    /// Projects this field's structure (a map's key set, a list's element-id
    /// sequence, a set's members), when the family has one.
    fn structure(_storage: &Self::Storage) -> Option<irrigation_core::descriptor::Structure> {
        None
    }

    /// Number of display elements, when the family can answer cheaply.
    ///
    /// This is a count of what a person would see (tokens, map keys, tree
    /// nodes), not a count of logical units. Scalar fields have no elements.
    fn len(_storage: &Self::Storage) -> Option<usize> {
        None
    }

    /// A whole-field summary value, when the family has one.
    ///
    /// A reduction publishes its cached aggregate here so a projection can show
    /// it without a host renderer. `None` means the field has no summary beyond
    /// its rows.
    fn summary(_storage: &Self::Storage) -> Option<Value> {
        None
    }

    /// Compares one logical unit between two field storages.
    ///
    /// `None` means the family cannot answer without the whole-value fallback.
    fn unit_eq(
        _old: &Self::Storage,
        _new: &Self::Storage,
        _unit: &irrigation_core::descriptor::UnitKey,
    ) -> Option<bool> {
        None
    }

    /// Whether installing this unit changes the field's content value.
    ///
    /// Membership, structure, rank and adjacency units can move without
    /// changing the content they are projections of. Families that know this
    /// override the default; the runtime can then skip content work for units
    /// that only move a logical projection.
    fn unit_moves_content(_unit: &irrigation_core::descriptor::UnitKey) -> bool {
        true
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

    /// A scalar has exactly one value, so its storage comparison is the value's.
    fn equal_storage(left: &Value, right: &Value) -> bool {
        match (left.downcast_ref::<T>(), right.downcast_ref::<T>()) {
            (Some(left), Some(right)) => left == right,
            _ => false,
        }
    }

    fn define(storage: &Self::Storage, _key: &dyn AnyKey, value: Value) -> Self::Storage {
        // A mismatched value leaves the storage alone, like every other family:
        // the read side reports the type error.
        let Some(value) = value.downcast_ref::<T>() else {
            return storage.clone();
        };
        value.clone()
    }

    fn retract(_storage: &Self::Storage, _key: &dyn AnyKey) -> Self::Storage {
        // The scalar's only unit is the whole field.
        T::default()
    }

    fn contains(_storage: &Self::Storage, _key: &dyn AnyKey) -> bool {
        true
    }

    fn get(storage: &Self::Storage, _key: &dyn AnyKey) -> Option<Value> {
        Some(irrigation_core::value::value_of(storage.clone()))
    }

    fn unit_eq(
        old: &Self::Storage,
        new: &Self::Storage,
        unit: &irrigation_core::descriptor::UnitKey,
    ) -> Option<bool> {
        (unit.entity.is_none()
            && matches!(
                unit.kind,
                irrigation_core::descriptor::UnitKind::Value
                    | irrigation_core::descriptor::UnitKind::Structure
            ))
        .then(|| old == new)
    }
}

#[cfg(test)]
#[path = "../tests/unit/field/tests.rs"]
mod tests;
