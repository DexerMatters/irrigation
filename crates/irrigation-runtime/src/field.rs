//! Field handles: the tracked read/edit surface generated code uses.
//!
//! A View field is a persistent value plus a set of logical units. The handle
//! hides the unit protocol so generated accessors look like ordinary methods:
//! `options.strict()` is a tracked read; `list.get(3)` observes membership and
//! the element's payload separately.

use std::any::Any;

use irrigation_core::descriptor::{UnitKey, UnitKind};
use irrigation_core::ids::{FieldId, ViewId};
use irrigation_core::key::any_key;
use irrigation_store::field::{Scalar, ViewField};

use crate::edit::Edit;
use crate::helpers::read_unit_value;

/// A field type that can produce a tracked handle and an editor.
pub trait ViewFieldHandle: ViewField {
    /// Tracked read handle.
    type Handle;
    /// Transactional editor handle.
    type Editor<'a>;

    /// Builds the read handle from a View field and content accessor.
    fn handle(view: ViewId, field: FieldId, content: fn(&dyn Any) -> Self::Storage)
    -> Self::Handle;

    /// Builds the editor handle over the transaction and mutable storage.
    fn editor<'a>(
        edit: &'a mut Edit,
        view: ViewId,
        field: FieldId,
        storage: &'a mut Self::Storage,
    ) -> Self::Editor<'a>;
}

/// Tracked handle for a single-valued field.
pub struct ScalarRef<T> {
    view: ViewId,
    field: FieldId,
    content: fn(&dyn Any) -> T,
}

impl<T> Copy for ScalarRef<T> {}

impl<T> Clone for ScalarRef<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> std::fmt::Debug for ScalarRef<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScalarRef")
            .field("view", &self.view)
            .field("field", &self.field)
            .finish_non_exhaustive()
    }
}

impl<T> PartialEq for ScalarRef<T> {
    fn eq(&self, other: &Self) -> bool {
        self.view == other.view && self.field == other.field
    }
}

impl<T> Eq for ScalarRef<T> {}

impl<T> ScalarRef<T> {
    /// Builds a handle for one scalar field.
    #[must_use]
    pub const fn new(view: ViewId, field: FieldId, content: fn(&dyn Any) -> T) -> Self {
        Self {
            view,
            field,
            content,
        }
    }

    /// The unit observed by this handle.
    #[must_use]
    pub fn unit(&self) -> UnitKey {
        UnitKey::field(self.view, self.field, UnitKind::Value)
    }
}

impl<T> ScalarRef<T>
where
    T: Clone + PartialEq + Send + Sync + 'static,
{
    /// Reads the field, recording the observation.
    ///
    /// # Panics
    ///
    /// Panics with an engine payload outside a computation.
    #[must_use]
    pub fn read(&self) -> T {
        read_unit_value(self.unit(), |content| Some((self.content)(content)))
    }
}

/// Editor for a single-valued field.
pub struct ScalarEditor<'a, T> {
    edit: &'a mut Edit,
    unit: UnitKey,
    marker: std::marker::PhantomData<fn() -> T>,
}

impl<T> std::fmt::Debug for ScalarEditor<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScalarEditor").finish_non_exhaustive()
    }
}

impl<'a, T> ScalarEditor<'a, T>
where
    T: Clone + PartialEq + Send + Sync + 'static,
{
    /// Writes the field, replacing the committed value at commit time.
    pub fn set(&mut self, value: T) {
        self.edit.set(self.unit.clone(), value);
    }
}

impl<T> ViewFieldHandle for Scalar<T>
where
    T: Clone + Default + PartialEq + Send + Sync + 'static,
{
    type Handle = ScalarRef<T>;
    type Editor<'a> = ScalarEditor<'a, T>;

    fn handle(
        view: ViewId,
        field: FieldId,
        content: fn(&dyn Any) -> Self::Storage,
    ) -> Self::Handle {
        ScalarRef::new(view, field, content)
    }

    fn editor<'a>(
        edit: &'a mut Edit,
        view: ViewId,
        field: FieldId,
        _storage: &'a mut Self::Storage,
    ) -> Self::Editor<'a> {
        ScalarEditor {
            edit,
            unit: UnitKey::field(view, field, UnitKind::Value),
            marker: std::marker::PhantomData,
        }
    }
}

/// The membership unit key for one entity in a collection field.
#[must_use]
pub fn membership_unit(view: ViewId, field: FieldId, key: impl Into<EntityKey>) -> UnitKey {
    UnitKey::entity(view, field, UnitKind::Membership, key.into().0)
}

/// The value unit key for one entity in a collection field.
#[must_use]
pub fn value_unit(view: ViewId, field: FieldId, key: impl Into<EntityKey>) -> UnitKey {
    UnitKey::entity(view, field, UnitKind::Value, key.into().0)
}

/// A dynamically typed entity key.
pub struct EntityKey(std::sync::Arc<dyn irrigation_core::key::AnyKey>);

impl std::fmt::Debug for EntityKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("EntityKey")
            .field(&self.0.key_display())
            .finish()
    }
}

impl EntityKey {
    /// Erases a concrete key.
    #[must_use]
    pub fn new<K>(key: K) -> Self
    where
        K: Any + std::hash::Hash + Eq + Send + Sync + std::fmt::Debug,
    {
        Self(any_key(key))
    }
}

impl<K> From<K> for EntityKey
where
    K: Any + std::hash::Hash + Eq + Send + Sync + std::fmt::Debug,
{
    fn from(key: K) -> Self {
        Self::new(key)
    }
}

/// The structure unit key for a collection field.
#[must_use]
pub fn structure_unit(view: ViewId, field: FieldId) -> UnitKey {
    UnitKey::field(view, field, UnitKind::Structure)
}
