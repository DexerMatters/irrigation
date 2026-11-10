//! Incremental sequences.
//!
//! A source list separates membership from payload: the structure unit holds
//! stable element ids in order, and each element's value lives in its own unit.
//! Inserting at the front therefore changes the structure but leaves every
//! existing element's payload observation untouched, which is what keeps
//! identity-based mapped work alive across an insertion.
//!
//! A derived list has two publication modes, and the producer chooses per
//! definition rather than letting the runtime guess from content equality.
//!
//! `define_all` states the sequence as one whole value: a producer that
//! reconstructs a sequence has no occurrence identity to offer, so every
//! restatement is a new sequence. `define_keyed` states it as one value draft per
//! element plus a `Structure` draft, and that structure draft is what declares
//! the keyed protocol — the same protocol a source list carries, so an element's
//! payload observation survives a reorder or an insertion elsewhere.

use std::any::Any;
use std::marker::PhantomData;

use irrigation_core::descriptor::{FieldKind, UnitKey, UnitKind, WriteScope};
use irrigation_core::error::{ComputeError, PreconditionReason};
use irrigation_core::ids::{FieldId, SiteId, ViewId};
use irrigation_core::key::any_key;
use irrigation_core::ops::panic_engine;
use irrigation_core::value::{Value, value_of};
use irrigation_runtime::edit::Edit;
use irrigation_runtime::field::{ViewFieldHandle, structure_unit};
use irrigation_runtime::helpers::{define_unit, read_unit_value, try_read_unit_value, write_group};
use irrigation_store::delta::{AppliedDelta, DeltaStore, KeyedListDelta, KeyedListEdit};
use irrigation_store::field::ViewField;
use irrigation_store::persistent::{PMap, PSeq};

/// A declaration-only marker for a list field in `#[view]`.
///
/// The macro chooses `SourceList` or `DerivedList` storage from the View kind;
/// this type exists so the field declaration is valid Rust in every context and
/// so documentation can name the shape.
pub struct List<T>(std::marker::PhantomData<fn() -> T>);

impl<T> std::fmt::Debug for List<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("List")
    }
}

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

    fn len(storage: &Self::Storage) -> Option<usize> {
        Some(storage.len())
    }

    fn define(
        storage: &Self::Storage,
        key: &dyn irrigation_core::key::AnyKey,
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

    fn retract(storage: &Self::Storage, key: &dyn irrigation_core::key::AnyKey) -> Self::Storage {
        let Some(id) = key.as_any().downcast_ref::<u64>() else {
            return storage.clone();
        };
        match storage.iter().position(|(existing, _)| existing == id) {
            Some(index) => storage.remove(index).0,
            None => storage.clone(),
        }
    }

    fn contains(storage: &Self::Storage, key: &dyn irrigation_core::key::AnyKey) -> bool {
        key.as_any()
            .downcast_ref::<u64>()
            .is_some_and(|id| storage.iter().any(|(existing, _)| existing == id))
    }

    fn structure(storage: &Self::Storage) -> Option<irrigation_core::descriptor::Structure> {
        let ids: PSeq<u64> = storage.iter().map(|(id, _)| *id).collect();
        Some(irrigation_core::descriptor::Structure {
            value: value_of(ids),
            eq: |left, right| left.downcast_ref::<PSeq<u64>>() == right.downcast_ref::<PSeq<u64>>(),
        })
    }

    fn get(storage: &Self::Storage, key: &dyn irrigation_core::key::AnyKey) -> Option<Value> {
        let id = key.as_any().downcast_ref::<u64>()?;
        storage
            .iter()
            .find(|(existing, _)| existing == id)
            .map(|(_, value)| value_of(value.clone()))
    }

    fn unit_eq(old: &Self::Storage, new: &Self::Storage, unit: &UnitKey) -> Option<bool> {
        if unit.entity.is_none() && unit.kind == UnitKind::Value {
            return Some(old == new);
        }
        if unit.kind == UnitKind::Structure {
            let old_ids: PSeq<u64> = old.iter().map(|(id, _)| *id).collect();
            let new_ids: PSeq<u64> = new.iter().map(|(id, _)| *id).collect();
            return Some(old_ids == new_ids);
        }
        let id = unit.entity.as_deref()?.as_any().downcast_ref::<u64>()?;
        match unit.kind {
            UnitKind::Value => {
                let old_value = old
                    .iter()
                    .find(|(existing, _)| existing == id)
                    .map(|(_, value)| value);
                let new_value = new
                    .iter()
                    .find(|(existing, _)| existing == id)
                    .map(|(_, value)| value);
                Some(old_value == new_value)
            }
            UnitKind::Membership => {
                let old_present = old.iter().any(|(existing, _)| existing == id);
                let new_present = new.iter().any(|(existing, _)| existing == id);
                Some(old_present == new_present)
            }
            _ => None,
        }
    }

    fn unit_moves_content(unit: &UnitKey) -> bool {
        // A source list stores its identity order in content as well as its
        // payloads; a reorder therefore moves content even without a value edit.
        unit.kind == UnitKind::Value || unit.kind == UnitKind::Structure
    }
}

/// Tracked read handle for a source list.
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

impl<T> Copy for SourceListRef<T> {}

impl<T> Clone for SourceListRef<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> SourceListRef<T>
where
    T: Clone + PartialEq + Send + Sync + 'static,
{
    /// Builds a handle for one list field.
    #[must_use]
    pub const fn new(
        view: ViewId,
        field: FieldId,
        content: fn(&dyn Any) -> PSeq<(u64, T)>,
    ) -> Self {
        Self {
            view,
            field,
            content,
        }
    }

    /// Identity sequence of the source list.
    #[must_use]
    pub fn ids(&self) -> PSeq<u64> {
        read_unit_value(structure_unit(self.view, self.field), |content| {
            Some((self.content)(content).iter().map(|(id, _)| *id).collect())
        })
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
        self.member(id)
    }

    /// Reads one member after its identity was observed in the structure.
    ///
    /// The identity is a snapshot, not a promise that its cells still exist: a
    /// concurrent retraction can move the element between `ids()` and this
    /// read. An element whose cells disagree is absent, never an engine panic.
    fn member(&self, id: u64) -> Option<T> {
        read_member(self.view, self.field, self.content, id)
    }

    /// Reads every element in order.
    #[must_use]
    pub fn to_vec(&self) -> Vec<T> {
        self.ids()
            .iter()
            .filter_map(|id| self.member(*id))
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
            if let Some(value) = self.member(*id) {
                if let Some((key, mapped)) = f(*id, &value) {
                    items.push((key, mapped));
                }
            }
        }
        target.define_keyed(items, "list_to_list");
    }

    /// Defines a derived map from this source list's elements.
    ///
    /// One map entry per element identity; a removed element retracts its entry.
    /// The target is published as a whole field
    /// ([`crate::map::MapRef::define_all`]), so the retraction does not depend
    /// on the calling body restating its output.
    pub fn to_map<K, V, F>(&self, target: &crate::map::MapRef<K, V>, f: F)
    where
        K: Ord + std::hash::Hash + Clone + std::fmt::Debug + Send + Sync + 'static,
        V: Clone + PartialEq + Send + Sync + 'static,
        F: Fn(u64, &T) -> Option<(K, V)>,
    {
        let mut items = Vec::new();
        for id in self.ids().iter() {
            if let Some(value) = self.member(*id) {
                if let Some(item) = f(*id, &value) {
                    items.push(item);
                }
            }
        }
        target.define_all(items, "list_to_map");
    }

    /// Maps every element through `f`, memoizing per element identity.
    ///
    /// The closure receives a `Param` (`irrigation_runtime::Param`, which this
    /// crate does not depend on) that records the element observation on every
    /// dereference, so a primitive inside the closure depends on exactly the
    /// element it read. With more than one worker, chunks run in parallel;
    /// the result order is fixed by element order, not completion order.
    #[must_use]
    pub fn map<U, F>(&self, f: F) -> Vec<U>
    where
        U: Clone + PartialEq + Send + Sync + 'static,
        T: Send + Sync,
        F: Fn(irrigation_runtime::Param<T>) -> U + Send + Sync,
    {
        let frame = irrigation_core::frame::current_or_engine_panic("list map");
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
            // Frames are thread-local, so a spawned chunk would otherwise open
            // at depth zero under a fresh, never-cancelled token. A scratch
            // frame carries the demand's depth and cancellation without
            // recording the members' observations, which is the same trace the
            // manual result-node loop below already owns.
            let depth = frame.depth();
            let node = frame.node();
            let cancel = frame.cancel().clone();
            let mut results: Vec<Option<U>> = Vec::with_capacity(ids.len());
            std::thread::scope(|scope| {
                let handles: Vec<_> = chunks
                    .iter()
                    .map(|chunk| {
                        let runtime = std::sync::Arc::clone(&runtime);
                        let cancel = cancel.clone();
                        let f = &f;
                        scope.spawn(move || {
                            // Frames are thread-local execution state: `RefCell`
                            // is intentional and the frame never crosses a thread
                            // boundary, so the non-`Sync` `Arc` never escapes it.
                            #[allow(clippy::arc_with_non_send_sync)]
                            let scratch =
                                std::sync::Arc::new(irrigation_core::frame::Frame::scratch(
                                    std::sync::Arc::clone(&runtime),
                                    node,
                                    call_scope,
                                    depth,
                                    cancel,
                                ));
                            let _guard = irrigation_core::frame::FrameGuard::push(scratch);
                            let mut values = Vec::with_capacity(chunk.len());
                            for id in chunk {
                                values.push(map_member(
                                    &runtime,
                                    call_scope,
                                    *id,
                                    || read_member(view, field, content, *id),
                                    f,
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
            for (id, value) in ids.iter().zip(&results) {
                let member_scope = runtime.child_scope(call_scope, *id, "list_member");
                // The sequential arm runs `map_member` on the demand's own
                // frame, so its trace names the member's param node for every
                // identity — an absent member included, whose negative payload
                // observation is what a later install has to invalidate. A
                // chunk records nothing (its frame is scratch), so the replay
                // restates the same per-identity dependency here.
                record_member_site(&runtime, member_scope, SiteId(1));
                if value.is_none() {
                    continue;
                }
                record_member_site(&runtime, member_scope, SiteId(2));
            }
            return results.into_iter().flatten().collect();
        }
        ids.iter()
            .filter_map(|id| {
                map_member(
                    &runtime,
                    call_scope,
                    *id,
                    || read_member(view, field, content, *id),
                    &f,
                )
            })
            .collect()
    }

    /// Keeps elements whose predicate holds.
    #[must_use]
    pub fn filter<F>(&self, predicate: F) -> Vec<T>
    where
        F: Fn(irrigation_runtime::Param<T>) -> bool,
    {
        let frame = irrigation_core::frame::current_or_engine_panic("list filter");
        let runtime = std::sync::Arc::clone(frame.runtime());
        let call_scope =
            runtime.child_scope(frame.scope(), u64::from(frame.node().0), "list_filter");
        let content = self.content;
        let view = self.view;
        let field = self.field;
        self.ids()
            .iter()
            .filter_map(|id| {
                filter_member(
                    &runtime,
                    call_scope,
                    *id,
                    || read_member(view, field, content, *id),
                    &predicate,
                )
            })
            .collect()
    }

    /// Maps each element to zero or more values.
    #[must_use]
    pub fn flat_map<U, F>(&self, f: F) -> Vec<U>
    where
        U: Clone + PartialEq + Send + Sync + 'static,
        F: Fn(irrigation_runtime::Param<T>) -> Vec<U>,
    {
        let frame = irrigation_core::frame::current_or_engine_panic("list flat_map");
        let runtime = std::sync::Arc::clone(frame.runtime());
        let call_scope =
            runtime.child_scope(frame.scope(), u64::from(frame.node().0), "list_flat_map");
        let content = self.content;
        let view = self.view;
        let field = self.field;
        self.ids()
            .iter()
            .filter_map(|id| {
                map_member(
                    &runtime,
                    call_scope,
                    *id,
                    || read_member(view, field, content, *id),
                    &|param| f(param),
                )
            })
            .flatten()
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
        self.edit.set(structure_unit(self.view, self.field), ids);
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
///
/// The keyed form separates order from payload: `order` is the identity
/// sequence, `values` is an indexed payload store. An upsert or removal touches
/// one payload entry and at most one order position, not a whole sequence scan.
#[derive(Clone, Debug, PartialEq)]
pub enum DerivedListStorage<T> {
    /// A whole-value definition; identities are positional.
    Whole(PSeq<T>),
    /// A keyed definition; identities are explicit and payload-indexed.
    Keyed {
        /// Identity sequence.
        order: PSeq<u64>,
        /// Payload by identity.
        values: PMap<u64, T>,
    },
}

impl<T: Clone> DerivedListStorage<T> {
    /// Builds keyed storage from `(identity, value)` pairs.
    #[must_use]
    pub fn from_keyed<I>(items: I) -> Self
    where
        I: IntoIterator<Item = (u64, T)>,
    {
        let mut order = PSeq::new();
        let mut values = PMap::new();
        for (id, value) in items {
            let (next, previous) = values.insert(id, value);
            values = next;
            if previous.is_none() {
                order = order.push(id);
            }
        }
        Self::Keyed { order, values }
    }
}

impl<T> DerivedListStorage<T>
where
    T: Clone + PartialEq,
{
    /// Applies a keyed list delta in one batch.
    #[must_use]
    pub fn apply_delta(&self, delta: &KeyedListDelta<T>) -> (Self, AppliedDelta<u64>) {
        let mut next = self.clone();
        let mut applied = AppliedDelta::empty();
        for edit in delta.edits() {
            match edit {
                KeyedListEdit::Set { id, value } => {
                    let changed = next.get(*id) != Some(value);
                    next = next.with_entity(*id, value.clone());
                    if changed {
                        applied.changed.push(*id);
                        applied.content_changed = true;
                    }
                }
                KeyedListEdit::Remove { id } => {
                    if next.contains(*id) {
                        next = next.without_entity(*id);
                        applied.removed.push(*id);
                        applied.content_changed = true;
                    }
                }
            }
        }
        (next, applied)
    }
}

impl<T> DerivedListStorage<T> {
    /// Number of elements.
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::Whole(items) => items.len(),
            Self::Keyed { order, .. } => order.len(),
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
            Self::Keyed { order, .. } => order.clone(),
        }
    }

    /// Value by identity (positional for a whole-value list).
    #[must_use]
    pub fn get(&self, id: u64) -> Option<&T> {
        match self {
            Self::Whole(items) => items.get(id as usize),
            Self::Keyed { values, .. } => values.get(&id),
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
                let mut order = PSeq::new();
                let mut values = PMap::new();
                for (index, item) in items.iter().enumerate() {
                    let key = index as u64;
                    order = order.push(key);
                    values = values.insert(key, item.clone()).0;
                }
                let (next, previous) = values.insert(id, value);
                values = next;
                if previous.is_none() {
                    order = order.push(id);
                }
                Self::Keyed { order, values }
            }
            Self::Keyed { order, values } => {
                let (next, previous) = values.clone().insert(id, value);
                let order = if previous.is_none() {
                    order.push(id)
                } else {
                    order.clone()
                };
                Self::Keyed {
                    order,
                    values: next,
                }
            }
        }
    }

    fn without_entity(&self, id: u64) -> Self
    where
        T: Clone,
    {
        match self {
            Self::Whole(_) => self.clone(),
            Self::Keyed { order, values } => {
                if values.get(&id).is_none() {
                    return self.clone();
                }
                let (values, _) = values.remove(&id);
                Self::Keyed {
                    order: order
                        .iter()
                        .copied()
                        .filter(|existing| *existing != id)
                        .collect(),
                    values,
                }
            }
        }
    }

    fn reorder(&self, ids: &PSeq<u64>) -> Self
    where
        T: Clone,
    {
        match self {
            Self::Keyed { values, .. } => {
                let mut order = PSeq::new();
                let mut kept = PMap::new();
                for id in ids.iter() {
                    if let Some(value) = values.get(id) {
                        order = order.push(*id);
                        kept = kept.insert(*id, value.clone()).0;
                    }
                }
                Self::Keyed {
                    order,
                    values: kept,
                }
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
            Self::Keyed { order, values } => order
                .iter()
                .filter_map(|id| values.get(id).cloned())
                .collect(),
        }
    }
}

impl<T> DeltaStore for DerivedListStorage<T>
where
    T: Clone + PartialEq + Send + Sync + 'static,
{
    type Key = u64;
    type Delta = KeyedListDelta<T>;

    fn apply_delta(&self, delta: &Self::Delta) -> (Self, AppliedDelta<u64>) {
        DerivedListStorage::apply_delta(self, delta)
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

    fn len(storage: &Self::Storage) -> Option<usize> {
        Some(storage.len())
    }

    fn define(
        storage: &Self::Storage,
        key: &dyn irrigation_core::key::AnyKey,
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

    fn retract(storage: &Self::Storage, key: &dyn irrigation_core::key::AnyKey) -> Self::Storage {
        match key.as_any().downcast_ref::<u64>() {
            Some(id) => storage.without_entity(*id),
            None => DerivedListStorage::Whole(PSeq::new()),
        }
    }

    fn contains(storage: &Self::Storage, key: &dyn irrigation_core::key::AnyKey) -> bool {
        key.as_any()
            .downcast_ref::<u64>()
            .is_none_or(|id| storage.contains(*id))
    }

    fn get(storage: &Self::Storage, key: &dyn irrigation_core::key::AnyKey) -> Option<Value> {
        match key.as_any().downcast_ref::<u64>() {
            Some(id) => storage.get(*id).cloned().map(value_of),
            None => Some(value_of(storage.clone())),
        }
    }

    fn unit_eq(old: &Self::Storage, new: &Self::Storage, unit: &UnitKey) -> Option<bool> {
        if unit.entity.is_none() && unit.kind == UnitKind::Value {
            return Some(old == new);
        }
        if unit.kind == UnitKind::Structure {
            return Some(old.ids() == new.ids());
        }
        let id = unit.entity.as_deref()?.as_any().downcast_ref::<u64>()?;
        match unit.kind {
            UnitKind::Value => Some(old.get(*id) == new.get(*id)),
            UnitKind::Membership => Some(old.contains(*id) == new.contains(*id)),
            _ => None,
        }
    }

    fn unit_moves_content(unit: &UnitKey) -> bool {
        // A keyed list's order/structures and payloads are content; per-id
        // membership is a projection.
        unit.kind != UnitKind::Membership
    }

    fn define_many(storage: &Self::Storage, drafts: &[(UnitKey, Value)]) -> Self::Storage {
        // A whole-field or structure draft defines the order; preserve the
        // draft order for that mixed write. Pure entity payloads are batched
        // against one keyed storage clone.
        if drafts
            .iter()
            .any(|(key, _)| key.entity.is_none() || key.kind == UnitKind::Structure)
        {
            let mut next = storage.clone();
            for (key, value) in drafts {
                let entity = key.entity.as_deref().unwrap_or(&());
                if key.kind == UnitKind::Membership && value.downcast_ref::<bool>() == Some(&false)
                {
                    next = Self::retract(&next, entity);
                } else {
                    next = Self::define(&next, entity, value.clone());
                }
            }
            return next;
        }
        let mut next = storage.clone();
        for (key, value) in drafts {
            let Some(id) = key
                .entity
                .as_deref()
                .and_then(|entity| entity.as_any().downcast_ref::<u64>().copied())
            else {
                continue;
            };
            match key.kind {
                UnitKind::Membership => {
                    if value.downcast_ref::<bool>() == Some(&false) {
                        next = next.without_entity(id);
                    }
                }
                UnitKind::Value => {
                    if let Some(value) = value.downcast_ref::<T>() {
                        next = next.with_entity(id, value.clone());
                    }
                }
                _ => {}
            }
        }
        next
    }

    fn structure(storage: &Self::Storage) -> Option<irrigation_core::descriptor::Structure> {
        let ids = storage.ids();
        Some(irrigation_core::descriptor::Structure {
            value: value_of(ids),
            eq: |left, right| left.downcast_ref::<PSeq<u64>>() == right.downcast_ref::<PSeq<u64>>(),
        })
    }
}

/// Tracked read handle for a derived list.
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

impl<T> Copy for DerivedListRef<T> {}

impl<T> Clone for DerivedListRef<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> DerivedListRef<T>
where
    T: Clone + PartialEq + Send + Sync + 'static,
{
    /// Builds a handle for one derived list field.
    #[must_use]
    pub const fn new(
        view: ViewId,
        field: FieldId,
        content: fn(&dyn Any) -> DerivedListStorage<T>,
    ) -> Self {
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
        UnitKey::entity(self.view, self.field, UnitKind::Membership, any_key(id))
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
        if !irrigation_runtime::helpers::field_protocol(self.view, self.field) {
            // A whole-value definition owns no per-id cells. Reading one anyway
            // would pin it to the members of an older definition, so index the
            // one unit the definition does own instead.
            let storage: DerivedListStorage<T> =
                read_unit_value(self.unit(), |content| Some((self.content)(content)));
            return storage.get(id).cloned();
        }
        let present: bool = read_unit_value(self.membership_key(id), |content| {
            Some((self.content)(content).contains(id))
        });
        if !present {
            return None;
        }
        // A keyed member's payload cell can be absent under a membership that
        // still reads true — a membership-only draft, or a retraction between
        // the two observations — so the payload read is total.
        try_read_unit_value(self.value_key(id), |content| {
            (self.content)(content).get(id).cloned()
        })
    }

    /// Reads one positional member of a whole-value definition.
    ///
    /// A whole-value definition has no per-id cells: every position is a slot
    /// in the one field payload. The definition can move between the structure
    /// read that named the position and this read, so an absent payload is an
    /// absent member rather than an engine failure.
    fn whole_member(&self, id: u64) -> Option<T> {
        let storage: DerivedListStorage<T> =
            try_read_unit_value(self.unit(), |content| Some((self.content)(content)))?;
        storage.get(id).cloned()
    }

    /// Defines the whole list from the currently executing computation.
    #[track_caller]
    pub fn define_all<I>(&self, items: I, label: &'static str)
    where
        I: IntoIterator<Item = T>,
    {
        let list: PSeq<T> = items.into_iter().collect();
        write_group(
            WriteScope::Field(self.view, self.field),
            label,
            &[],
            |_| (),
            |()| self.define_whole(list, label),
        );
    }

    /// Stages the whole-value definition inside an already claimed group.
    fn define_whole(&self, list: PSeq<T>, label: &'static str) {
        define_unit(self.unit(), DerivedListStorage::Whole(list), label);
    }

    /// Inserts at `index`, failing when it is past the end.
    #[track_caller]
    pub fn insert_at(&self, index: usize, value: T, label: &'static str) {
        write_group(
            WriteScope::Field(self.view, self.field),
            label,
            &[],
            |content| (self.content)(content).to_vec(),
            |mut list| {
                if index > list.len() {
                    panic_engine(ComputeError::PreconditionUnmet {
                        condition: format!("list index {index} in range"),
                        ports: vec![format!("{:?}", structure_unit(self.view, self.field))],
                        reason: PreconditionReason::ValueMismatch,
                    });
                }
                list.insert(index, value);
                let list: PSeq<T> = list.into_iter().collect();
                self.define_whole(list, label);
            },
        );
    }

    /// Replaces the element at `index`, failing when it is out of range.
    #[track_caller]
    pub fn set_at(&self, index: usize, value: T, label: &'static str) {
        write_group(
            WriteScope::Field(self.view, self.field),
            label,
            &[],
            |content| (self.content)(content).to_vec(),
            |mut list| {
                if index >= list.len() {
                    panic_engine(ComputeError::PreconditionUnmet {
                        condition: format!("list index {index} present"),
                        ports: vec![format!("{:?}", structure_unit(self.view, self.field))],
                        reason: PreconditionReason::ValueMismatch,
                    });
                }
                list[index] = value;
                let list: PSeq<T> = list.into_iter().collect();
                self.define_whole(list, label);
            },
        );
    }

    /// Removes the element at `index`, failing when it is out of range.
    #[track_caller]
    pub fn remove_at(&self, index: usize, label: &'static str) {
        write_group(
            WriteScope::Field(self.view, self.field),
            label,
            &[],
            |content| (self.content)(content).to_vec(),
            |mut list| {
                if index >= list.len() {
                    panic_engine(ComputeError::PreconditionUnmet {
                        condition: format!("list index {index} present"),
                        ports: vec![format!("{:?}", structure_unit(self.view, self.field))],
                        reason: PreconditionReason::ValueMismatch,
                    });
                }
                list.remove(index);
                let list: PSeq<T> = list.into_iter().collect();
                self.define_whole(list, label);
            },
        );
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
        F: Fn(irrigation_runtime::Param<T>) -> U + Send + Sync,
    {
        let frame = irrigation_core::frame::current_or_engine_panic("derived list map");
        let runtime = std::sync::Arc::clone(frame.runtime());
        let call_scope =
            runtime.child_scope(frame.scope(), u64::from(frame.node().0), "derived_list_map");
        let storage: DerivedListStorage<T> =
            read_unit_value(self.unit(), |content| Some((self.content)(content)));
        match storage {
            DerivedListStorage::Keyed { .. } => {
                let ids = self.ids();
                // The identity came from the structure, which a concurrent retraction can
                // move under this read. An identity whose cells are gone is skipped; the
                // structure observation makes the demand retry against the new definition.
                ids.iter()
                    .filter_map(|id| {
                        map_member(&runtime, call_scope, *id, || self.get_key(*id), &f)
                    })
                    .collect()
            }
            DerivedListStorage::Whole(items) => items
                .iter()
                .enumerate()
                .filter_map(|(index, _)| {
                    map_member(
                        &runtime,
                        call_scope,
                        index as u64,
                        || self.whole_member(index as u64),
                        &f,
                    )
                })
                .collect(),
        }
    }

    /// Keeps elements whose predicate holds, memoizing per identity.
    #[must_use]
    pub fn filter<F>(&self, predicate: F) -> Vec<T>
    where
        F: Fn(irrigation_runtime::Param<T>) -> bool,
    {
        let frame = irrigation_core::frame::current_or_engine_panic("derived list filter");
        let runtime = std::sync::Arc::clone(frame.runtime());
        let call_scope = runtime.child_scope(
            frame.scope(),
            u64::from(frame.node().0),
            "derived_list_filter",
        );
        let storage: DerivedListStorage<T> =
            read_unit_value(self.unit(), |content| Some((self.content)(content)));
        let mut kept = Vec::new();
        match storage {
            DerivedListStorage::Keyed { .. } => {
                for id in self.ids().iter() {
                    kept.extend(filter_member(
                        &runtime,
                        call_scope,
                        *id,
                        || self.get_key(*id),
                        &predicate,
                    ));
                }
            }
            DerivedListStorage::Whole(items) => {
                for (index, _) in items.iter().enumerate() {
                    kept.extend(filter_member(
                        &runtime,
                        call_scope,
                        index as u64,
                        || self.whole_member(index as u64),
                        &predicate,
                    ));
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
        F: Fn(irrigation_runtime::Param<T>) -> Vec<U>,
    {
        let frame = irrigation_core::frame::current_or_engine_panic("derived list flat_map");
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
            DerivedListStorage::Keyed { .. } => {
                for id in self.ids().iter() {
                    if let Some(chunk) =
                        map_member(&runtime, call_scope, *id, || self.get_key(*id), &f)
                    {
                        values.extend(chunk);
                    }
                }
            }
            DerivedListStorage::Whole(items) => {
                for (index, _) in items.iter().enumerate() {
                    if let Some(chunk) = map_member(
                        &runtime,
                        call_scope,
                        index as u64,
                        || self.whole_member(index as u64),
                        &f,
                    ) {
                        values.extend(chunk);
                    }
                }
            }
        }
        values
    }

    /// Defines a derived map from this list's keyed members.
    ///
    /// One map entry per identity; a removed member retracts its entry because
    /// the target is published as a whole field
    /// ([`crate::map::MapRef::define_all`]).
    pub fn to_map<K, V, F>(&self, target: &crate::map::MapRef<K, V>, f: F)
    where
        K: Ord + std::hash::Hash + Clone + std::fmt::Debug + Send + Sync + 'static,
        V: Clone + PartialEq + Send + Sync + 'static,
        F: Fn(u64, &T) -> Option<(K, V)>,
    {
        let mut items = Vec::new();
        if !irrigation_runtime::helpers::field_protocol(self.view, self.field) {
            // Same rule as `get_key`: a whole-value list is read once, not
            // entry by entry through cells nobody defined.
            let storage: DerivedListStorage<T> =
                read_unit_value(self.unit(), |content| Some((self.content)(content)));
            for id in storage.ids().iter() {
                if let Some(value) = storage.get(*id) {
                    if let Some(item) = f(*id, value) {
                        items.push(item);
                    }
                }
            }
            target.define_all(items, "list_to_map");
            return;
        }
        for id in self.ids().iter() {
            if let Some(value) = self.get_key(*id) {
                if let Some(item) = f(*id, &value) {
                    items.push(item);
                }
            }
        }
        target.define_all(items, "list_to_map");
    }

    /// Defines a keyed list: stable identities plus one value per entity.
    #[track_caller]
    pub fn define_keyed<I>(&self, items: I, label: &'static str)
    where
        I: IntoIterator<Item = (u64, T)>,
    {
        let items: Vec<(u64, T)> = items.into_iter().collect();
        write_group(
            WriteScope::Field(self.view, self.field),
            label,
            &[],
            |_| (),
            |()| {
                let ids: PSeq<u64> = items.iter().map(|(id, _)| *id).collect();
                for (id, value) in &items {
                    define_unit(self.value_key(*id), value.clone(), label);
                }
                define_unit(
                    self.unit(),
                    DerivedListStorage::from_keyed(items.iter().cloned()),
                    label,
                );
                // The structure unit is what declares the keyed protocol to
                // readers, so it is staged after the whole-field value that
                // declares the other one.
                define_unit(self.structure_key(), ids, label);
            },
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

/// Reads one source-list member, tolerating a retraction between the structure
/// read that named it and this read.
///
/// The element is present only when both its membership cell and its payload
/// cell still say so: a retraction moves the membership, a membership-only
/// draft leaves the payload absent, and either observation is meaningful. The
/// payload read is therefore total, and a member the store no longer defines is
/// absent instead of aborting the demand.
fn read_member<T>(
    view: ViewId,
    field: FieldId,
    content: fn(&dyn Any) -> PSeq<(u64, T)>,
    id: u64,
) -> Option<T>
where
    T: Clone + PartialEq + Send + Sync + 'static,
{
    let present: bool = read_unit_value(
        UnitKey::entity(view, field, UnitKind::Membership, any_key(id)),
        |any| Some(content(any).iter().any(|(existing, _)| *existing == id)),
    );
    if !present {
        return None;
    }
    try_read_unit_value(
        UnitKey::entity(view, field, UnitKind::Value, any_key(id)),
        |any| {
            content(any)
                .iter()
                .find(|(existing, _)| *existing == id)
                .map(|(_, value)| value.clone())
        },
    )
}

/// Records one member site's dependency on the executing frame.
///
/// The chunked arm evaluates its members under a scratch frame, which records
/// nothing, so it replays the observations the sequential arm's own frame
/// recorded. Both arms must name the same nodes or one worker count serves a
/// stale result while the other re-runs.
fn record_member_site(
    runtime: &std::sync::Arc<dyn irrigation_core::RuntimeOps>,
    scope: irrigation_core::ScopeId,
    site: SiteId,
) {
    let node = runtime.site_node(scope, site);
    irrigation_core::frame::record_dep(irrigation_core::frame::Dep {
        node,
        version: runtime.value_version_at(node),
        routing: 0,
        validity: runtime.validity_at(node),
    });
}

/// Evaluates one member's predicate in its own scope and returns the member when it holds.
///
/// The sibling of [`map_member`] for operators whose result is the *input* value rather than
/// the operator's output: `filter` needs both the predicate's answer and the value it tested,
/// which `map_member`'s closure never sees. The scope protocol -- a `list_member` child scope,
/// the parameter node at `SiteId(1)`, the result node at `SiteId(2)` -- is the same, and is
/// spelled here once for both families.
fn filter_member<T, F, R>(
    runtime: &std::sync::Arc<dyn irrigation_core::RuntimeOps>,
    call_scope: irrigation_core::ScopeId,
    id: u64,
    read: R,
    predicate: &F,
) -> Option<T>
where
    T: Clone + PartialEq + Send + Sync + 'static,
    F: Fn(irrigation_runtime::Param<T>) -> bool + ?Sized,
    R: FnOnce() -> Option<T>,
{
    let member_scope = runtime.child_scope(call_scope, id, "list_member");
    let param_node = runtime.site_node(member_scope, SiteId(1));
    let value: Option<T> = irrigation_core::ops::eval_node_with(runtime, param_node, read);
    let value = value?;
    let result_node = runtime.site_node(member_scope, SiteId(2));
    let keep = irrigation_core::ops::eval_node_with(runtime, result_node, || {
        predicate(irrigation_runtime::Param::new(param_node, value.clone()))
    });
    keep.then_some(value)
}

/// Evaluates one member in its own scope and returns the operator's result for it.
///
/// Every list operator needs the same three things per member: a child scope named
/// `list_member` under the operator's call scope, a parameter node at `SiteId(1)` that
/// reads the member, and a result node at `SiteId(2)` that runs the operator. The helper
/// takes the read as a closure so both families can use it -- the source family reads
/// through `read_member`, the derived family through `get_key` or `whole_member` -- and so
/// the scope name and site ids are spelled once instead of at each operator.
fn map_member<T, U, F, R>(
    runtime: &std::sync::Arc<dyn irrigation_core::RuntimeOps>,
    call_scope: irrigation_core::ScopeId,
    id: u64,
    read: R,
    f: &F,
) -> Option<U>
where
    T: Clone + PartialEq + Send + Sync + 'static,
    U: Clone + PartialEq + Send + Sync + 'static,
    F: Fn(irrigation_runtime::Param<T>) -> U + ?Sized,
    R: FnOnce() -> Option<T>,
{
    let member_scope = runtime.child_scope(call_scope, id, "list_member");
    let param_node = runtime.site_node(member_scope, SiteId(1));
    let value: Option<T> = irrigation_core::ops::eval_node_with(runtime, param_node, read);
    let value = value?;
    let result_node = runtime.site_node(member_scope, SiteId(2));
    Some(irrigation_core::ops::eval_node_with(
        runtime,
        result_node,
        || f(irrigation_runtime::Param::new(param_node, value.clone())),
    ))
}

/// Framework-native keyed list publication for general components.
pub trait ListOutputExt {
    /// Replaces a set of explicit keyed list entries as one ordered list.
    fn list_define_keyed<T>(
        &mut self,
        view: ViewId,
        field: FieldId,
        items: impl IntoIterator<Item = (u64, T)>,
    ) where
        T: Clone + PartialEq + Send + Sync + 'static;
}

impl ListOutputExt for irrigation_runtime::general::GeneralCx {
    fn list_define_keyed<T>(
        &mut self,
        view: ViewId,
        field: FieldId,
        items: impl IntoIterator<Item = (u64, T)>,
    ) where
        T: Clone + PartialEq + Send + Sync + 'static,
    {
        // The handle publisher claims this field through `write_group`; a
        // general component has to claim it explicitly, or two publishers of one
        // field race without arbitration. The tree publisher does the same.
        self.claim_scope(
            irrigation_core::descriptor::WriteScope::Field(view, field),
            "list_output_keyed",
        );
        let items: PSeq<(u64, T)> = items.into_iter().collect();
        let ids: PSeq<u64> = items.iter().map(|(id, _)| *id).collect();
        for (id, value) in items.iter() {
            self.define(
                UnitKey::entity(view, field, UnitKind::Value, any_key(*id)),
                value.clone(),
                "output.list.value",
            );
        }
        self.define(
            UnitKey::field(view, field, UnitKind::Value),
            DerivedListStorage::from_keyed(items.iter().cloned()),
            "output.list.content",
        );
        // Staged after the whole-field value: a field-level `Value` definition
        // declares the whole-value protocol, and a field-level `Structure`
        // definition declares the keyed one.
        self.define(
            UnitKey::field(view, field, UnitKind::Structure),
            ids,
            "output.list.structure",
        );
    }
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
    fn derived_list_unit_eq_compares_one_identity() {
        let view = ViewId(1);
        let field = FieldId(0);
        let value = UnitKey::entity(view, field, UnitKind::Value, any_key(1u64));
        let membership = UnitKey::entity(view, field, UnitKind::Membership, any_key(1u64));
        let old = DerivedListStorage::from_keyed([(1u64, 10u32), (2, 20)]);
        let same = DerivedListStorage::from_keyed([(1u64, 10u32), (2, 20)]);
        let next = DerivedListStorage::from_keyed([(1u64, 11u32), (2, 20)]);
        assert_eq!(
            <DerivedList<u32> as ViewField>::unit_eq(&old, &same, &value),
            Some(true)
        );
        assert_eq!(
            <DerivedList<u32> as ViewField>::unit_eq(&old, &next, &value),
            Some(false)
        );
        assert_eq!(
            <DerivedList<u32> as ViewField>::unit_eq(&old, &next, &membership),
            Some(true)
        );
    }

    #[test]
    fn derived_list_define_many_batches_payload_identities() {
        let storage = <DerivedList<u32> as ViewField>::empty();
        let view = ViewId(1);
        let field = FieldId(0);
        let drafts = vec![
            (
                UnitKey::entity(view, field, UnitKind::Value, any_key(2u64)),
                value_of(20u32),
            ),
            (
                UnitKey::entity(view, field, UnitKind::Value, any_key(1u64)),
                value_of(10u32),
            ),
        ];
        let storage = <DerivedList<u32> as ViewField>::define_many(&storage, &drafts);
        assert_eq!(storage.ids().to_vec(), vec![2, 1]);
        assert_eq!(storage.get(1), Some(&10));
        assert_eq!(storage.to_vec(), vec![20, 10]);
    }

    #[test]
    fn keyed_list_storage_is_a_field_store() {
        fn assert_field_store<T: irrigation_store::delta::FieldStore>() {}
        assert_field_store::<DerivedListStorage<u32>>();
    }

    #[test]
    fn keyed_list_delta_reports_identity_moves() {
        let storage = DerivedListStorage::from_keyed([(1u64, 10u32), (2, 20)]);
        let delta = KeyedListDelta::from_edits(vec![
            KeyedListEdit::Set { id: 1, value: 11 },
            KeyedListEdit::Set { id: 2, value: 20 },
            KeyedListEdit::Set { id: 3, value: 30 },
            KeyedListEdit::Remove { id: 9 },
        ]);
        let (storage, applied) = storage.apply_delta(&delta);
        assert_eq!(applied.changed, vec![1, 3]);
        assert!(applied.removed.is_empty());
        assert_eq!(storage.get(1), Some(&11));
        assert_eq!(storage.get(3), Some(&30));

        let delta = KeyedListDelta::from_edits(vec![KeyedListEdit::Remove { id: 1 }]);
        let (storage, applied) = storage.apply_delta(&delta);
        assert_eq!(applied.removed, vec![1]);
        assert_eq!(storage.get(1), None);
    }

    #[test]
    fn keyed_storage_separates_order_from_payload_index() {
        let storage = DerivedListStorage::from_keyed((0..10_000u64).map(|id| (id, id * 2)));
        assert_eq!(storage.len(), 10_000);
        assert_eq!(storage.get(4_999), Some(&9_998));

        let updated = storage.with_entity(4_999, 7);
        assert_eq!(updated.get(4_999), Some(&7));
        assert_eq!(updated.ids().get(4_999), Some(&4_999));
        assert_eq!(updated.len(), 10_000);

        let removed = updated.without_entity(4_999);
        assert_eq!(removed.get(4_999), None);
        assert_eq!(removed.len(), 9_999);

        let reordered = removed.reorder(&[3u64, 1, 2].into_iter().collect());
        assert_eq!(reordered.ids().to_vec(), vec![3, 1, 2]);
        assert_eq!(reordered.to_vec(), vec![6, 2, 4]);
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
            value_of(DerivedListStorage::from_keyed([(2u64, 20u32), (1, 10)])),
        );
        assert_eq!(storage.to_vec(), vec![20, 10]);
        assert_eq!(storage.ids().to_vec(), vec![2, 1]);
        assert_eq!(storage.get(1), Some(&10));
        let structure = <DerivedList<u32> as ViewField>::structure(&storage).expect("structure");
        let ids = structure.value.downcast_ref::<PSeq<u64>>().expect("ids");
        assert_eq!(ids.to_vec(), vec![2, 1]);
    }
}
