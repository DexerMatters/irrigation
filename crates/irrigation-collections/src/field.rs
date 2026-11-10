//! Typed View field handles.
//!
//! A `#[view]` declaration knows every field's name, position, family, and
//! storage. The macro emits one [`Field`] constant per field so user code can
//! name the field instead of reconstructing a `UnitKey`:
//!
//! ```ignore
//! let document = SourceFile::DOCUMENT.read_in(cx)?;
//! cx.write(Tokens::PAYLOADS).entry(token_id).set(payload);
//! ```
//!
//! Reads come in two forms. A bare verb (`read`, `get`, `to_vec`) is a tracked
//! read in the ambient computation frame, exactly like the generated accessors.
//! A `_in` verb takes a [`GeneralCx`] explicitly. Writes always take a
//! `GeneralCx` and stage through its dispatcher-issued output transaction, so
//! output-view authorization and atomic commit are unchanged.

use std::ops::Deref;

use irrigation_core::descriptor::{UnitKey, UnitKind, ViewDescriptor};
use irrigation_core::error::Result;
use irrigation_core::ids::{FieldId, ViewId};
use irrigation_core::key::any_key;
use irrigation_runtime::general::{FieldWrite, GeneralCx};
use irrigation_runtime::helpers::ensure_view;
use irrigation_runtime::runtime::{Runtime, Subscription};
use irrigation_runtime::snapshot::Snapshot;

use crate::context::Context;
use crate::graph::{EdgeKey, EdgePayload, GraphRef, GraphTopology, NodeKey};
use crate::list::{DerivedListRef, DerivedListStorage, SourceListRef};
use crate::map::MapRef;
use crate::reduce::ReducedRef;
use crate::set::{DerivedSetRef, DerivedSetStorage, SourceSetRef};

/// One declared View field: its descriptor, its position, and a typed handle.
#[derive(Debug)]
pub struct Field<H> {
    descriptor: &'static ViewDescriptor,
    id: FieldId,
    handle: H,
}

impl<H: Copy> Copy for Field<H> {}

impl<H: Copy> Clone for Field<H> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<H> Field<H> {
    /// Wraps a family handle with the declaration it belongs to.
    #[must_use]
    pub const fn new(descriptor: &'static ViewDescriptor, id: FieldId, handle: H) -> Self {
        Self {
            descriptor,
            id,
            handle,
        }
    }

    /// The View namespace.
    #[must_use]
    pub const fn view(&self) -> ViewId {
        self.descriptor.id
    }

    /// The field's position in the declaration.
    #[must_use]
    pub const fn id(&self) -> FieldId {
        self.id
    }

    /// The View descriptor.
    #[must_use]
    pub const fn descriptor(&self) -> &'static ViewDescriptor {
        self.descriptor
    }

    /// The entity-free `Value` unit of this field.
    ///
    /// Scalar fields use this as their only unit; collection fields use it for
    /// a whole-field definition.
    #[must_use]
    pub fn unit(&self) -> UnitKey {
        UnitKey::field(self.view(), self.id(), UnitKind::Value)
    }

    /// The field's structure projection unit.
    #[must_use]
    pub fn structure_unit(&self) -> UnitKey {
        UnitKey::field(self.view(), self.id(), UnitKind::Structure)
    }

    /// The membership unit of one member of a collection field.
    ///
    /// This is the unit a set's `contains` answer is published on: presence, with no
    /// payload. A caller building it by hand has to name `UnitKind::Membership` and
    /// carry the entity, so the two things that make it correct were spelled at the call
    /// site rather than taken from the field.
    #[must_use]
    pub fn member_unit(&self, member: std::sync::Arc<dyn irrigation_core::key::AnyKey>) -> UnitKey {
        UnitKey::entity(self.view(), self.id(), UnitKind::Membership, member)
    }

    /// The entity-bearing `Value` unit of one entry of a collection field.
    ///
    /// The `Arc<dyn AnyKey>` is not a type-erased handle: it carries the concrete key, and
    /// equality on it verifies the real type, so `any_key(7u64)` and `any_key(7u32)` are
    /// different entries. The runtime's own table keys are built the same way, which is why
    /// [`Self::member_unit`] can be asserted equal to a hand-built entity unit.
    ///
    /// A map or graph publishes one such unit per entry; the whole-field [`Self::unit`]
    /// is what a whole-field definition uses instead.
    #[must_use]
    pub fn entry_unit(&self, entry: std::sync::Arc<dyn irrigation_core::key::AnyKey>) -> UnitKey {
        UnitKey::entity(self.view(), self.id(), UnitKind::Value, entry)
    }

    /// The underlying family handle.
    #[must_use]
    pub const fn handle(&self) -> &H {
        &self.handle
    }

    fn ensure_registered(&self) {
        ensure_view(self.descriptor);
    }

    /// Declares this field's View for the current computation.
    ///
    /// Generated accessors and writers call this automatically. The escape
    /// hatch exists for family handles whose definition verbs are borrowed
    /// directly (a graph's `define_node`, for example).
    pub fn ensure(&self) {
        self.ensure_registered();
    }
}

impl<H> Deref for Field<H> {
    type Target = H;

    fn deref(&self) -> &Self::Target {
        &self.handle
    }
}

/// A scalar field handle.
pub type ScalarField<T> = Field<irrigation_runtime::field::ScalarRef<T>>;
/// A map field handle.
pub type MapField<K, V> = Field<MapRef<K, V>>;
/// A source list field handle.
pub type SourceListField<T> = Field<SourceListRef<T>>;
/// A derived list field handle.
pub type DerivedListField<T> = Field<DerivedListRef<T>>;
/// A source set field handle.
pub type SourceSetField<T> = Field<SourceSetRef<T>>;
/// A derived set field handle.
pub type DerivedSetField<T> = Field<DerivedSetRef<T>>;
/// A graph field handle.
pub type GraphField<N, E> = Field<GraphRef<N, E>>;
/// A reduced sequence field handle.
pub type ReducedField<R> = Field<ReducedRef<R>>;

impl<T> Field<irrigation_runtime::field::ScalarRef<T>>
where
    T: Clone + PartialEq + Send + Sync + 'static,
{
    /// Reads the scalar in the ambient computation frame.
    #[must_use]
    pub fn read(&self) -> T {
        self.ensure_registered();
        self.handle.read()
    }

    /// Reads the scalar through an explicit general-component context.
    ///
    /// # Errors
    ///
    /// Fails when the unit is missing or holds another type.
    pub fn read_in(&self, cx: &mut GeneralCx) -> Result<T> {
        self.ensure_registered();
        cx.read_typed(self.unit())
    }

    /// Observes the scalar unit.
    ///
    /// # Errors
    ///
    /// Fails when the View cannot be registered.
    pub fn observe(
        &self,
        runtime: &Runtime,
        callback: impl Fn(&Snapshot) + Send + Sync + 'static,
    ) -> irrigation_core::error::Result<Subscription> {
        runtime.observe_units_of(self.descriptor, vec![self.unit()], callback)
    }

    /// Starts a scalar output write.
    pub fn write<'a>(&self, cx: &'a mut GeneralCx) -> ScalarWriter<'a, T> {
        self.ensure_registered();
        ScalarWriter {
            cx,
            view: self.view(),
            field: self.id(),
            marker: std::marker::PhantomData,
        }
    }
}

impl<T> FieldWrite for Field<irrigation_runtime::field::ScalarRef<T>>
where
    T: PartialEq + Send + Sync + 'static,
{
    type Writer<'a> = ScalarWriter<'a, T>;

    fn open<'a>(self, cx: &'a mut GeneralCx) -> Self::Writer<'a> {
        self.ensure_registered();
        ScalarWriter {
            cx,
            view: self.view(),
            field: self.id(),
            marker: std::marker::PhantomData,
        }
    }
}

/// A scalar output writer.
pub struct ScalarWriter<'a, T> {
    cx: &'a mut GeneralCx,
    view: ViewId,
    field: FieldId,
    marker: std::marker::PhantomData<fn() -> T>,
}

impl<T> std::fmt::Debug for ScalarWriter<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ScalarWriter")
    }
}

impl<T: PartialEq + Send + Sync + 'static> ScalarWriter<'_, T> {
    /// Replaces the scalar's value.
    pub fn set(self, value: T) {
        self.cx.output().field(self.view, self.field, value);
    }
}

impl<K, V> Field<MapRef<K, V>>
where
    K: Ord + std::hash::Hash + Clone + Send + Sync + std::fmt::Debug + 'static,
    V: Clone + PartialEq + Send + Sync + 'static,
{
    /// A handle for one map entry.
    #[must_use]
    pub fn entry(&self, key: K) -> MapEntry<K, V> {
        MapEntry {
            map: self.handle,
            key,
            descriptor: self.descriptor,
        }
    }

    /// Reads one entry in the ambient frame.
    #[must_use]
    pub fn get(&self, key: &K) -> Option<V> {
        self.handle.get(key)
    }

    /// Reads one entry through an explicit context.
    ///
    /// # Errors
    ///
    /// Fails when a unit is missing or mistyped.
    pub fn get_in(&self, cx: &mut GeneralCx, key: &K) -> Result<Option<V>> {
        self.entry(key.clone()).read_in(cx)
    }

    /// Reads every entry through an explicit context.
    ///
    /// # Errors
    ///
    /// Fails when the structure or an entry is missing or mistyped.
    pub fn entries_in(&self, cx: &mut GeneralCx) -> Result<Vec<(K, V)>> {
        self.ensure_registered();
        let keys: irrigation_store::persistent::PMap<K, ()> =
            cx.read_typed(self.structure_unit())?;
        let mut entries = Vec::with_capacity(keys.len());
        for (key, ()) in keys.iter() {
            if let Some(value) = self.entry(key.clone()).read_in(cx)? {
                entries.push((key.clone(), value));
            }
        }
        Ok(entries)
    }

    /// Observes the map's key structure.
    ///
    /// # Errors
    ///
    /// Fails when the View cannot be registered.
    pub fn observe(
        &self,
        runtime: &Runtime,
        callback: impl Fn(&Snapshot) + Send + Sync + 'static,
    ) -> irrigation_core::error::Result<Subscription> {
        runtime.observe_units_of(self.descriptor, vec![self.structure_unit()], callback)
    }

    /// Observes one entry's membership and value units.
    ///
    /// # Errors
    ///
    /// Fails when the View cannot be registered.
    pub fn observe_entry(
        &self,
        runtime: &Runtime,
        key: K,
        callback: impl Fn(&Snapshot) + Send + Sync + 'static,
    ) -> irrigation_core::error::Result<Subscription> {
        self.entry(key).observe(runtime, callback)
    }

    /// Starts a map output write.
    pub fn write<'a>(&self, cx: &'a mut GeneralCx) -> MapWriter<'a, K, V> {
        self.ensure_registered();
        MapWriter {
            cx,
            view: self.view(),
            field: self.id(),
            marker: std::marker::PhantomData,
        }
    }
}

/// A handle for one map entry.
#[derive(Clone)]
pub struct MapEntry<K, V> {
    map: MapRef<K, V>,
    key: K,
    descriptor: &'static ViewDescriptor,
}

impl<K: PartialEq, V> PartialEq for MapEntry<K, V> {
    fn eq(&self, other: &Self) -> bool {
        self.map == other.map && self.key == other.key
    }
}

impl<K: Eq, V> Eq for MapEntry<K, V> {}

impl<K, V> std::fmt::Debug for MapEntry<K, V>
where
    K: std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("MapEntry").field(&self.key).finish()
    }
}

impl<K, V> MapEntry<K, V>
where
    K: Ord + std::hash::Hash + Clone + Send + Sync + std::fmt::Debug + 'static,
    V: Clone + PartialEq + Send + Sync + 'static,
{
    /// The key this entry names.
    #[must_use]
    pub fn key(&self) -> &K {
        &self.key
    }

    /// The entry's value unit.
    #[must_use]
    pub fn value_unit(&self) -> UnitKey {
        UnitKey::entity(
            self.map.view_id(),
            self.map.field_id(),
            UnitKind::Value,
            any_key(self.key.clone()),
        )
    }

    /// The entry's membership unit.
    #[must_use]
    pub fn membership_unit(&self) -> UnitKey {
        UnitKey::entity(
            self.map.view_id(),
            self.map.field_id(),
            UnitKind::Membership,
            any_key(self.key.clone()),
        )
    }

    /// Reads the entry in the ambient frame.
    #[must_use]
    pub fn read(&self) -> Option<V> {
        self.map.get(&self.key)
    }

    /// Reads the entry through an explicit context.
    ///
    /// # Errors
    ///
    /// Fails when a unit is missing or mistyped.
    pub fn read_in(&self, cx: &mut GeneralCx) -> Result<Option<V>> {
        let present: bool = cx.read_typed(self.membership_unit())?;
        if !present {
            return Ok(None);
        }
        cx.read_typed(self.value_unit()).map(Some)
    }

    /// A tracked context argument whose invocation key is this entry's key.
    #[must_use]
    pub fn context(&self) -> Context<K, V> {
        Context::new(self.key.clone(), self.value_unit())
    }

    /// Observes this entry's membership and value units.
    ///
    /// # Errors
    ///
    /// Fails when the View cannot be registered.
    pub fn observe(
        &self,
        runtime: &Runtime,
        callback: impl Fn(&Snapshot) + Send + Sync + 'static,
    ) -> irrigation_core::error::Result<Subscription> {
        runtime.observe_units_of(
            self.descriptor,
            vec![self.membership_unit(), self.value_unit()],
            callback,
        )
    }

    /// Re-observes both entry units without reading their payloads.
    ///
    /// # Errors
    ///
    /// Fails when a unit is missing or not certified.
    pub fn observe_in(&self, cx: &mut GeneralCx) -> Result<()> {
        cx.observe(self.membership_unit())?;
        cx.observe(self.value_unit())?;
        Ok(())
    }

    /// Writes a value and marks the entry present.
    pub fn set(self, cx: &mut GeneralCx, value: V) {
        let mut output = cx.output();
        output.define(self.value_unit(), value);
        output.define(self.membership_unit(), true);
    }

    /// Sets membership without touching the payload.
    pub fn present(self, cx: &mut GeneralCx, present: bool) {
        cx.output().define(self.membership_unit(), present);
    }

    /// Removes the entry.
    pub fn retract(self, cx: &mut GeneralCx) {
        let mut output = cx.output();
        output.retract(self.value_unit());
        output.define(self.membership_unit(), false);
    }
}

impl<K, V> FieldWrite for Field<MapRef<K, V>>
where
    K: Ord + std::hash::Hash + Clone + Send + Sync + std::fmt::Debug + 'static,
    V: Clone + PartialEq + Send + Sync + 'static,
{
    type Writer<'a> = MapWriter<'a, K, V>;

    fn open<'a>(self, cx: &'a mut GeneralCx) -> Self::Writer<'a> {
        self.ensure_registered();
        MapWriter {
            cx,
            view: self.view(),
            field: self.id(),
            marker: std::marker::PhantomData,
        }
    }
}

/// A map output writer.
pub struct MapWriter<'a, K, V> {
    cx: &'a mut GeneralCx,
    view: ViewId,
    field: FieldId,
    marker: std::marker::PhantomData<fn() -> (K, V)>,
}

impl<K, V> std::fmt::Debug for MapWriter<'_, K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MapWriter")
    }
}

impl<K, V> MapWriter<'_, K, V>
where
    K: Ord + std::hash::Hash + Clone + Send + Sync + std::fmt::Debug + 'static,
    V: Clone + PartialEq + Send + Sync + 'static,
{
    /// A writer for one entry.
    pub fn entry(&mut self, key: K) -> MapEntryWriter<'_, K, V> {
        MapEntryWriter {
            cx: self.cx,
            view: self.view,
            field: self.field,
            key,
            marker: std::marker::PhantomData,
        }
    }

    /// Defines every entry of an iterator.
    pub fn define_all(&mut self, entries: impl IntoIterator<Item = (K, V)>) {
        for (key, value) in entries {
            self.entry(key).set(value);
        }
    }
}

/// A writer for one map entry.
pub struct MapEntryWriter<'a, K, V> {
    cx: &'a mut GeneralCx,
    view: ViewId,
    field: FieldId,
    key: K,
    marker: std::marker::PhantomData<fn() -> V>,
}

impl<K, V> std::fmt::Debug for MapEntryWriter<'_, K, V>
where
    K: std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MapEntryWriter")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl<K, V> MapEntryWriter<'_, K, V>
where
    K: Ord + std::hash::Hash + Clone + Send + Sync + std::fmt::Debug + 'static,
    V: PartialEq + Send + Sync + 'static,
{
    fn value_unit(&self) -> UnitKey {
        UnitKey::entity(
            self.view,
            self.field,
            UnitKind::Value,
            any_key(self.key.clone()),
        )
    }

    fn membership_unit(&self) -> UnitKey {
        UnitKey::entity(
            self.view,
            self.field,
            UnitKind::Membership,
            any_key(self.key.clone()),
        )
    }

    /// Defines the payload and marks the entry present.
    pub fn set(self, value: V) {
        let value_unit = self.value_unit();
        let membership_unit = self.membership_unit();
        let mut output = self.cx.output();
        output.define(value_unit, value);
        output.define(membership_unit, true);
    }

    /// Sets membership only.
    pub fn present(self, present: bool) {
        let membership_unit = self.membership_unit();
        self.cx.output().define(membership_unit, present);
    }

    /// Retracts the payload and clears membership.
    pub fn retract(self) {
        let value_unit = self.value_unit();
        let membership_unit = self.membership_unit();
        let mut output = self.cx.output();
        output.retract(value_unit);
        output.define(membership_unit, false);
    }
}

impl<T> Field<SourceListRef<T>>
where
    T: Clone + PartialEq + Send + Sync + 'static,
{
    /// Observes the field's structure unit.
    ///
    /// # Errors
    ///
    /// Fails when the View cannot be registered.
    pub fn observe(
        &self,
        runtime: &Runtime,
        callback: impl Fn(&Snapshot) + Send + Sync + 'static,
    ) -> irrigation_core::error::Result<Subscription> {
        runtime.observe_units_of(self.descriptor, vec![self.structure_unit()], callback)
    }

    /// Reads the identity sequence through an explicit context.
    ///
    /// # Errors
    ///
    /// Fails when the structure unit is missing or mistyped.
    pub fn ids_in(&self, cx: &mut GeneralCx) -> Result<irrigation_store::persistent::PSeq<u64>> {
        self.ensure_registered();
        cx.read_typed(self.structure_unit())
    }

    /// Reads the whole list through an explicit context.
    ///
    /// # Errors
    ///
    /// Fails when the field value is missing or mistyped.
    pub fn to_vec_in(&self, cx: &mut GeneralCx) -> Result<Vec<T>> {
        self.ensure_registered();
        let storage: irrigation_store::persistent::PSeq<(u64, T)> = cx.read_typed(self.unit())?;
        Ok(storage.iter().map(|(_, value)| value.clone()).collect())
    }

    /// Reads the identity sequence in the ambient frame.
    #[must_use]
    pub fn ids(&self) -> irrigation_store::persistent::PSeq<u64> {
        self.handle.ids()
    }

    /// Reads every element in the ambient frame.
    #[must_use]
    pub fn to_vec(&self) -> Vec<T> {
        self.handle.to_vec()
    }

    /// Reads one element by position.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<T> {
        self.handle.get(index)
    }

    /// Folds every element in order.
    ///
    /// This is a whole-node fold: every member is read and observed, but the
    /// fold itself is not memoized per member. Use `map`/`filter` when
    /// per-member reuse matters.
    pub fn fold<A, F>(&self, init: A, f: F) -> A
    where
        F: Fn(A, T) -> A,
    {
        self.handle.to_vec().into_iter().fold(init, f)
    }

    /// Number of elements.
    #[must_use]
    pub fn len(&self) -> usize {
        self.handle.len()
    }

    /// Whether the list is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.handle.is_empty()
    }
}

impl<T> Field<DerivedListRef<T>>
where
    T: Clone + PartialEq + Send + Sync + 'static,
{
    /// Observes the field's structure unit.
    ///
    /// # Errors
    ///
    /// Fails when the View cannot be registered.
    pub fn observe(
        &self,
        runtime: &Runtime,
        callback: impl Fn(&Snapshot) + Send + Sync + 'static,
    ) -> irrigation_core::error::Result<Subscription> {
        runtime.observe_units_of(self.descriptor, vec![self.structure_unit()], callback)
    }

    /// Reads the identity sequence through an explicit context.
    ///
    /// # Errors
    ///
    /// Fails when the structure unit is missing or mistyped.
    pub fn ids_in(&self, cx: &mut GeneralCx) -> Result<irrigation_store::persistent::PSeq<u64>> {
        self.ensure_registered();
        cx.read_typed(self.structure_unit())
    }

    /// Reads the whole list through an explicit context.
    ///
    /// # Errors
    ///
    /// Fails when the content or a unit is missing or mistyped.
    pub fn to_vec_in(&self, cx: &mut GeneralCx) -> Result<Vec<T>> {
        self.ensure_registered();
        let storage: crate::list::DerivedListStorage<T> = cx.read_typed(self.unit())?;
        Ok(storage.to_vec())
    }

    /// Reads every element in the ambient frame.
    #[must_use]
    pub fn to_vec(&self) -> Vec<T> {
        self.handle.to_vec()
    }

    /// Reads one element by position.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<T> {
        self.handle.get(index)
    }

    /// Folds every element in order.
    pub fn fold<A, F>(&self, init: A, f: F) -> A
    where
        F: Fn(A, T) -> A,
    {
        self.handle.to_vec().into_iter().fold(init, f)
    }

    /// Number of elements.
    #[must_use]
    pub fn len(&self) -> usize {
        self.handle.len()
    }

    /// Whether the list is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.handle.is_empty()
    }
}

impl<T> Field<SourceSetRef<T>>
where
    T: Ord + std::hash::Hash + Clone + Send + Sync + std::fmt::Debug + 'static,
{
    /// Observes the field's structure unit.
    ///
    /// # Errors
    ///
    /// Fails when the View cannot be registered.
    pub fn observe(
        &self,
        runtime: &Runtime,
        callback: impl Fn(&Snapshot) + Send + Sync + 'static,
    ) -> irrigation_core::error::Result<Subscription> {
        runtime.observe_units_of(self.descriptor, vec![self.structure_unit()], callback)
    }

    /// Whether a member is present.
    #[must_use]
    pub fn contains(&self, value: &T) -> bool {
        self.handle.contains(value)
    }

    /// Reads the member set through an explicit context.
    ///
    /// # Errors
    ///
    /// Fails when the structure unit is missing or mistyped.
    pub fn members_in(&self, cx: &mut GeneralCx) -> Result<irrigation_store::persistent::PSet<T>> {
        self.ensure_registered();
        cx.read_typed(self.structure_unit())
    }

    /// Whether a member is present, through an explicit context.
    ///
    /// # Errors
    ///
    /// Fails when the membership unit is missing or mistyped.
    pub fn contains_in(&self, cx: &mut GeneralCx, value: &T) -> Result<bool> {
        self.ensure_registered();
        let key = UnitKey::entity(
            self.view(),
            self.id(),
            UnitKind::Membership,
            any_key(value.clone()),
        );
        cx.read_typed(key)
    }

    /// Reads every member.
    #[must_use]
    pub fn to_vec(&self) -> Vec<T> {
        self.handle.to_vec()
    }

    /// Number of members.
    #[must_use]
    pub fn len(&self) -> usize {
        self.handle.len()
    }

    /// Whether the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.handle.is_empty()
    }
}

impl<T> Field<DerivedSetRef<T>>
where
    T: Ord + std::hash::Hash + Clone + Send + Sync + std::fmt::Debug + 'static,
{
    /// Observes the field's structure unit.
    ///
    /// # Errors
    ///
    /// Fails when the View cannot be registered.
    pub fn observe(
        &self,
        runtime: &Runtime,
        callback: impl Fn(&Snapshot) + Send + Sync + 'static,
    ) -> irrigation_core::error::Result<Subscription> {
        runtime.observe_units_of(self.descriptor, vec![self.structure_unit()], callback)
    }

    /// Reads the member set through an explicit context.
    ///
    /// # Errors
    ///
    /// Fails when the structure unit is missing or mistyped.
    pub fn members_in(&self, cx: &mut GeneralCx) -> Result<irrigation_store::persistent::PSet<T>> {
        self.ensure_registered();
        cx.read_typed(self.structure_unit())
    }

    /// Whether a member is present, through an explicit context.
    ///
    /// # Errors
    ///
    /// Fails when the membership unit is missing or mistyped.
    pub fn contains_in(&self, cx: &mut GeneralCx, value: &T) -> Result<bool> {
        self.ensure_registered();
        let key = UnitKey::entity(
            self.view(),
            self.id(),
            UnitKind::Membership,
            any_key(value.clone()),
        );
        cx.read_typed(key)
    }

    /// Whether a member is present.
    #[must_use]
    pub fn contains(&self, value: &T) -> bool {
        self.handle.contains(value)
    }

    /// Reads every member.
    #[must_use]
    pub fn to_vec(&self) -> Vec<T> {
        self.handle.to_vec()
    }
}

impl<N, E> Field<GraphRef<N, E>>
where
    N: Clone + PartialEq + Send + Sync + 'static,
    E: Clone + PartialEq + Send + Sync + 'static,
{
    /// Observes the field's structure unit.
    ///
    /// # Errors
    ///
    /// Fails when the View cannot be registered.
    pub fn observe(
        &self,
        runtime: &Runtime,
        callback: impl Fn(&Snapshot) + Send + Sync + 'static,
    ) -> irrigation_core::error::Result<Subscription> {
        runtime.observe_units_of(self.descriptor, vec![self.structure_unit()], callback)
    }

    /// Node identities in order.
    #[must_use]
    pub fn nodes(&self) -> Vec<u64> {
        self.handle.nodes()
    }

    /// Edge identities in order.
    #[must_use]
    pub fn edges(&self) -> Vec<u64> {
        self.handle.edges()
    }

    /// Reads one node payload.
    #[must_use]
    pub fn node(&self, id: u64) -> Option<N> {
        self.handle.node(id)
    }

    /// Reads one edge payload.
    #[must_use]
    pub fn edge(&self, id: u64) -> Option<crate::graph::EdgePayload<E>> {
        self.handle.edge(id)
    }
}

impl<R: crate::reduce::Reduction> Field<ReducedRef<R>> {
    /// Observes the field's structure unit.
    ///
    /// # Errors
    ///
    /// Fails when the View cannot be registered.
    pub fn observe(
        &self,
        runtime: &Runtime,
        callback: impl Fn(&Snapshot) + Send + Sync + 'static,
    ) -> irrigation_core::error::Result<Subscription> {
        runtime.observe_units_of(self.descriptor, vec![self.structure_unit()], callback)
    }

    /// The cached aggregate.
    #[must_use]
    pub fn aggregate(&self) -> R::Aggregate {
        self.handle.aggregate()
    }

    /// Number of elements.
    #[must_use]
    pub fn len(&self) -> usize {
        self.handle.len()
    }

    /// Whether the sequence is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.handle.is_empty()
    }
}

impl<T> FieldWrite for Field<DerivedListRef<T>>
where
    T: Clone + PartialEq + Send + Sync + 'static,
{
    type Writer<'a> = DerivedListWriter<'a, T>;

    fn open<'a>(self, cx: &'a mut GeneralCx) -> Self::Writer<'a> {
        self.ensure_registered();
        DerivedListWriter {
            cx,
            view: self.view(),
            field: self.id(),
            marker: std::marker::PhantomData,
        }
    }
}

/// A writer for a derived list field.
pub struct DerivedListWriter<'a, T> {
    cx: &'a mut GeneralCx,
    view: ViewId,
    field: FieldId,
    marker: std::marker::PhantomData<fn() -> T>,
}

impl<T> std::fmt::Debug for DerivedListWriter<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DerivedListWriter")
    }
}

impl<T> DerivedListWriter<'_, T>
where
    T: Clone + PartialEq + Send + Sync + 'static,
{
    /// Defines the list as one whole value.
    pub fn define_all(self, items: impl IntoIterator<Item = T>) {
        let list: irrigation_store::persistent::PSeq<T> = items.into_iter().collect();
        self.cx.output().define(
            UnitKey::field(self.view, self.field, UnitKind::Value),
            DerivedListStorage::Whole(list),
        );
    }

    /// Defines a keyed list: stable identities, payloads, and structure.
    pub fn define_keyed(self, items: impl IntoIterator<Item = (u64, T)>) {
        let items: Vec<(u64, T)> = items.into_iter().collect();
        let ids: irrigation_store::persistent::PSeq<u64> =
            items.iter().map(|(id, _)| *id).collect();
        let mut output = self.cx.output();
        for (id, value) in &items {
            output.define(
                UnitKey::entity(self.view, self.field, UnitKind::Value, any_key(*id)),
                value.clone(),
            );
            output.define(
                UnitKey::entity(self.view, self.field, UnitKind::Membership, any_key(*id)),
                true,
            );
        }
        output.define(
            UnitKey::field(self.view, self.field, UnitKind::Value),
            DerivedListStorage::from_keyed(items.iter().cloned()),
        );
        output.define(
            UnitKey::field(self.view, self.field, UnitKind::Structure),
            ids,
        );
    }
}

impl<T> FieldWrite for Field<DerivedSetRef<T>>
where
    T: Ord + std::hash::Hash + Clone + Send + Sync + std::fmt::Debug + 'static,
{
    type Writer<'a> = DerivedSetWriter<'a, T>;

    fn open<'a>(self, cx: &'a mut GeneralCx) -> Self::Writer<'a> {
        self.ensure_registered();
        DerivedSetWriter {
            cx,
            view: self.view(),
            field: self.id(),
            marker: std::marker::PhantomData,
        }
    }
}

/// A writer for a derived set field.
pub struct DerivedSetWriter<'a, T> {
    cx: &'a mut GeneralCx,
    view: ViewId,
    field: FieldId,
    marker: std::marker::PhantomData<fn() -> T>,
}

impl<T> std::fmt::Debug for DerivedSetWriter<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DerivedSetWriter")
    }
}

impl<T> DerivedSetWriter<'_, T>
where
    T: Ord + std::hash::Hash + Clone + Send + Sync + std::fmt::Debug + 'static,
{
    /// Defines the set as one whole value.
    pub fn define_all(self, members: impl IntoIterator<Item = T>) {
        let set: irrigation_store::persistent::PSet<T> = members.into_iter().collect();
        self.cx.output().define(
            UnitKey::field(self.view, self.field, UnitKind::Value),
            DerivedSetStorage::Whole(set),
        );
    }

    /// Defines a keyed set: one membership unit per member plus structure.
    pub fn define_keyed(self, members: impl IntoIterator<Item = T>) {
        let members: Vec<T> = members.into_iter().collect();
        let structure: irrigation_store::persistent::PSet<T> = members.iter().cloned().collect();
        let mut output = self.cx.output();
        output.define(
            UnitKey::field(self.view, self.field, UnitKind::Value),
            DerivedSetStorage::Keyed(structure.clone()),
        );
        for member in &members {
            output.define(
                UnitKey::entity(
                    self.view,
                    self.field,
                    UnitKind::Membership,
                    any_key(member.clone()),
                ),
                true,
            );
        }
        output.define(
            UnitKey::field(self.view, self.field, UnitKind::Structure),
            structure,
        );
    }

    /// A writer for one member's membership.
    pub fn member(&mut self, member: T) -> SetMemberWriter<'_, T> {
        SetMemberWriter {
            cx: self.cx,
            view: self.view,
            field: self.field,
            member,
        }
    }
}

/// A writer for one set member.
pub struct SetMemberWriter<'a, T> {
    cx: &'a mut GeneralCx,
    view: ViewId,
    field: FieldId,
    member: T,
}

impl<T> std::fmt::Debug for SetMemberWriter<'_, T>
where
    T: std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SetMemberWriter")
            .field("member", &self.member)
            .finish_non_exhaustive()
    }
}

impl<T> SetMemberWriter<'_, T>
where
    T: Ord + std::hash::Hash + Clone + Send + Sync + std::fmt::Debug + 'static,
{
    /// Sets membership for this member.
    pub fn present(self, present: bool) {
        self.cx.output().define(
            UnitKey::entity(
                self.view,
                self.field,
                UnitKind::Membership,
                any_key(self.member),
            ),
            present,
        );
    }
}

impl<N, E> FieldWrite for Field<GraphRef<N, E>>
where
    N: Clone + PartialEq + Send + Sync + 'static,
    E: Clone + PartialEq + Send + Sync + 'static,
{
    type Writer<'a> = GraphWriter<'a, N, E>;

    fn open<'a>(self, cx: &'a mut GeneralCx) -> Self::Writer<'a> {
        self.ensure_registered();
        GraphWriter {
            cx,
            view: self.view(),
            field: self.id(),
            marker: std::marker::PhantomData,
        }
    }
}

/// A writer for a derived graph field.
pub struct GraphWriter<'a, N, E> {
    cx: &'a mut GeneralCx,
    view: ViewId,
    field: FieldId,
    marker: std::marker::PhantomData<fn() -> (N, E)>,
}

impl<N, E> std::fmt::Debug for GraphWriter<'_, N, E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GraphWriter")
    }
}

fn node_unit(view: ViewId, field: FieldId, node: u64, kind: UnitKind) -> UnitKey {
    UnitKey::entity(view, field, kind, any_key(NodeKey(node)))
}

fn edge_unit(view: ViewId, field: FieldId, edge: u64, kind: UnitKind) -> UnitKey {
    UnitKey::entity(view, field, kind, any_key(EdgeKey(edge)))
}

impl<N, E> GraphWriter<'_, N, E>
where
    N: Clone + PartialEq + Send + Sync + 'static,
    E: Clone + PartialEq + Send + Sync + 'static,
{
    /// Defines a whole graph: nodes, edges, and topology.
    pub fn define_graph(self, nodes: &[(u64, N)], edges: &[(u64, u64, u64, E)]) {
        let mut topology = GraphTopology::default();
        let mut output = self.cx.output();
        for (id, payload) in nodes {
            topology.nodes = topology.nodes.insert(*id).0;
            output.define(
                node_unit(self.view, self.field, *id, UnitKind::Value),
                payload.clone(),
            );
            output.define(
                node_unit(self.view, self.field, *id, UnitKind::Membership),
                true,
            );
        }
        for (id, from, to, payload) in edges {
            if topology.nodes.contains(from) && topology.nodes.contains(to) {
                topology.edges = topology.edges.insert(*id, (*from, *to)).0;
                output.define(
                    edge_unit(self.view, self.field, *id, UnitKind::Value),
                    Some(EdgePayload {
                        from: *from,
                        to: *to,
                        value: payload.clone(),
                    }),
                );
                output.define(
                    edge_unit(self.view, self.field, *id, UnitKind::Membership),
                    true,
                );
            }
        }
        output.define(
            UnitKey::field(self.view, self.field, UnitKind::Structure),
            topology,
        );
    }
}

impl<R: crate::reduce::Reduction> FieldWrite for Field<ReducedRef<R>> {
    type Writer<'a> = ReducedWriter<'a, R>;

    fn open<'a>(self, cx: &'a mut GeneralCx) -> Self::Writer<'a> {
        self.ensure_registered();
        ReducedWriter {
            cx,
            view: self.view(),
            field: self.id(),
            marker: std::marker::PhantomData,
        }
    }
}

/// A writer for a reduced sequence field.
pub struct ReducedWriter<'a, R: crate::reduce::Reduction> {
    cx: &'a mut GeneralCx,
    view: ViewId,
    field: FieldId,
    marker: std::marker::PhantomData<fn() -> R>,
}

impl<R: crate::reduce::Reduction> std::fmt::Debug for ReducedWriter<'_, R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReducedWriter")
    }
}

impl<R: crate::reduce::Reduction> ReducedWriter<'_, R> {
    /// Defines the whole reduced sequence.
    pub fn define_all(self, items: impl IntoIterator<Item = R::Item>) {
        let mut sequence = irrigation_store::AggSeq::new(R::leaf, R::combine, R::identity());
        for item in items {
            sequence = sequence.push(item);
        }
        self.cx.output().define(
            UnitKey::field(self.view, self.field, UnitKind::Value),
            sequence,
        );
    }
}

impl<H> Field<H> {
    /// Reads one unit through an explicit context.
    ///
    /// # Errors
    ///
    /// Fails when the unit is missing or holds another type.
    pub fn read_unit_in<T: Clone + Send + Sync + 'static>(
        &self,
        cx: &mut GeneralCx,
        unit: UnitKey,
    ) -> Result<T> {
        self.ensure_registered();
        cx.read_typed(unit)
    }
}
