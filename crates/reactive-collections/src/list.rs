//! Incremental sequences.
//!
//! A source list separates membership from payload: the structure unit holds
//! stable element ids in order, and each element's value lives in its own unit.
//! Inserting at the front therefore changes the structure but leaves every
//! existing element's payload observation untouched, which is what keeps
//! identity-based mapped work alive across an insertion.
//!
//! A derived list is defined as one whole value. A producer that reconstructs a
//! sequence has no occurrence identity to offer, and the plan requires that
//! boundary to be explicit rather than guessed from content equality.

use std::any::Any;
use std::marker::PhantomData;

use reactive_core::ids::{FieldId, SiteId, ViewId};
use reactive_core::key::any_key;
use reactive_core::value::{Value, value_of};
use reactive_runtime::edit::Edit;
use reactive_runtime::field::{ViewFieldHandle, structure_unit};
use reactive_runtime::helpers::{define_unit, read_unit_value};
use reactive_core::descriptor::{FieldKind, UnitKey, UnitKind};
use reactive_store::field::ViewField;
use reactive_store::persistent::PSeq;

/// A source list field with stable element identity.
pub struct SourceList<T>(PhantomData<fn() -> T>);

impl<T> std::fmt::Debug for SourceList<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SourceList")
    }
}

impl<T> ViewField for SourceList<T>
where
    T: Clone + PartialEq + Send + Sync + 'static,
{
    type Storage = PSeq<(u64, T)>;

    const KIND: FieldKind = FieldKind::List;

    fn empty() -> Self::Storage {
        PSeq::new()
    }

    fn define(
        storage: &Self::Storage,
        key: &dyn reactive_core::key::AnyKey,
        value: Value,
    ) -> Self::Storage {
        let (Some(id), Some(value)) = (
            key.as_any().downcast_ref::<u64>(),
            value.downcast_ref::<T>(),
        ) else {
            return storage.clone();
        };
        match storage.iter().position(|(existing, _)| existing == id) {
            Some(index) => storage.set(index, (*id, value.clone())),
            None => storage.push((*id, value.clone())),
        }
    }

    fn retract(storage: &Self::Storage, key: &dyn reactive_core::key::AnyKey) -> Self::Storage {
        let Some(id) = key.as_any().downcast_ref::<u64>() else {
            return storage.clone();
        };
        match storage.iter().position(|(existing, _)| existing == id) {
            Some(index) => storage.remove(index).0,
            None => storage.clone(),
        }
    }

    fn contains(storage: &Self::Storage, key: &dyn reactive_core::key::AnyKey) -> bool {
        key.as_any()
            .downcast_ref::<u64>()
            .is_some_and(|id| storage.iter().any(|(existing, _)| existing == id))
    }

    fn structure(storage: &Self::Storage) -> Option<reactive_core::descriptor::Structure> {
        let ids: PSeq<u64> = storage.iter().map(|(id, _)| *id).collect();
        Some(reactive_core::descriptor::Structure {
            value: value_of(ids),
            eq: |left, right| {
                left.downcast_ref::<PSeq<u64>>() == right.downcast_ref::<PSeq<u64>>()
            },
        })
    }

    fn get(storage: &Self::Storage, key: &dyn reactive_core::key::AnyKey) -> Option<Value> {
        let id = key.as_any().downcast_ref::<u64>()?;
        storage
            .iter()
            .find(|(existing, _)| existing == id)
            .map(|(_, value)| value_of(value.clone()))
    }
}

/// Tracked read handle for a source list.
#[derive(Clone)]
pub struct SourceListRef<T> {
    view: ViewId,
    field: FieldId,
    content: fn(&dyn Any) -> PSeq<(u64, T)>,
}

impl<T> PartialEq for SourceListRef<T> {
    fn eq(&self, other: &Self) -> bool {
        self.view == other.view && self.field == other.field
    }
}

impl<T> Eq for SourceListRef<T> {}

impl<T> std::fmt::Debug for SourceListRef<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceListRef")
            .field("view", &self.view)
            .field("field", &self.field)
            .finish_non_exhaustive()
    }
}

impl<T> SourceListRef<T>
where
    T: Clone + PartialEq + Send + Sync + 'static,
{
    /// Builds a handle for one list field.
    #[must_use]
    pub fn new(view: ViewId, field: FieldId, content: fn(&dyn Any) -> PSeq<(u64, T)>) -> Self {
        Self {
            view,
            field,
            content,
        }
    }

    fn ids(&self) -> PSeq<u64> {
        read_unit_value(structure_unit(self.view, self.field), |content| {
            Some(
                (self.content)(content)
                    .iter()
                    .map(|(id, _)| *id)
                    .collect(),
            )
        })
    }

    fn membership_key(&self, id: u64) -> UnitKey {
        UnitKey::entity(self.view, self.field, UnitKind::Membership, any_key(id))
    }

    fn value_key(&self, id: u64) -> UnitKey {
        UnitKey::entity(self.view, self.field, UnitKind::Value, any_key(id))
    }

    /// Number of elements.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ids().len()
    }

    /// Returns `true` when the list has no elements.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Reads the element at `index`, observing the rank and the payload.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<T> {
        let id = *self.ids().get(index)?;
        let present: bool = read_unit_value(self.membership_key(id), |_| Some(true));
        if !present {
            return None;
        }
        let value: T = read_unit_value(self.value_key(id), |content| {
            (self.content)(content)
                .iter()
                .find(|(existing, _)| *existing == id)
                .map(|(_, value)| value.clone())
        });
        Some(value)
    }

    /// Reads every element in order.
    #[must_use]
    pub fn to_vec(&self) -> Vec<T> {
        let content = self.content;
        self.ids()
            .iter()
            .map(|id| {
                read_unit_value(self.value_key(*id), |any| {
                    content(any)
                        .iter()
                        .find(|(existing, _)| existing == id)
                        .map(|(_, value)| value.clone())
                })
            })
            .collect()
    }

    /// Maps every element through `f`, memoizing per element identity.
    ///
    /// The closure receives a [`Param`] that records the element observation on
    /// every dereference, so a primitive inside the closure depends on exactly
    /// the element it read. With more than one worker, chunks run in parallel;
    /// the result order is fixed by element order, not completion order.
    #[must_use]
    pub fn map<U, F>(&self, f: F) -> Vec<U>
    where
        U: Clone + PartialEq + Send + Sync + 'static,
        T: Send + Sync,
        F: Fn(reactive_runtime::Param<T>) -> U + Send + Sync,
    {
        let frame = reactive_core::frame::current_or_engine_panic("list map");
        let runtime = std::sync::Arc::clone(frame.runtime());
        let call_scope = runtime.child_scope(frame.scope(), u64::from(frame.node().0), "list_map");
        let ids = self.ids().to_vec();
        let content = self.content;
        let view = self.view;
        let field = self.field;
        let workers = runtime.workers();
        if workers > 1 && ids.len() >= 8 {
            let chunk = ids.len().div_ceil(workers);
            let chunks: Vec<Vec<u64>> = ids.chunks(chunk).map(<[u64]>::to_vec).collect();
            let mut results = Vec::with_capacity(ids.len());
            std::thread::scope(|scope| {
                let handles: Vec<_> = chunks
                    .iter()
                    .map(|chunk| {
                        let runtime = std::sync::Arc::clone(&runtime);
                        let f = &f;
                        scope.spawn(move || {
                            let mut values = Vec::with_capacity(chunk.len());
                            for id in chunk {
                                values.push(map_member(
                                    &runtime, call_scope, *id, view, field, content, f,
                                ));
                            }
                            values
                        })
                    })
                    .collect();
                for handle in handles {
                    match handle.join() {
                        Ok(values) => results.extend(values),
                        // Resume the original payload so an engine error keeps
                        // its kind (Cycle, Cancelled, ...) instead of becoming a
                        // generic `Panicked` with a wrapped message.
                        Err(payload) => std::panic::resume_unwind(payload),
                    }
                }
            });
            for id in &ids {
                let node = runtime.site_node(
                    runtime.child_scope(call_scope, *id, "list_member"),
                    SiteId(2),
                );
                reactive_core::frame::record_dep(reactive_core::frame::Dep {
                    node,
                    version: runtime.changed_at(node),
                });
            }
            return results;
        }
        ids.iter()
            .map(|id| map_member(&runtime, call_scope, *id, view, field, content, &f))
            .collect()
    }

    /// Keeps elements whose predicate holds.
    #[must_use]
    pub fn filter<F>(&self, predicate: F) -> Vec<T>
    where
        F: Fn(reactive_runtime::Param<T>) -> bool,
    {
        let frame = reactive_core::frame::current_or_engine_panic("list filter");
        let runtime = std::sync::Arc::clone(frame.runtime());
        let call_scope = runtime.child_scope(frame.scope(), u64::from(frame.node().0), "list_filter");
        let content = self.content;
        let view = self.view;
        let field = self.field;
        self.ids()
            .iter()
            .filter_map(|id| {
                let member_scope = runtime.child_scope(call_scope, *id, "list_member");
                let id = *id;
                let param_node = runtime.site_node(member_scope, SiteId(1));
                let value: T = reactive_core::ops::eval_node(param_node, || {
                    read_unit_value(
                        UnitKey::entity(view, field, UnitKind::Value, any_key(id)),
                        |any| {
                            content(any)
                                .iter()
                                .find(|(existing, _)| *existing == id)
                                .map(|(_, value)| value.clone())
                        },
                    )
                });
                let result_node = runtime.site_node(member_scope, SiteId(2));
                let keep = reactive_core::ops::eval_node(result_node, || {
                    predicate(reactive_runtime::Param::new(param_node, value.clone()))
                });
                keep.then_some(value)
            })
            .collect()
    }

    /// Maps each element to zero or more values.
    #[must_use]
    pub fn flat_map<U, F>(&self, f: F) -> Vec<U>
    where
        U: Clone + PartialEq + Send + Sync + 'static,
        F: Fn(reactive_runtime::Param<T>) -> Vec<U>,
    {
        let frame = reactive_core::frame::current_or_engine_panic("list flat_map");
        let runtime = std::sync::Arc::clone(frame.runtime());
        let call_scope =
            runtime.child_scope(frame.scope(), u64::from(frame.node().0), "list_flat_map");
        let content = self.content;
        let view = self.view;
        let field = self.field;
        self.ids()
            .iter()
            .flat_map(|id| {
                let member_scope = runtime.child_scope(call_scope, *id, "list_member");
                let id = *id;
                let param_node = runtime.site_node(member_scope, SiteId(1));
                let value: T = reactive_core::ops::eval_node(param_node, || {
                    read_unit_value(
                        UnitKey::entity(view, field, UnitKind::Value, any_key(id)),
                        |any| {
                            content(any)
                                .iter()
                                .find(|(existing, _)| *existing == id)
                                .map(|(_, value)| value.clone())
                        },
                    )
                });
                let result_node = runtime.site_node(member_scope, SiteId(2));
                reactive_core::ops::eval_node(result_node, || {
                    f(reactive_runtime::Param::new(param_node, value.clone()))
                })
            })
            .collect()
    }
}

impl<T> ViewFieldHandle for SourceList<T>
where
    T: Clone + PartialEq + Send + Sync + 'static,
{
    type Handle = SourceListRef<T>;
    type Editor<'a> = SourceListEditor<'a, T>;

    fn handle(
        view: ViewId,
        field: FieldId,
        content: fn(&dyn Any) -> Self::Storage,
    ) -> Self::Handle {
        SourceListRef::new(view, field, content)
    }

    fn editor<'a>(
        edit: &'a mut Edit,
        view: ViewId,
        field: FieldId,
        storage: &'a mut Self::Storage,
    ) -> Self::Editor<'a> {
        SourceListEditor {
            edit,
            view,
            field,
            storage,
        }
    }
}

/// Transactional editor for a source list.
pub struct SourceListEditor<'a, T: Clone + PartialEq + Send + Sync + 'static> {
    edit: &'a mut Edit,
    view: ViewId,
    field: FieldId,
    storage: &'a mut PSeq<(u64, T)>,
}

impl<T: Clone + PartialEq + Send + Sync + 'static> std::fmt::Debug for SourceListEditor<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceListEditor").finish_non_exhaustive()
    }
}

impl<T> SourceListEditor<'_, T>
where
    T: Clone + PartialEq + Send + Sync + 'static,
{
    /// Inserts a new element at `index`, returning its stable identity.
    pub fn insert(&mut self, index: usize, value: T) -> u64 {
        let id = self.edit.alloc_entity();
        *self.storage = self.storage.insert(index, (id, value.clone()));
        self.edit.set(
            UnitKey::entity(self.view, self.field, UnitKind::Membership, any_key(id)),
            true,
        );
        self.edit.set(
            UnitKey::entity(self.view, self.field, UnitKind::Value, any_key(id)),
            value,
        );
        id
    }

    /// Appends a new element, returning its stable identity.
    pub fn push(&mut self, value: T) -> u64 {
        let index = self.storage.len();
        self.insert(index, value)
    }

    /// Removes the element at `index`, returning it.
    pub fn remove(&mut self, index: usize) -> Option<T> {
        let (next, removed) = self.storage.remove(index);
        let (id, value) = removed?;
        *self.storage = next;
        self.edit.set(
            UnitKey::entity(self.view, self.field, UnitKind::Membership, any_key(id)),
            false,
        );
        Some(value)
    }

    /// Replaces the element at `index`.
    pub fn set(&mut self, index: usize, value: T) {
        let Some((id, _)) = self.storage.get(index).cloned() else {
            return;
        };
        *self.storage = self.storage.set(index, (id, value.clone()));
        self.edit.set(
            UnitKey::entity(self.view, self.field, UnitKind::Value, any_key(id)),
            value,
        );
    }

    /// Moves an element between positions, preserving its identity.
    pub fn move_element(&mut self, from: usize, to: usize) {
        if from == to || from >= self.storage.len() || to >= self.storage.len() {
            return;
        }
        *self.storage = self.storage.move_element(from, to);
    }

    fn stage_structure(&mut self) {
        let ids: PSeq<u64> = self.storage.iter().map(|(id, _)| *id).collect();
        self.edit
            .set(structure_unit(self.view, self.field), ids);
    }
}

impl<T> Drop for SourceListEditor<'_, T>
where
    T: Clone + PartialEq + Send + Sync + 'static,
{
    fn drop(&mut self) {
        self.stage_structure();
    }
}

/// A derived list field defined as one whole value.
pub struct DerivedList<T>(PhantomData<fn() -> T>);

impl<T> std::fmt::Debug for DerivedList<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DerivedList")
    }
}

impl<T> ViewField for DerivedList<T>
where
    T: Clone + PartialEq + Send + Sync + 'static,
{
    type Storage = PSeq<T>;

    const KIND: FieldKind = FieldKind::List;

    fn empty() -> Self::Storage {
        PSeq::new()
    }

    fn define(
        storage: &Self::Storage,
        _key: &dyn reactive_core::key::AnyKey,
        value: Value,
    ) -> Self::Storage {
        value
            .downcast_ref::<PSeq<T>>()
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

    fn structure(storage: &Self::Storage) -> Option<reactive_core::descriptor::Structure> {
        let ids: PSeq<u64> = (0..storage.len() as u64).collect();
        Some(reactive_core::descriptor::Structure {
            value: value_of(ids),
            eq: |left, right| {
                left.downcast_ref::<PSeq<u64>>() == right.downcast_ref::<PSeq<u64>>()
            },
        })
    }
}

/// Tracked read handle for a derived list.
#[derive(Clone)]
pub struct DerivedListRef<T> {
    view: ViewId,
    field: FieldId,
    content: fn(&dyn Any) -> PSeq<T>,
}

impl<T> PartialEq for DerivedListRef<T> {
    fn eq(&self, other: &Self) -> bool {
        self.view == other.view && self.field == other.field
    }
}

impl<T> Eq for DerivedListRef<T> {}

impl<T> std::fmt::Debug for DerivedListRef<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DerivedListRef")
            .field("view", &self.view)
            .field("field", &self.field)
            .finish_non_exhaustive()
    }
}

impl<T> DerivedListRef<T>
where
    T: Clone + PartialEq + Send + Sync + 'static,
{
    /// Builds a handle for one derived list field.
    #[must_use]
    pub fn new(view: ViewId, field: FieldId, content: fn(&dyn Any) -> PSeq<T>) -> Self {
        Self {
            view,
            field,
            content,
        }
    }

    fn unit(&self) -> UnitKey {
        UnitKey::field(self.view, self.field, UnitKind::Value)
    }

    /// The positional identity sequence of the whole-value list.
    #[must_use]
    pub fn ids(&self) -> PSeq<u64> {
        read_unit_value(structure_unit(self.view, self.field), |content| {
            let list = (self.content)(content);
            Some((0..list.len() as u64).collect())
        })
    }

    /// Reads the whole list.
    #[must_use]
    pub fn to_vec(&self) -> Vec<T> {
        // The unit stores the same `PSeq<T>` that `define_all` stages; the
        // initializer must return that type, not a converted `Vec<T>`.
        let list: PSeq<T> = read_unit_value(self.unit(), |content| Some((self.content)(content)));
        list.to_vec()
    }

    /// Number of elements.
    #[must_use]
    pub fn len(&self) -> usize {
        self.to_vec().len()
    }

    /// Returns `true` when the list has no elements.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Reads one element.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<T> {
        self.to_vec().into_iter().nth(index)
    }

    /// Defines the whole list from the currently executing computation.
    pub fn define_all<I>(&self, items: I, label: &'static str)
    where
        I: IntoIterator<Item = T>,
    {
        let list: PSeq<T> = items.into_iter().collect();
        define_unit(self.unit(), list, label);
    }
}

impl<T> ViewFieldHandle for DerivedList<T>
where
    T: Clone + PartialEq + Send + Sync + 'static,
{
    type Handle = DerivedListRef<T>;
    type Editor<'a> = ();

    fn handle(
        view: ViewId,
        field: FieldId,
        content: fn(&dyn Any) -> Self::Storage,
    ) -> Self::Handle {
        DerivedListRef::new(view, field, content)
    }

    fn editor<'a>(
        _edit: &'a mut Edit,
        _view: ViewId,
        _field: FieldId,
        _storage: &'a mut Self::Storage,
    ) -> Self::Editor<'a> {
    }
}

fn map_member<T, U, F>(
    runtime: &std::sync::Arc<dyn reactive_core::RuntimeOps>,
    call_scope: reactive_core::ScopeId,
    id: u64,
    view: ViewId,
    field: FieldId,
    content: fn(&dyn Any) -> PSeq<(u64, T)>,
    f: &F,
) -> U
where
    T: Clone + PartialEq + Send + Sync + 'static,
    U: Clone + PartialEq + Send + Sync + 'static,
    F: Fn(reactive_runtime::Param<T>) -> U + ?Sized,
{
    let member_scope = runtime.child_scope(call_scope, id, "list_member");
    let param_node = runtime.site_node(member_scope, SiteId(1));
    let value: T = reactive_core::ops::eval_node_with(runtime, param_node, || {
        read_unit_value(
            UnitKey::entity(view, field, UnitKind::Value, any_key(id)),
            |any| {
                content(any)
                    .iter()
                    .find(|(existing, _)| *existing == id)
                    .map(|(_, value)| value.clone())
            },
        )
    });
    let result_node = runtime.site_node(member_scope, SiteId(2));
    reactive_core::ops::eval_node_with(runtime, result_node, || {
        f(reactive_runtime::Param::new(param_node, value.clone()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_list_field_tracks_identity() {
        let storage = <SourceList<u32> as ViewField>::empty();
        let key = UnitKey::entity(ViewId(1), FieldId(0), UnitKind::Value, any_key(9u64));
        let storage = <SourceList<u32> as ViewField>::define(
            &storage,
            key.entity_key().expect("entity").as_ref(),
            value_of(5u32),
        );
        assert_eq!(storage.len(), 1);
        assert_eq!(storage.get(0), Some(&(9, 5)));
        let storage = <SourceList<u32> as ViewField>::retract(
            &storage,
            key.entity_key().expect("entity").as_ref(),
        );
        assert!(storage.is_empty());
    }

    #[test]
    fn derived_list_field_replaces_the_whole_value() {
        let storage = <DerivedList<u32> as ViewField>::empty();
        let key = UnitKey::field(ViewId(1), FieldId(0), UnitKind::Value);
        let storage = <DerivedList<u32> as ViewField>::define(
            &storage,
            key.entity_key().map_or(&(), |key| key.as_ref()),
            value_of([1u32, 2, 3].into_iter().collect::<PSeq<u32>>()),
        );
        assert_eq!(storage.to_vec(), vec![1, 2, 3]);
    }
}
