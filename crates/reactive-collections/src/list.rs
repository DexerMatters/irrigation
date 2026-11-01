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

    /// Defines a derived keyed list from this source list's members.
    ///
    /// Member identities are the source identities, so downstream per-member
    /// work survives reorder and unrelated edits.
    pub fn to_list<V, F>(&self, target: &DerivedListRef<V>, f: F)
    where
        V: Clone + PartialEq + Send + Sync + 'static,
        F: Fn(u64, &T) -> Option<(u64, V)>,
    {
        let mut items: Vec<(u64, V)> = Vec::new();
        for id in self.ids().iter() {
            let value: T = read_unit_value(
                UnitKey::entity(self.view, self.field, UnitKind::Value, any_key(*id)),
                |content| {
                    (self.content)(content)
                        .iter()
                        .find(|(existing, _)| existing == id)
                        .map(|(_, value)| value.clone())
                },
            );
            if let Some((key, mapped)) = f(*id, &value) {
                items.push((key, mapped));
            }
        }
        target.define_keyed(items, "list_to_list");
    }

    /// Defines a derived map from this source list's elements.
    ///
    /// One map entry per element identity; a removed element retracts its entry.
    pub fn to_map<K, V, F>(&self, target: &crate::map::MapRef<K, V>, f: F)
    where
        K: Ord + std::hash::Hash + Clone + std::fmt::Debug + Send + Sync + 'static,
        V: Clone + PartialEq + Send + Sync + 'static,
        F: Fn(u64, &T) -> Option<(K, V)>,
    {
        for id in self.ids().iter() {
            let value: T = read_unit_value(
                UnitKey::entity(self.view, self.field, UnitKind::Value, any_key(*id)),
                |content| {
                    (self.content)(content)
                        .iter()
                        .find(|(existing, _)| existing == id)
                        .map(|(_, value)| value.clone())
                },
            );
            if let Some((key, mapped)) = f(*id, &value) {
                target.define(key, mapped, "list_to_map");
            }
        }
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
                    routing: 0,
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

/// Storage for a derived list: one whole value or keyed entities.
#[derive(Clone, Debug, PartialEq)]
pub enum DerivedListStorage<T> {
    /// A whole-value definition; identities are positional.
    Whole(PSeq<T>),
    /// A keyed definition; identities are explicit.
    Keyed(PSeq<(u64, T)>),
}

impl<T> DerivedListStorage<T> {
    /// Number of elements.
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::Whole(items) => items.len(),
            Self::Keyed(items) => items.len(),
        }
    }

    /// Returns `true` when the list has no elements.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Identity sequence.
    #[must_use]
    pub fn ids(&self) -> PSeq<u64> {
        match self {
            Self::Whole(items) => (0..items.len() as u64).collect(),
            Self::Keyed(items) => items.iter().map(|(id, _)| *id).collect(),
        }
    }

    /// Value by identity (positional for a whole-value list).
    #[must_use]
    pub fn get(&self, id: u64) -> Option<&T> {
        match self {
            Self::Whole(items) => items.get(id as usize),
            Self::Keyed(items) => items
                .iter()
                .find(|(existing, _)| *existing == id)
                .map(|(_, value)| value),
        }
    }

    /// Whether the identity is present.
    #[must_use]
    pub fn contains(&self, id: u64) -> bool {
        self.get(id).is_some()
    }

    fn with_entity(&self, id: u64, value: T) -> Self
    where
        T: Clone,
    {
        match self {
            Self::Whole(items) => {
                let mut keyed: PSeq<(u64, T)> = items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| (index as u64, item.clone()))
                    .collect();
                keyed = upsert(keyed, id, value);
                Self::Keyed(keyed)
            }
            Self::Keyed(items) => Self::Keyed(upsert(items.clone(), id, value)),
        }
    }

    fn without_entity(&self, id: u64) -> Self
    where
        T: Clone,
    {
        match self {
            Self::Whole(_) => self.clone(),
            Self::Keyed(items) => match items.iter().position(|(existing, _)| *existing == id) {
                Some(index) => Self::Keyed(items.remove(index).0),
                None => self.clone(),
            },
        }
    }

    fn reorder(&self, ids: &PSeq<u64>) -> Self
    where
        T: Clone,
    {
        match self {
            Self::Keyed(items) => {
                let mut ordered = PSeq::new();
                for id in ids.iter() {
                    if let Some((_, value)) = items.iter().find(|(existing, _)| existing == id) {
                        ordered = ordered.push((*id, value.clone()));
                    }
                }
                Self::Keyed(ordered)
            }
            Self::Whole(_) => self.clone(),
        }
    }
}

impl<T: Clone> DerivedListStorage<T> {
    /// Values in order.
    #[must_use]
    pub fn to_vec(&self) -> Vec<T> {
        match self {
            Self::Whole(items) => items.to_vec(),
            Self::Keyed(items) => items.iter().map(|(_, value)| value.clone()).collect(),
        }
    }
}

fn upsert<T: Clone>(items: PSeq<(u64, T)>, id: u64, value: T) -> PSeq<(u64, T)> {
    match items.iter().position(|(existing, _)| *existing == id) {
        Some(index) => items.set(index, (id, value)),
        None => items.push((id, value)),
    }
}

/// A derived list field: one whole value or keyed entities.
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
    type Storage = DerivedListStorage<T>;

    const KIND: FieldKind = FieldKind::List;

    fn empty() -> Self::Storage {
        DerivedListStorage::Whole(PSeq::new())
    }

    fn define(
        storage: &Self::Storage,
        key: &dyn reactive_core::key::AnyKey,
        value: Value,
    ) -> Self::Storage {
        if let Some(id) = key.as_any().downcast_ref::<u64>() {
            if let Some(item) = value.downcast_ref::<T>() {
                return storage.with_entity(*id, item.clone());
            }
        }
        if let Some(next) = value.downcast_ref::<DerivedListStorage<T>>() {
            return next.clone();
        }
        if let Some(ids) = value.downcast_ref::<PSeq<u64>>() {
            return storage.reorder(ids);
        }
        storage.clone()
    }

    fn retract(storage: &Self::Storage, key: &dyn reactive_core::key::AnyKey) -> Self::Storage {
        match key.as_any().downcast_ref::<u64>() {
            Some(id) => storage.without_entity(*id),
            None => DerivedListStorage::Whole(PSeq::new()),
        }
    }

    fn contains(storage: &Self::Storage, key: &dyn reactive_core::key::AnyKey) -> bool {
        key.as_any()
            .downcast_ref::<u64>()
            .is_none_or(|id| storage.contains(*id))
    }

    fn get(storage: &Self::Storage, key: &dyn reactive_core::key::AnyKey) -> Option<Value> {
        match key.as_any().downcast_ref::<u64>() {
            Some(id) => storage.get(*id).cloned().map(value_of),
            None => Some(value_of(storage.clone())),
        }
    }

    fn structure(storage: &Self::Storage) -> Option<reactive_core::descriptor::Structure> {
        let ids = storage.ids();
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
    content: fn(&dyn Any) -> DerivedListStorage<T>,
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
    pub fn new(view: ViewId, field: FieldId, content: fn(&dyn Any) -> DerivedListStorage<T>) -> Self {
        Self {
            view,
            field,
            content,
        }
    }

    fn unit(&self) -> UnitKey {
        UnitKey::field(self.view, self.field, UnitKind::Value)
    }

    fn structure_key(&self) -> UnitKey {
        structure_unit(self.view, self.field)
    }

    fn membership_key(&self, id: u64) -> UnitKey {
        UnitKey::entity(
            self.view,
            self.field,
            UnitKind::Membership,
            any_key(id),
        )
    }

    fn value_key(&self, id: u64) -> UnitKey {
        UnitKey::entity(self.view, self.field, UnitKind::Value, any_key(id))
    }

    /// Identity sequence of the list.
    #[must_use]
    pub fn ids(&self) -> PSeq<u64> {
        read_unit_value(self.structure_key(), |content| {
            Some((self.content)(content).ids())
        })
    }

    /// Reads the whole list.
    #[must_use]
    pub fn to_vec(&self) -> Vec<T> {
        let storage: DerivedListStorage<T> =
            read_unit_value(self.unit(), |content| Some((self.content)(content)));
        storage.to_vec()
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

    /// Reads one element by rank.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<T> {
        self.to_vec().into_iter().nth(index)
    }

    /// Reads one element by identity.
    ///
    /// Meaningful for keyed definitions; whole-value definitions have
    /// positional identities that are renumbered by every define.
    #[must_use]
    pub fn get_key(&self, id: u64) -> Option<T> {
        let present: bool = read_unit_value(self.membership_key(id), |content| {
            Some((self.content)(content).contains(id))
        });
        if !present {
            return None;
        }
        let value: T = read_unit_value(self.value_key(id), |content| {
            (self.content)(content).get(id).cloned()
        });
        Some(value)
    }

    /// Defines the whole list from the currently executing computation.
    #[track_caller]
    pub fn define_all<I>(&self, items: I, label: &'static str)
    where
        I: IntoIterator<Item = T>,
    {
        let list: PSeq<T> = items.into_iter().collect();
        define_unit(self.unit(), DerivedListStorage::Whole(list), label);
    }

    /// Maps every element through `f`, memoizing per identity.
    ///
    /// A keyed list keeps one member scope per identity, so a reorder or an
    /// unrelated value edit reuses the mapped results. A whole-value list has
    /// positional identities and re-runs every member when the value changes.
    #[must_use]
    pub fn map<U, F>(&self, f: F) -> Vec<U>
    where
        U: Clone + PartialEq + Send + Sync + 'static,
        F: Fn(reactive_runtime::Param<T>) -> U + Send + Sync,
    {
        let frame = reactive_core::frame::current_or_engine_panic("derived list map");
        let runtime = std::sync::Arc::clone(frame.runtime());
        let call_scope =
            runtime.child_scope(frame.scope(), u64::from(frame.node().0), "derived_list_map");
        let storage: DerivedListStorage<T> =
            read_unit_value(self.unit(), |content| Some((self.content)(content)));
        match storage {
            DerivedListStorage::Keyed(_) => {
                let ids = self.ids();
                ids.iter()
                    .map(|id| {
                        let member_scope = runtime.child_scope(call_scope, *id, "list_member");
                        let param_node = runtime.site_node(member_scope, SiteId(1));
                        let value: T =
                            reactive_core::ops::eval_node_with(&runtime, param_node, || {
                                self.get_key(*id).unwrap_or_else(|| {
                                    reactive_core::ops::panic_engine(
                                        reactive_core::error::ComputeError::internal(
                                            "keyed list member disappeared",
                                        ),
                                    )
                                })
                            });
                        let result_node = runtime.site_node(member_scope, SiteId(2));
                        reactive_core::ops::eval_node_with(&runtime, result_node, || {
                            f(reactive_runtime::Param::new(param_node, value.clone()))
                        })
                    })
                    .collect()
            }
            DerivedListStorage::Whole(items) => items
                .iter()
                .enumerate()
                .map(|(index, _)| {
                    let member_scope =
                        runtime.child_scope(call_scope, index as u64, "list_member");
                    let param_node = runtime.site_node(member_scope, SiteId(1));
                    let value: T =
                        reactive_core::ops::eval_node_with(&runtime, param_node, || {
                            let storage: DerivedListStorage<T> = read_unit_value(
                                self.unit(),
                                |content| Some((self.content)(content)),
                            );
                            storage.get(index as u64).cloned().unwrap_or_else(|| {
                                reactive_core::ops::panic_engine(
                                    reactive_core::error::ComputeError::internal(
                                        "whole-value list member disappeared",
                                    ),
                                )
                            })
                        });
                    let result_node = runtime.site_node(member_scope, SiteId(2));
                    reactive_core::ops::eval_node_with(&runtime, result_node, || {
                        f(reactive_runtime::Param::new(param_node, value.clone()))
                    })
                })
                .collect(),
        }
    }

    /// Keeps elements whose predicate holds, memoizing per identity.
    #[must_use]
    pub fn filter<F>(&self, predicate: F) -> Vec<T>
    where
        F: Fn(reactive_runtime::Param<T>) -> bool,
    {
        let frame = reactive_core::frame::current_or_engine_panic("derived list filter");
        let runtime = std::sync::Arc::clone(frame.runtime());
        let call_scope =
            runtime.child_scope(frame.scope(), u64::from(frame.node().0), "derived_list_filter");
        let storage: DerivedListStorage<T> =
            read_unit_value(self.unit(), |content| Some((self.content)(content)));
        let mut kept = Vec::new();
        match storage {
            DerivedListStorage::Keyed(_) => {
                for id in self.ids().iter() {
                    let member_scope = runtime.child_scope(call_scope, *id, "list_member");
                    let param_node = runtime.site_node(member_scope, SiteId(1));
                    let value: T = reactive_core::ops::eval_node_with(&runtime, param_node, || {
                        self.get_key(*id).unwrap_or_else(|| {
                            reactive_core::ops::panic_engine(
                                reactive_core::error::ComputeError::internal(
                                    "keyed list member disappeared",
                                ),
                            )
                        })
                    });
                    let result_node = runtime.site_node(member_scope, SiteId(2));
                    let keep = reactive_core::ops::eval_node_with(&runtime, result_node, || {
                        predicate(reactive_runtime::Param::new(param_node, value.clone()))
                    });
                    if keep {
                        kept.push(value);
                    }
                }
            }
            DerivedListStorage::Whole(items) => {
                for (index, _) in items.iter().enumerate() {
                    let member_scope =
                        runtime.child_scope(call_scope, index as u64, "list_member");
                    let param_node = runtime.site_node(member_scope, SiteId(1));
                    let value: T = reactive_core::ops::eval_node_with(&runtime, param_node, || {
                        let storage: DerivedListStorage<T> = read_unit_value(
                            self.unit(),
                            |content| Some((self.content)(content)),
                        );
                        storage.get(index as u64).cloned().unwrap_or_else(|| {
                            reactive_core::ops::panic_engine(
                                reactive_core::error::ComputeError::internal(
                                    "whole-value list member disappeared",
                                ),
                            )
                        })
                    });
                    let result_node = runtime.site_node(member_scope, SiteId(2));
                    let keep = reactive_core::ops::eval_node_with(&runtime, result_node, || {
                        predicate(reactive_runtime::Param::new(param_node, value.clone()))
                    });
                    if keep {
                        kept.push(value);
                    }
                }
            }
        }
        kept
    }

    /// Maps each element to zero or more values, memoizing per identity.
    #[must_use]
    pub fn flat_map<U, F>(&self, f: F) -> Vec<U>
    where
        U: Clone + PartialEq + Send + Sync + 'static,
        F: Fn(reactive_runtime::Param<T>) -> Vec<U>,
    {
        let frame = reactive_core::frame::current_or_engine_panic("derived list flat_map");
        let runtime = std::sync::Arc::clone(frame.runtime());
        let call_scope = runtime.child_scope(
            frame.scope(),
            u64::from(frame.node().0),
            "derived_list_flat_map",
        );
        let storage: DerivedListStorage<T> =
            read_unit_value(self.unit(), |content| Some((self.content)(content)));
        let mut values = Vec::new();
        match storage {
            DerivedListStorage::Keyed(_) => {
                for id in self.ids().iter() {
                    let member_scope = runtime.child_scope(call_scope, *id, "list_member");
                    let param_node = runtime.site_node(member_scope, SiteId(1));
                    let value: T = reactive_core::ops::eval_node_with(&runtime, param_node, || {
                        self.get_key(*id).unwrap_or_else(|| {
                            reactive_core::ops::panic_engine(
                                reactive_core::error::ComputeError::internal(
                                    "keyed list member disappeared",
                                ),
                            )
                        })
                    });
                    let result_node = runtime.site_node(member_scope, SiteId(2));
                    values.extend(reactive_core::ops::eval_node_with(
                        &runtime,
                        result_node,
                        || f(reactive_runtime::Param::new(param_node, value.clone())),
                    ));
                }
            }
            DerivedListStorage::Whole(items) => {
                for (index, _) in items.iter().enumerate() {
                    let member_scope =
                        runtime.child_scope(call_scope, index as u64, "list_member");
                    let param_node = runtime.site_node(member_scope, SiteId(1));
                    let value: T = reactive_core::ops::eval_node_with(&runtime, param_node, || {
                        let storage: DerivedListStorage<T> = read_unit_value(
                            self.unit(),
                            |content| Some((self.content)(content)),
                        );
                        storage.get(index as u64).cloned().unwrap_or_else(|| {
                            reactive_core::ops::panic_engine(
                                reactive_core::error::ComputeError::internal(
                                    "whole-value list member disappeared",
                                ),
                            )
                        })
                    });
                    let result_node = runtime.site_node(member_scope, SiteId(2));
                    values.extend(reactive_core::ops::eval_node_with(
                        &runtime,
                        result_node,
                        || f(reactive_runtime::Param::new(param_node, value.clone())),
                    ));
                }
            }
        }
        values
    }

    /// Defines a derived map from this list's keyed members.
    ///
    /// One map entry per identity; a removed member retracts its entry.
    pub fn to_map<K, V, F>(&self, target: &crate::map::MapRef<K, V>, f: F)
    where
        K: Ord + std::hash::Hash + Clone + std::fmt::Debug + Send + Sync + 'static,
        V: Clone + PartialEq + Send + Sync + 'static,
        F: Fn(u64, &T) -> Option<(K, V)>,
    {
        for id in self.ids().iter() {
            if let Some(value) = self.get_key(*id) {
                if let Some((key, mapped)) = f(*id, &value) {
                    target.define(key, mapped, "list_to_map");
                }
            }
        }
    }

    /// Defines a keyed list: stable identities plus one value per entity.
    #[track_caller]
    pub fn define_keyed<I>(&self, items: I, label: &'static str)
    where
        I: IntoIterator<Item = (u64, T)>,
    {
        let items: Vec<(u64, T)> = items.into_iter().collect();
        let sequence: PSeq<(u64, T)> = items.iter().cloned().collect();
        for (id, value) in &items {
            define_unit(self.value_key(*id), value.clone(), label);
        }
        define_unit(
            self.unit(),
            DerivedListStorage::Keyed(sequence),
            label,
        );
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
            value_of(DerivedListStorage::Whole(
                [1u32, 2, 3].into_iter().collect::<PSeq<u32>>(),
            )),
        );
        assert_eq!(storage.to_vec(), vec![1, 2, 3]);
    }

    #[test]
    fn derived_list_field_keyed_entities_keep_identity() {
        let mut storage = <DerivedList<u32> as ViewField>::empty();
        for (id, value) in [(2u64, 20u32), (1, 10)] {
            storage = <DerivedList<u32> as ViewField>::define(
                &storage,
                any_key(id).as_ref(),
                value_of(value),
            );
        }
        let key = UnitKey::field(ViewId(1), FieldId(0), UnitKind::Value);
        storage = <DerivedList<u32> as ViewField>::define(
            &storage,
            key.entity_key().map_or(&(), |key| key.as_ref()),
            value_of(DerivedListStorage::Keyed(
                [(2u64, 20u32), (1, 10)].into_iter().collect(),
            )),
        );
        assert_eq!(storage.to_vec(), vec![20, 10]);
        assert_eq!(storage.ids().to_vec(), vec![2, 1]);
        assert_eq!(storage.get(1), Some(&10));
        let structure = <DerivedList<u32> as ViewField>::structure(&storage).expect("structure");
        let ids = structure.value.downcast_ref::<PSeq<u64>>().expect("ids");
        assert_eq!(ids.to_vec(), vec![2, 1]);
    }
}
