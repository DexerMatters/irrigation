//! Incremental maps keyed by user values.
//!
//! A map entry has two independent observations: membership of its key and the
//! value stored there. Reading `map.get(k) == None` subscribes to the key's
//! membership cell, so inserting `k` wakes exactly that reader.

use std::any::Any;
use std::fmt::Debug;
use std::hash::Hash;
use std::marker::PhantomData;

use irrigation_core::descriptor::{FieldKind, UnitKey, UnitKind, WriteScope};
use irrigation_core::ids::{FieldId, SiteId, ViewId};
use irrigation_core::key::any_key;
use irrigation_core::value::{Value, value_of};
use irrigation_runtime::edit::Edit;
use irrigation_runtime::field::{ViewFieldHandle, structure_unit};
use irrigation_runtime::helpers::{
    PortPrecondition, define_unit, read_unit_value, retract_unit, try_read_unit_value, write_group,
};
use irrigation_store::delta::{AppliedDelta, DeltaStore, MapDelta, MapEdit};
use irrigation_store::field::ViewField;
use irrigation_store::persistent::PMap;

/// Map content: entries plus the key-set projection.
///
/// The projection is maintained on every entry write, so structure and length
/// reads never rebuild the key set from the whole map. Entry writes touch one
/// ordered-tree path in `entries` and one in `keys`.
#[derive(Clone)]
pub struct MapStorage<K, V> {
    entries: PMap<K, V>,
    keys: PMap<K, ()>,
}

impl<K, V> Default for MapStorage<K, V>
where
    K: Ord + Clone,
    V: Clone,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V> std::fmt::Debug for MapStorage<K, V>
where
    K: Ord + std::fmt::Debug,
    V: std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MapStorage")
            .field("entries", &self.entries)
            .field("keys", &self.keys)
            .finish()
    }
}

impl<K, V> PartialEq for MapStorage<K, V>
where
    K: Ord,
    V: PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries && self.keys == other.keys
    }
}

impl<K, V> Eq for MapStorage<K, V>
where
    K: Ord,
    V: Eq,
{
}

impl<K, V> From<PMap<K, V>> for MapStorage<K, V>
where
    K: Ord + Clone,
    V: Clone,
{
    fn from(entries: PMap<K, V>) -> Self {
        let keys = entries.iter().map(|(key, _)| (key.clone(), ())).collect();
        Self { entries, keys }
    }
}

impl<K, V> MapStorage<K, V>
where
    K: Ord + Clone,
    V: Clone,
{
    /// An empty map.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: PMap::new(),
            keys: PMap::new(),
        }
    }

    /// Builds a map from entries; the last value for a duplicate key wins.
    #[must_use]
    pub fn from_entries<I>(items: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
    {
        let mut storage = Self::new();
        for (key, value) in items {
            let (entries, _) = storage.entries.insert(key.clone(), value);
            let (keys, _) = storage.keys.insert(key, ());
            storage.entries = entries;
            storage.keys = keys;
        }
        storage
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the map is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Entries in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.entries.iter()
    }

    /// Reads one value.
    #[must_use]
    pub fn get(&self, key: &K) -> Option<&V> {
        self.entries.get(key)
    }

    /// Reports key presence.
    #[must_use]
    pub fn contains_key(&self, key: &K) -> bool {
        self.entries.contains_key(key)
    }

    /// Inserts or replaces one entry.
    #[must_use]
    pub fn insert(&self, key: K, value: V) -> (Self, Option<V>) {
        let (entries, previous) = self.entries.insert(key.clone(), value);
        let keys = if previous.is_none() {
            self.keys.insert(key, ()).0
        } else {
            self.keys.clone()
        };
        (Self { entries, keys }, previous)
    }

    /// Removes one entry.
    #[must_use]
    pub fn remove(&self, key: &K) -> (Self, Option<V>) {
        let (entries, previous) = self.entries.remove(key);
        if previous.is_none() {
            return (self.clone(), None);
        }
        let keys = self.keys.remove(key).0;
        (Self { entries, keys }, previous)
    }

    /// Applies a map delta in one batch.
    ///
    /// The returned `AppliedDelta` names the keys whose payload moved; an
    /// equal update and an already-absent removal contribute no change.
    #[must_use]
    pub fn apply_delta(&self, delta: &MapDelta<K, V>) -> (Self, AppliedDelta<K>)
    where
        V: PartialEq,
    {
        let mut next = self.clone();
        let mut applied = AppliedDelta::empty();
        for edit in delta.edits() {
            match edit {
                MapEdit::Insert { key, value } | MapEdit::Update { key, value } => {
                    let (storage, previous) = next.insert(key.clone(), value.clone());
                    next = storage;
                    if previous.as_ref() != Some(value) {
                        applied.changed.push(key.clone());
                        applied.content_changed = true;
                    }
                }
                MapEdit::Remove { key } => {
                    let (storage, previous) = next.remove(key);
                    next = storage;
                    if previous.is_some() {
                        applied.removed.push(key.clone());
                        applied.content_changed = true;
                    }
                }
            }
        }
        (next, applied)
    }

    /// The key-set projection.
    #[must_use]
    pub fn keys(&self) -> &PMap<K, ()> {
        &self.keys
    }
}

/// A keyed View field.
///
/// Keys must be totally ordered so iteration is deterministic; the persistent
/// representation is an ordered tree, which the plan accepts as the price of
/// stable ordering (`O(log n)` lookup rather than `O(1)`).
pub struct Map<K, V>(PhantomData<fn() -> (K, V)>);

impl<K, V> Debug for Map<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Map")
    }
}

impl<K, V> ViewField for Map<K, V>
where
    K: Ord + Hash + Clone + Send + Sync + Debug + 'static,
    V: Clone + PartialEq + Send + Sync + 'static,
{
    type Storage = MapStorage<K, V>;

    const KIND: FieldKind = FieldKind::Map;

    fn empty() -> Self::Storage {
        MapStorage::new()
    }

    fn len(storage: &Self::Storage) -> Option<usize> {
        Some(storage.len())
    }

    fn define(
        storage: &Self::Storage,
        key: &dyn irrigation_core::key::AnyKey,
        value: Value,
    ) -> Self::Storage {
        let Some(key) = key.as_any().downcast_ref::<K>() else {
            return storage.clone();
        };
        let Some(value) = value.downcast_ref::<V>() else {
            return storage.clone();
        };
        storage.insert(key.clone(), value.clone()).0
    }

    fn retract(storage: &Self::Storage, key: &dyn irrigation_core::key::AnyKey) -> Self::Storage {
        match key.as_any().downcast_ref::<K>() {
            Some(key) => storage.remove(key).0,
            None => storage.clone(),
        }
    }

    fn contains(storage: &Self::Storage, key: &dyn irrigation_core::key::AnyKey) -> bool {
        key.as_any()
            .downcast_ref::<K>()
            .is_some_and(|key| storage.contains_key(key))
    }

    fn structure(storage: &Self::Storage) -> Option<irrigation_core::descriptor::Structure> {
        Some(irrigation_core::descriptor::Structure {
            value: value_of(storage.keys().clone()),
            eq: |left, right| {
                left.downcast_ref::<PMap<K, ()>>() == right.downcast_ref::<PMap<K, ()>>()
            },
        })
    }

    fn get(storage: &Self::Storage, key: &dyn irrigation_core::key::AnyKey) -> Option<Value> {
        let key = key.as_any().downcast_ref::<K>()?;
        storage.get(key).map(|value| value_of(value.clone()))
    }

    fn define_many(storage: &Self::Storage, drafts: &[(UnitKey, Value)]) -> Self::Storage {
        let mut edits = Vec::new();
        for (key, value) in drafts {
            if key.kind == UnitKind::Membership && value.downcast_ref::<bool>() == Some(&false) {
                if let Some(entity) = key.entity.as_deref()
                    && let Some(key) = entity.as_any().downcast_ref::<K>()
                {
                    edits.push(MapEdit::Remove { key: key.clone() });
                }
                continue;
            }
            if key.kind != UnitKind::Value {
                continue;
            }
            let (Some(entity), Some(value)) = (key.entity.as_deref(), value.downcast_ref::<V>())
            else {
                continue;
            };
            let Some(key) = entity.as_any().downcast_ref::<K>() else {
                continue;
            };
            edits.push(MapEdit::Insert {
                key: key.clone(),
                value: value.clone(),
            });
        }
        if edits.is_empty() {
            return storage.clone();
        }
        let (next, _) = storage.apply_delta(&MapDelta::from_edits(edits));
        next
    }

    fn unit_eq(old: &Self::Storage, new: &Self::Storage, unit: &UnitKey) -> Option<bool> {
        if unit.entity.is_none() && unit.kind == UnitKind::Value {
            return Some(old == new);
        }
        if unit.kind == UnitKind::Structure {
            return Some(old.keys() == new.keys());
        }
        let entity = unit.entity.as_deref()?;
        let key = entity.as_any().downcast_ref::<K>()?;
        match unit.kind {
            UnitKind::Value => Some(old.get(key) == new.get(key)),
            UnitKind::Membership => Some(old.contains_key(key) == new.contains_key(key)),
            _ => None,
        }
    }

    fn unit_moves_content(unit: &UnitKey) -> bool {
        // Map content is the entries themselves. Membership and structure are
        // projections; only an entry payload defines content.
        unit.kind == UnitKind::Value && unit.entity.is_some()
    }
}

/// Tracked read handle for a map field.
pub struct MapRef<K, V> {
    view: ViewId,
    field: FieldId,
    content: fn(&dyn Any) -> MapStorage<K, V>,
}

impl<K, V> PartialEq for MapRef<K, V> {
    fn eq(&self, other: &Self) -> bool {
        self.view == other.view && self.field == other.field
    }
}

impl<K, V> Eq for MapRef<K, V> {}

impl<K, V> Debug for MapRef<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MapRef")
            .field("view", &self.view)
            .field("field", &self.field)
            .finish_non_exhaustive()
    }
}

impl<K, V> Copy for MapRef<K, V> {}

impl<K, V> Clone for MapRef<K, V> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K, V> MapRef<K, V>
where
    K: Ord + Hash + Clone + Send + Sync + Debug + 'static,
    V: Clone + PartialEq + Send + Sync + 'static,
{
    /// Builds a handle for one map field.
    #[must_use]
    pub const fn new(
        view: ViewId,
        field: FieldId,
        content: fn(&dyn Any) -> MapStorage<K, V>,
    ) -> Self {
        Self {
            view,
            field,
            content,
        }
    }

    fn membership_key(&self, key: &K) -> UnitKey {
        UnitKey::entity(
            self.view,
            self.field,
            UnitKind::Membership,
            any_key(key.clone()),
        )
    }

    fn value_key(&self, key: &K) -> UnitKey {
        UnitKey::entity(self.view, self.field, UnitKind::Value, any_key(key.clone()))
    }

    /// The View namespace this handle reads.
    #[must_use]
    pub fn view_id(&self) -> ViewId {
        self.view
    }

    /// The field position this handle reads.
    #[must_use]
    pub fn field_id(&self) -> FieldId {
        self.field
    }

    fn structure(&self) -> PMap<K, ()> {
        read_unit_value(structure_unit(self.view, self.field), |content| {
            Some((self.content)(content).keys().clone())
        })
    }

    /// Projects the map's committed entries.
    ///
    /// Untracked by design: a write verb's group replaces the units a tracked
    /// read would observe, so tracing it would invalidate the commit that
    /// installs the write.
    fn committed_entries(&self, content: &dyn Any) -> MapStorage<K, V> {
        (self.content)(content)
    }

    /// Reads the map's key set, recording one structural observation.
    #[must_use]
    pub fn keys(&self) -> Vec<K> {
        self.structure()
            .iter()
            .map(|(key, ())| key.clone())
            .collect()
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.structure().len()
    }

    /// Returns `true` when the map has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.structure().is_empty()
    }

    /// Returns `true` when the key is present.
    #[must_use]
    pub fn contains_key(&self, key: &K) -> bool {
        read_unit_value(self.membership_key(key), |content| {
            Some((self.content)(content).contains_key(key))
        })
    }

    /// Reads one entry, observing membership and value separately.
    ///
    /// The payload read is total: a member whose cells disagree — a
    /// membership-only draft, or a retraction between the two reads — is
    /// absent, not an engine failure.
    #[must_use]
    pub fn get(&self, key: &K) -> Option<V> {
        if !self.contains_key(key) {
            return None;
        }
        self.member_value(key)
    }

    /// Reads one member's payload cell, tolerating its absence.
    ///
    /// Only call this after [`Self::contains_key`] observed the membership, so
    /// a retraction between the two observations is recorded as a negative
    /// payload observation instead of aborting the demand.
    fn member_value(&self, key: &K) -> Option<V> {
        try_read_unit_value(self.value_key(key), |content| {
            (self.content)(content).get(key).cloned()
        })
    }

    /// Reads every entry in key order.
    #[must_use]
    pub fn to_vec(&self) -> Vec<(K, V)> {
        self.keys()
            .into_iter()
            .filter_map(|key| self.get(&key).map(|value| (key, value)))
            .collect()
    }

    /// Maps every entry through `f`, preserving per-key memoization.
    #[must_use]
    pub fn map_values<U, F>(&self, f: F) -> Vec<(K, U)>
    where
        U: Clone + PartialEq + Send + Sync + 'static,
        F: Fn(irrigation_runtime::Param<V>) -> U,
    {
        let keys = self.keys();
        let frame = irrigation_core::frame::current_or_engine_panic("map_values");
        let runtime = std::sync::Arc::clone(frame.runtime());
        let call_scope =
            runtime.child_scope(frame.scope(), u64::from(frame.node().0), "map_values");
        keys.into_iter()
            .filter_map(|key| {
                // The key set is a snapshot of the structure, not a promise that
                // every key still has cells: a concurrent retraction can move a
                // member between `keys()` and the payload read. A member whose
                // cells disagree is skipped, and the structure observation the
                // read above recorded makes the demand retry against the new
                // key set.
                if !self.contains_key(&key) {
                    return None;
                }
                let member_scope =
                    runtime.child_scope_keyed(call_scope, any_key(key.clone()), "map_member");
                let key_for_read = key.clone();
                let param_node = runtime.site_node(member_scope, SiteId(1));
                let value: Option<V> = irrigation_core::ops::eval_node(param_node, || {
                    self.member_value(&key_for_read)
                });
                let value = value?;
                let result_node = runtime.site_node(member_scope, SiteId(2));
                let mapped = irrigation_core::ops::eval_node(result_node, || {
                    f(irrigation_runtime::Param::new(param_node, value.clone()))
                });
                Some((key, mapped))
            })
            .collect()
    }

    /// Defines a keyed derived list from this map's entries.
    ///
    /// The list targets one identity per map key; a value edit on one key
    /// restages only that key's list member.
    pub fn to_list<T, F>(&self, target: &crate::list::DerivedListRef<T>, f: F)
    where
        T: Clone + PartialEq + Send + Sync + 'static,
        F: Fn(&K, &V) -> Option<(u64, T)>,
    {
        let keys = self.keys();
        let mut items = Vec::new();
        for key in &keys {
            if let Some(value) = self.get(key) {
                if let Some(item) = f(key, &value) {
                    items.push(item);
                }
            }
        }
        target.define_keyed(items, "map_to_list");
    }

    /// Defines one entry from the currently executing computation.
    ///
    /// The definition commits atomically with the computation's value and
    /// trace, and a duplicate definition from another computation is an error.
    #[track_caller]
    pub fn define(&self, key: K, value: V, label: &'static str) {
        let unit = self.value_key(&key);
        write_group(
            WriteScope::entity(&unit),
            label,
            &[],
            |_| (),
            |()| define_unit(unit, value, label),
        );
    }

    /// Inserts a new key, failing if it is already present.
    ///
    /// The absence is a requirement on the write group rather than a skip: a
    /// checked insert must fail when the key exists, where the conditional
    /// verb reads the membership cell and returns. The requirement is
    /// re-evaluated at commit, after a concurrent writer of the key has
    /// published, and the group's own previous publication is exempt.
    #[track_caller]
    pub fn insert(&self, key: K, value: V, label: &'static str) {
        let unit = self.value_key(&key);
        let ports = [self.membership_key(&key)];
        write_group(
            WriteScope::entity(&unit),
            label,
            &[PortPrecondition::absent(&ports, "map key absent")],
            |_| (),
            |()| define_unit(unit, value, label),
        );
    }

    /// Updates an existing key, failing if it is absent.
    ///
    /// Presence is a requirement on the write group for the same reason: this
    /// verb must fail, while [`Self::update_if_present`] observes the
    /// membership cell and skips. The requirement is re-checked at commit, and
    /// the payload the group installs is derived from the committed content.
    #[track_caller]
    pub fn update(&self, key: K, value: V, label: &'static str) {
        let unit = self.value_key(&key);
        let ports = [self.membership_key(&key)];
        write_group(
            WriteScope::entity(&unit),
            label,
            &[PortPrecondition::present(&ports, "map key present")],
            |_| (),
            |()| define_unit(unit, value, label),
        );
    }

    /// Updates a key only when it is present; absent is a no-op.
    ///
    /// Presence is a tracked observation of the key's membership cell, so the
    /// site re-decides when the key appears or disappears. The kernel excludes
    /// a presence cell from the ports a write group installs, so the read is
    /// not the read-your-own-write shape. The closure is still applied to the
    /// committed View content, projected after the port is claimed: a tracked
    /// read of the payload would observe the unit this group replaces.
    #[track_caller]
    pub fn update_if_present(&self, key: K, value: impl FnOnce(&V) -> V, label: &'static str) {
        let unit = self.value_key(&key);
        if !self.membership(&key) {
            return;
        }
        write_group(
            WriteScope::entity(&unit),
            label,
            &[],
            |content| self.committed_entries(content).get(&key).cloned(),
            |old| {
                if let Some(old) = old {
                    define_unit(unit, value(&old), label);
                }
            },
        );
    }

    /// Removes a key only when it is present; absent is a no-op.
    ///
    /// Presence is a tracked observation of the key's membership cell, so the
    /// site re-decides when the key appears; the kernel excludes a presence
    /// cell from the ports a write group installs, so the read does not make
    /// the verb's own retraction self-dependent. The committed-content
    /// projection still decides the write, because two cells that disagree —
    /// a membership-only re-arm — describe an absent member.
    ///
    /// The retracted unit is the value, which is the unit [`Self::define`]
    /// installs: the kernel derives the membership cell, the structure, and the
    /// payload's certification from it. Retracting the membership cell instead
    /// would leave the payload certified, and a membership-only re-arm would
    /// then read the removed entry's value while the structure omits the key.
    #[track_caller]
    pub fn remove_if_present(&self, key: K, label: &'static str) {
        let unit = self.value_key(&key);
        if !self.membership(&key) {
            return;
        }
        write_group(
            WriteScope::entity(&unit),
            label,
            &[],
            |content| self.committed_entries(content).contains_key(&key),
            |present| {
                if present {
                    retract_unit(unit, label);
                }
            },
        );
    }

    /// Reads one key's presence, recording the membership observation.
    ///
    /// A materialized membership cell is the committed answer; one that has
    /// never been read is initialized from the committed content. The cell,
    /// not the projection, is what the calling site observes, so a later
    /// install of the key invalidates the site and the verb re-decides.
    fn membership(&self, key: &K) -> bool {
        read_unit_value(self.membership_key(key), |content| {
            Some(self.committed_entries(content).contains_key(key))
        })
    }

    /// Defines the whole map: entries this run no longer produces are
    /// retracted.
    ///
    /// A `Map` has no whole-field value publisher, so a total producer that
    /// only stages entity units leaves the entries it stopped producing in the
    /// committed content until something restates the field. This verb closes
    /// that gap: it takes the whole field, projects the committed key set, and
    /// retracts the keys the new set omits. A total publisher therefore
    /// conflicts with a live per-key writer; delta publishers keep using
    /// [`Self::define`], [`Self::insert`], and [`Self::update`].
    #[track_caller]
    pub fn define_all<I>(&self, items: I, label: &'static str)
    where
        I: IntoIterator<Item = (K, V)>,
    {
        let items: Vec<(K, V)> = items.into_iter().collect();
        write_group(
            WriteScope::Field(self.view, self.field),
            label,
            &[],
            |content| self.committed_entries(content),
            |previous| {
                let produced: PMap<K, ()> =
                    items.iter().map(|(key, _)| (key.clone(), ())).collect();
                for (key, _) in previous.iter() {
                    if produced.get(key).is_none() {
                        retract_unit(self.value_key(key), label);
                    }
                }
                for (key, value) in items {
                    define_unit(self.value_key(&key), value, label);
                }
            },
        );
    }
}

impl<K, V> ViewFieldHandle for Map<K, V>
where
    K: Ord + Hash + Clone + Send + Sync + Debug + 'static,
    V: Clone + PartialEq + Send + Sync + 'static,
{
    type Handle = MapRef<K, V>;
    type Editor<'a> = MapEditor<'a, K, V>;

    fn handle(
        view: ViewId,
        field: FieldId,
        content: fn(&dyn Any) -> Self::Storage,
    ) -> Self::Handle {
        MapRef::new(view, field, content)
    }

    fn editor<'a>(
        edit: &'a mut Edit,
        view: ViewId,
        field: FieldId,
        storage: &'a mut Self::Storage,
    ) -> Self::Editor<'a> {
        MapEditor {
            edit,
            view,
            field,
            storage,
        }
    }
}

/// Transactional editor for a map field.
pub struct MapEditor<'a, K, V>
where
    K: Ord + Hash + Clone + Send + Sync + Debug + 'static,
    V: Clone + PartialEq + Send + Sync + 'static,
{
    edit: &'a mut Edit,
    view: ViewId,
    field: FieldId,
    storage: &'a mut MapStorage<K, V>,
}

impl<K, V> Debug for MapEditor<'_, K, V>
where
    K: Ord + Hash + Clone + Send + Sync + Debug + 'static,
    V: Clone + PartialEq + Send + Sync + 'static,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MapEditor").finish_non_exhaustive()
    }
}

impl<K, V> MapEditor<'_, K, V>
where
    K: Ord + Hash + Clone + Send + Sync + Debug + 'static,
    V: Clone + PartialEq + Send + Sync + 'static,
{
    /// Inserts or replaces one entry.
    pub fn insert(&mut self, key: K, value: V) {
        let was_present = self.storage.contains_key(&key);
        *self.storage = self.storage.insert(key.clone(), value.clone()).0;
        let membership = UnitKey::entity(
            self.view,
            self.field,
            UnitKind::Membership,
            any_key(key.clone()),
        );
        if !was_present {
            self.edit.set(membership, true);
        }
        let value_key = UnitKey::entity(self.view, self.field, UnitKind::Value, any_key(key));
        self.edit.set(value_key, value);
    }

    /// Removes one entry.
    pub fn remove(&mut self, key: &K) {
        let (next, removed) = self.storage.remove(key);
        if removed.is_none() {
            return;
        }
        *self.storage = next;
        let membership = UnitKey::entity(
            self.view,
            self.field,
            UnitKind::Membership,
            any_key(key.clone()),
        );
        self.edit.set(membership, false);
    }

    fn stage_structure(&mut self) {
        self.edit.set(
            structure_unit(self.view, self.field),
            self.storage.keys().clone(),
        );
    }
}

impl<K, V> Drop for MapEditor<'_, K, V>
where
    K: Ord + Hash + Clone + Send + Sync + Debug + 'static,
    V: Clone + PartialEq + Send + Sync + 'static,
{
    fn drop(&mut self) {
        self.stage_structure();
    }
}

impl<K, V> DeltaStore for MapStorage<K, V>
where
    K: Ord + Clone + Send + Sync + 'static,
    V: Clone + PartialEq + Send + Sync + 'static,
{
    type Key = K;
    type Delta = MapDelta<K, V>;

    fn apply_delta(&self, delta: &Self::Delta) -> (Self, AppliedDelta<K>) {
        MapStorage::apply_delta(self, delta)
    }
}

#[cfg(test)]
#[path = "../tests/unit/map/tests.rs"]
mod tests;
