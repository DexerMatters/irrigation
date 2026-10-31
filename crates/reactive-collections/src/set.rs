//! Incremental sets.
//!
//! Membership is observed per value: `set.contains(x) == false` subscribes to
//! that value's membership cell, so inserting `x` wakes exactly the readers
//! that asked about it.

use std::any::Any;
use std::hash::Hash;
use std::marker::PhantomData;

use reactive_core::ids::{FieldId, ViewId};
use reactive_core::key::any_key;
use reactive_core::value::{Value, value_of};
use reactive_runtime::edit::Edit;
use reactive_runtime::field::{ViewFieldHandle, structure_unit};
use reactive_runtime::helpers::{define_unit, read_unit_value};
use reactive_core::descriptor::{FieldKind, UnitKey, UnitKind};
use reactive_store::field::ViewField;
use reactive_store::persistent::PSet;

/// A source set field with per-member membership observations.
pub struct SourceSet<T>(PhantomData<fn() -> T>);

impl<T> std::fmt::Debug for SourceSet<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SourceSet")
    }
}

impl<T> ViewField for SourceSet<T>
where
    T: Ord + Hash + Clone + Send + Sync + std::fmt::Debug + 'static,
{
    type Storage = PSet<T>;

    const KIND: FieldKind = FieldKind::Set;

    fn empty() -> Self::Storage {
        PSet::new()
    }

    fn define(
        storage: &Self::Storage,
        key: &dyn reactive_core::key::AnyKey,
        _value: Value,
    ) -> Self::Storage {
        match key.as_any().downcast_ref::<T>() {
            Some(value) => storage.insert(value.clone()).0,
            None => storage.clone(),
        }
    }

    fn retract(storage: &Self::Storage, key: &dyn reactive_core::key::AnyKey) -> Self::Storage {
        match key.as_any().downcast_ref::<T>() {
            Some(value) => storage.remove(value).0,
            None => storage.clone(),
        }
    }

    fn contains(storage: &Self::Storage, key: &dyn reactive_core::key::AnyKey) -> bool {
        key.as_any()
            .downcast_ref::<T>()
            .is_some_and(|value| storage.contains(value))
    }

    fn get(storage: &Self::Storage, _key: &dyn reactive_core::key::AnyKey) -> Option<Value> {
        Some(value_of(storage.clone()))
    }
}

/// Tracked read handle for a source set.
#[derive(Clone)]
pub struct SourceSetRef<T> {
    view: ViewId,
    field: FieldId,
    content: fn(&dyn Any) -> PSet<T>,
}

impl<T> PartialEq for SourceSetRef<T> {
    fn eq(&self, other: &Self) -> bool {
        self.view == other.view && self.field == other.field
    }
}

impl<T> Eq for SourceSetRef<T> {}

impl<T> std::fmt::Debug for SourceSetRef<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceSetRef")
            .field("view", &self.view)
            .field("field", &self.field)
            .finish_non_exhaustive()
    }
}

impl<T> SourceSetRef<T>
where
    T: Ord + Hash + Clone + Send + Sync + std::fmt::Debug + 'static,
{
    /// Builds a handle for one set field.
    #[must_use]
    pub fn new(view: ViewId, field: FieldId, content: fn(&dyn Any) -> PSet<T>) -> Self {
        Self {
            view,
            field,
            content,
        }
    }

    /// Reports membership, observing that value's cell.
    #[must_use]
    pub fn contains(&self, value: &T) -> bool {
        read_unit_value(
            UnitKey::entity(
                self.view,
                self.field,
                UnitKind::Membership,
                any_key(value.clone()),
            ),
            |content| Some((self.content)(content).contains(value)),
        )
    }

    /// Number of members.
    #[must_use]
    pub fn len(&self) -> usize {
        self.to_vec().len()
    }

    /// Returns `true` when the set has no members.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Reads every member in order.
    #[must_use]
    pub fn to_vec(&self) -> Vec<T> {
        let structure: PSet<T> = read_unit_value(structure_unit(self.view, self.field), |content| {
            Some((self.content)(content))
        });
        structure.iter().cloned().collect()
    }
}

impl<T> ViewFieldHandle for SourceSet<T>
where
    T: Ord + Hash + Clone + Send + Sync + std::fmt::Debug + 'static,
{
    type Handle = SourceSetRef<T>;
    type Editor<'a> = SourceSetEditor<'a, T>;

    fn handle(
        view: ViewId,
        field: FieldId,
        content: fn(&dyn Any) -> Self::Storage,
    ) -> Self::Handle {
        SourceSetRef::new(view, field, content)
    }

    fn editor<'a>(
        edit: &'a mut Edit,
        view: ViewId,
        field: FieldId,
        storage: &'a mut Self::Storage,
    ) -> Self::Editor<'a> {
        SourceSetEditor {
            edit,
            view,
            field,
            storage,
        }
    }
}

/// Transactional editor for a source set.
pub struct SourceSetEditor<'a, T> {
    edit: &'a mut Edit,
    view: ViewId,
    field: FieldId,
    storage: &'a mut PSet<T>,
}

impl<T> std::fmt::Debug for SourceSetEditor<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceSetEditor").finish_non_exhaustive()
    }
}

impl<T> SourceSetEditor<'_, T>
where
    T: Ord + Hash + Clone + Send + Sync + std::fmt::Debug + 'static,
{
    /// Inserts a member, reporting whether it was new.
    pub fn insert(&mut self, value: T) -> bool {
        let (next, inserted) = self.storage.insert(value.clone());
        if !inserted {
            return false;
        }
        *self.storage = next;
        self.edit.set(
            UnitKey::entity(
                self.view,
                self.field,
                UnitKind::Membership,
                any_key(value),
            ),
            true,
        );
        self.stage_structure();
        true
    }

    /// Removes a member, reporting whether it was present.
    pub fn remove(&mut self, value: &T) -> bool {
        let (next, removed) = self.storage.remove(value);
        if !removed {
            return false;
        }
        *self.storage = next;
        self.edit.set(
            UnitKey::entity(
                self.view,
                self.field,
                UnitKind::Membership,
                any_key(value.clone()),
            ),
            false,
        );
        self.stage_structure();
        true
    }

    fn stage_structure(&mut self) {
        let structure = self.storage.clone();
        self.edit
            .set(structure_unit(self.view, self.field), structure);
    }
}

/// A derived set field defined as one whole value.
pub struct DerivedSet<T>(PhantomData<fn() -> T>);

impl<T> std::fmt::Debug for DerivedSet<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DerivedSet")
    }
}

impl<T> ViewField for DerivedSet<T>
where
    T: Ord + Hash + Clone + Send + Sync + std::fmt::Debug + 'static,
{
    type Storage = PSet<T>;

    const KIND: FieldKind = FieldKind::Set;

    fn empty() -> Self::Storage {
        PSet::new()
    }

    fn define(
        storage: &Self::Storage,
        _key: &dyn reactive_core::key::AnyKey,
        value: Value,
    ) -> Self::Storage {
        value
            .downcast_ref::<PSet<T>>()
            .cloned()
            .unwrap_or_else(|| storage.clone())
    }

    fn retract(storage: &Self::Storage, _key: &dyn reactive_core::key::AnyKey) -> Self::Storage {
        storage.clone()
    }

    fn contains(_storage: &Self::Storage, _key: &dyn reactive_core::key::AnyKey) -> bool {
        true
    }

    fn get(storage: &Self::Storage, _key: &dyn reactive_core::key::AnyKey) -> Option<Value> {
        Some(value_of(storage.clone()))
    }
}

/// Tracked read handle for a derived set.
#[derive(Clone)]
pub struct DerivedSetRef<T> {
    view: ViewId,
    field: FieldId,
    content: fn(&dyn Any) -> PSet<T>,
}

impl<T> PartialEq for DerivedSetRef<T> {
    fn eq(&self, other: &Self) -> bool {
        self.view == other.view && self.field == other.field
    }
}

impl<T> Eq for DerivedSetRef<T> {}

impl<T> std::fmt::Debug for DerivedSetRef<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DerivedSetRef")
            .field("view", &self.view)
            .field("field", &self.field)
            .finish_non_exhaustive()
    }
}

impl<T> DerivedSetRef<T>
where
    T: Ord + Hash + Clone + Send + Sync + std::fmt::Debug + 'static,
{
    /// Builds a handle for one derived set field.
    #[must_use]
    pub fn new(view: ViewId, field: FieldId, content: fn(&dyn Any) -> PSet<T>) -> Self {
        Self {
            view,
            field,
            content,
        }
    }

    fn unit(&self) -> UnitKey {
        UnitKey::field(self.view, self.field, UnitKind::Value)
    }

    /// Reads the whole set.
    #[must_use]
    pub fn to_vec(&self) -> Vec<T> {
        let set: PSet<T> = read_unit_value(self.unit(), |content| Some((self.content)(content)));
        set.iter().cloned().collect()
    }

    /// Number of members.
    #[must_use]
    pub fn len(&self) -> usize {
        self.to_vec().len()
    }

    /// Returns `true` when the set has no members.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Defines the whole set from the currently executing computation.
    pub fn define_all<I>(&self, members: I, label: &'static str)
    where
        I: IntoIterator<Item = T>,
    {
        let set: PSet<T> = members.into_iter().collect();
        define_unit(self.unit(), set, label);
    }
}

impl<T> ViewFieldHandle for DerivedSet<T>
where
    T: Ord + Hash + Clone + Send + Sync + std::fmt::Debug + 'static,
{
    type Handle = DerivedSetRef<T>;
    type Editor<'a> = ();

    fn handle(
        view: ViewId,
        field: FieldId,
        content: fn(&dyn Any) -> Self::Storage,
    ) -> Self::Handle {
        DerivedSetRef::new(view, field, content)
    }

    fn editor<'a>(
        _edit: &'a mut Edit,
        _view: ViewId,
        _field: FieldId,
        _storage: &'a mut Self::Storage,
    ) -> Self::Editor<'a> {
    }
}
