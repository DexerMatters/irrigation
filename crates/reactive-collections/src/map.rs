//! Incremental maps keyed by user values.
//!
//! A map entry has two independent observations: membership of its key and the
//! value stored there. Reading `map.get(k) == None` subscribes to the key's
//! membership cell, so inserting `k` wakes exactly that reader.

use std::any::Any;
use std::fmt::Debug;
use std::hash::Hash;
use std::marker::PhantomData;

use reactive_core::descriptor::{FieldKind, UnitKey, UnitKind};
use reactive_core::ids::{FieldId, SiteId, ViewId};
use reactive_core::key::any_key;
use reactive_core::value::{Value, value_of};
use reactive_runtime::edit::Edit;
use reactive_runtime::field::{ViewFieldHandle, structure_unit};
use reactive_runtime::helpers::read_unit_value;
use reactive_store::field::ViewField;
use reactive_store::persistent::PMap;

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
    type Storage = PMap<K, V>;

    const KIND: FieldKind = FieldKind::Map;

    fn empty() -> Self::Storage {
        PMap::new()
    }

    fn len(storage: &Self::Storage) -> Option<usize> {
        Some(storage.len())
    }

    fn define(
        storage: &Self::Storage,
        key: &dyn reactive_core::key::AnyKey,
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

    fn retract(storage: &Self::Storage, key: &dyn reactive_core::key::AnyKey) -> Self::Storage {
        match key.as_any().downcast_ref::<K>() {
            Some(key) => storage.remove(key).0,
            None => storage.clone(),
        }
    }

    fn contains(storage: &Self::Storage, key: &dyn reactive_core::key::AnyKey) -> bool {
        key.as_any()
            .downcast_ref::<K>()
            .is_some_and(|key| storage.contains_key(key))
    }

    fn structure(storage: &Self::Storage) -> Option<reactive_core::descriptor::Structure> {
        let keys: PMap<K, ()> = storage.iter().map(|(key, _)| (key.clone(), ())).collect();
        Some(reactive_core::descriptor::Structure {
            value: value_of(keys),
            eq: |left, right| {
                left.downcast_ref::<PMap<K, ()>>() == right.downcast_ref::<PMap<K, ()>>()
            },
        })
    }

    fn get(storage: &Self::Storage, key: &dyn reactive_core::key::AnyKey) -> Option<Value> {
        let key = key.as_any().downcast_ref::<K>()?;
        storage.get(key).map(|value| value_of(value.clone()))
    }
}

/// Tracked read handle for a map field.
#[derive(Clone)]
pub struct MapRef<K, V> {
    view: ViewId,
    field: FieldId,
    content: fn(&dyn Any) -> PMap<K, V>,
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

impl<K, V> MapRef<K, V>
where
    K: Ord + Hash + Clone + Send + Sync + Debug + 'static,
    V: Clone + PartialEq + Send + Sync + 'static,
{
    /// Builds a handle for one map field.
    #[must_use]
    pub fn new(view: ViewId, field: FieldId, content: fn(&dyn Any) -> PMap<K, V>) -> Self {
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

    fn structure(&self) -> PMap<K, ()> {
        read_unit_value(structure_unit(self.view, self.field), |content| {
            Some(
                (self.content)(content)
                    .iter()
                    .map(|(key, _)| (key.clone(), ()))
                    .collect(),
            )
        })
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
    #[must_use]
    pub fn get(&self, key: &K) -> Option<V> {
        if !self.contains_key(key) {
            return None;
        }
        let value: V = read_unit_value(self.value_key(key), |content| {
            (self.content)(content).get(key).cloned()
        });
        Some(value)
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
        F: Fn(reactive_runtime::Param<V>) -> U,
    {
        let view = self.view;
        let field = self.field;
        let content_fn = self.content;
        let keys = self.keys();
        let frame = reactive_core::frame::current_or_engine_panic("map_values");
        let runtime = std::sync::Arc::clone(frame.runtime());
        let call_scope =
            runtime.child_scope(frame.scope(), u64::from(frame.node().0), "map_values");
        keys.into_iter()
            .map(|key| {
                let member_scope =
                    runtime.child_scope_keyed(call_scope, any_key(key.clone()), "map_member");
                let key_for_read = key.clone();
                let param_node = runtime.site_node(member_scope, SiteId(1));
                let value: V = reactive_core::ops::eval_node(param_node, || {
                    read_unit_value(
                        UnitKey::entity(
                            view,
                            field,
                            UnitKind::Value,
                            any_key(key_for_read.clone()),
                        ),
                        |content| (content_fn)(content).get(&key_for_read).cloned(),
                    )
                });
                let result_node = runtime.site_node(member_scope, SiteId(2));
                let mapped = reactive_core::ops::eval_node(result_node, || {
                    f(reactive_runtime::Param::new(param_node, value.clone()))
                });
                (key, mapped)
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
        reactive_runtime::helpers::define_unit(self.value_key(&key), value, label);
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
    storage: &'a mut PMap<K, V>,
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
        let structure: PMap<K, ()> = self
            .storage
            .iter()
            .map(|(key, _)| (key.clone(), ()))
            .collect();
        self.edit
            .set(structure_unit(self.view, self.field), structure);
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

#[cfg(test)]
mod tests {
    use super::*;
    use reactive_core::descriptor::{FieldDescriptor, UnitKey};

    #[test]
    fn map_field_installs_and_retracts_units() {
        let storage = <Map<u32, String> as ViewField>::empty();
        let key = UnitKey::entity(ViewId(1), FieldId(0), UnitKind::Value, any_key(7u32));
        let storage = <Map<u32, String> as ViewField>::define(
            &storage,
            key.entity_key().expect("entity").as_ref(),
            value_of(String::from("seven")),
        );
        assert_eq!(storage.get(&7), Some(&String::from("seven")));
        assert!(<Map<u32, String> as ViewField>::contains(
            &storage,
            key.entity_key().expect("entity").as_ref()
        ));
        let storage = <Map<u32, String> as ViewField>::retract(
            &storage,
            key.entity_key().expect("entity").as_ref(),
        );
        assert!(storage.is_empty());
    }

    #[test]
    fn map_descriptor_kind_is_stable() {
        assert_eq!(<Map<u32, u32> as ViewField>::KIND, FieldKind::Map);
        let _ = FieldDescriptor {
            name: "entries",
            kind: <Map<u32, u32> as ViewField>::KIND,
        };
    }
}
