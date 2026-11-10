//! Incremental equi-joins over map Views.
//!
//! The join is a [`GeneralComponent`]: it retains the joined result and consumes
//! a change batch naming exactly the units that moved. For every changed key it
//! re-reads both sides and recomputes that key's contribution, which is the
//! per-key form of the bilinear rule:
//!
//! ```text
//! Δ(A ⋈ B) = (ΔA ⋈ B) + (A ⋈ ΔB) + (ΔA ⋈ ΔB)
//! ```
//!
//! Re-reading both sides for a changed key applies all three terms once, without
//! double counting and without touching untouched keys. Deletion is handled by
//! recomputing the key and dropping its entry when either side is absent.
//!
//! Updates retain every dependency they did not re-read: the key-set structure
//! units and the membership/value units of untouched joined keys are observed
//! after the delta, so a later edit to any joined key or to the key set
//! invalidates the join. With `i64` sides and multiplication this is a bilinear
//! bag join over signed weights (negative weights retract).

use std::collections::{BTreeMap, HashSet};
use std::fmt::Debug;
use std::hash::Hash;
use std::sync::Arc;

use irrigation_core::descriptor::{UnitKey, ViewDescriptor};
use irrigation_core::ids::{FieldId, ViewId};
use irrigation_core::key::any_key;
use irrigation_core::value::Value;
use irrigation_runtime::field::structure_unit;
use irrigation_runtime::general::{ChangeBatch, DependencyUpdates, GeneralComponent, GeneralCx};

/// The two map Views and the combination rule of one equi-join.
pub trait JoinSpec: Send + Sync + 'static {
    /// Join key, shared by both sides.
    type Key: Ord + Hash + Clone + Debug + Send + Sync + 'static;
    /// Left value.
    type Left: Clone + PartialEq + Send + Sync + 'static;
    /// Right value.
    type Right: Clone + PartialEq + Send + Sync + 'static;
    /// Joined value.
    type Joined: Clone + PartialEq + Send + Sync + 'static;

    /// The left View and its map field.
    fn left() -> (ViewId, FieldId);
    /// The right View and its map field.
    fn right() -> (ViewId, FieldId);
    /// Combines the two sides for one key.
    fn combine(left: &Self::Left, right: &Self::Right) -> Self::Joined;

    /// Optional map View that receives one entry per joined key.
    ///
    /// The target View must be registered before the join is demanded (for
    /// example with `Runtime::register_view`). The whole-map output is always
    /// produced, so callers that do not opt in are unaffected.
    fn target() -> Option<(ViewId, FieldId)> {
        None
    }

    /// Optional descriptor for the target View.
    ///
    /// A producer install needs the descriptor to register and own the View;
    /// the id-only `target` is still what the write path uses.
    fn target_view() -> Option<&'static ViewDescriptor> {
        None
    }
}

/// Stages one definition per joined key on the optional target View.
fn publish<S: JoinSpec>(cx: &mut GeneralCx, joined: &BTreeMap<S::Key, S::Joined>) {
    let Some((view, field)) = S::target() else {
        return;
    };
    // The join recomputes the whole map, so keys that disappeared must be
    // retracted: absence alone no longer removes a definition.
    cx.replace_all();
    for (key, value) in joined {
        cx.define(
            UnitKey::entity(
                view,
                field,
                irrigation_core::descriptor::UnitKind::Value,
                any_key(key.clone()),
            ),
            value.clone(),
            "equi_join",
        );
    }
}

fn membership(
    view: ViewId,
    field: FieldId,
    key: &Arc<dyn irrigation_core::key::AnyKey>,
) -> UnitKey {
    UnitKey::entity(
        view,
        field,
        irrigation_core::descriptor::UnitKind::Membership,
        Arc::clone(key),
    )
}

fn value(view: ViewId, field: FieldId, key: &Arc<dyn irrigation_core::key::AnyKey>) -> UnitKey {
    UnitKey::entity(
        view,
        field,
        irrigation_core::descriptor::UnitKind::Value,
        Arc::clone(key),
    )
}

fn entity_key(unit: &UnitKey) -> Option<Arc<dyn irrigation_core::key::AnyKey>> {
    unit.entity.as_ref().map(Arc::clone)
}

/// Reads one side's value for `key`, or `None` when the key is absent.
fn read_side<V: Clone + PartialEq + Send + Sync + 'static>(
    cx: &mut GeneralCx,
    view: ViewId,
    field: FieldId,
    key: &Arc<dyn irrigation_core::key::AnyKey>,
) -> irrigation_core::Result<Option<V>> {
    let present: bool = cx.read_typed(membership(view, field, key))?;
    if !present {
        return Ok(None);
    }
    let value: V = cx.read_typed(value(view, field, key))?;
    Ok(Some(value))
}

/// Retained state of one equi-join.
///
/// Both sides are kept so a structural change can tell which surviving keys
/// actually moved, instead of recombining every key.
pub struct JoinState<S: JoinSpec> {
    left: BTreeMap<S::Key, S::Left>,
    right: BTreeMap<S::Key, S::Right>,
    joined: BTreeMap<S::Key, S::Joined>,
}

impl<S: JoinSpec> Clone for JoinState<S> {
    fn clone(&self) -> Self {
        Self {
            left: self.left.clone(),
            right: self.right.clone(),
            joined: self.joined.clone(),
        }
    }
}

impl<S: JoinSpec> PartialEq for JoinState<S> {
    fn eq(&self, other: &Self) -> bool {
        self.left == other.left && self.right == other.right && self.joined == other.joined
    }
}

impl<S: JoinSpec> std::fmt::Debug for JoinState<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JoinState")
            .field("joined", &self.joined.len())
            .finish()
    }
}

impl<S: JoinSpec> Default for JoinState<S> {
    fn default() -> Self {
        Self {
            left: BTreeMap::new(),
            right: BTreeMap::new(),
            joined: BTreeMap::new(),
        }
    }
}

impl<S: JoinSpec> JoinState<S> {
    /// The joined entries.
    #[must_use]
    pub fn joined(&self) -> &BTreeMap<S::Key, S::Joined> {
        &self.joined
    }
}

/// An incremental equi-join component.
pub struct EquiJoin<S: JoinSpec> {
    marker: std::marker::PhantomData<fn() -> S>,
}

impl<S: JoinSpec> EquiJoin<S> {
    /// Builds the component.
    #[must_use]
    pub fn new() -> Self {
        Self {
            marker: std::marker::PhantomData,
        }
    }
}

impl<S: JoinSpec> Default for EquiJoin<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: JoinSpec> std::fmt::Debug for EquiJoin<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EquiJoin")
    }
}

fn joined_for<S: JoinSpec>(
    cx: &mut GeneralCx,
    key: &S::Key,
) -> irrigation_core::Result<Option<(S::Left, S::Right)>> {
    let (left_view, left_field) = S::left();
    let (right_view, right_field) = S::right();
    let typed = any_key(key.clone());
    let left: Option<S::Left> = read_side(cx, left_view, left_field, &typed)?;
    let right: Option<S::Right> = read_side(cx, right_view, right_field, &typed)?;
    Ok(match (left, right) {
        (Some(left), Some(right)) => Some((left, right)),
        _ => None,
    })
}

/// Re-observes every retained dependency that the update path did not re-read,
/// so a later edit to any joined key or to the key set invalidates the join.
fn retain_dependencies<S: JoinSpec>(
    cx: &mut GeneralCx,
    joined: &BTreeMap<S::Key, S::Joined>,
    refreshed: &HashSet<S::Key>,
    structural_read: bool,
) -> irrigation_core::Result<()> {
    let (left_view, left_field) = S::left();
    let (right_view, right_field) = S::right();
    if !structural_read {
        cx.observe(structure_unit(left_view, left_field))?;
        cx.observe(structure_unit(right_view, right_field))?;
    }
    for key in joined.keys() {
        if refreshed.contains(key) {
            continue;
        }
        let typed = any_key(key.clone());
        cx.observe(membership(left_view, left_field, &typed))?;
        cx.observe(value(left_view, left_field, &typed))?;
        cx.observe(membership(right_view, right_field, &typed))?;
        cx.observe(value(right_view, right_field, &typed))?;
    }
    Ok(())
}

fn refresh_key<S: JoinSpec>(
    cx: &mut GeneralCx,
    state: &mut JoinState<S>,
    key: &S::Key,
) -> irrigation_core::Result<()> {
    match joined_for::<S>(cx, key)? {
        Some((left, right)) => {
            let joined = S::combine(&left, &right);
            state.left.insert(key.clone(), left);
            state.right.insert(key.clone(), right);
            state.joined.insert(key.clone(), joined);
        }
        None => {
            state.left.remove(key);
            state.right.remove(key);
            state.joined.remove(key);
        }
    }
    Ok(())
}

impl<S: JoinSpec> GeneralComponent for EquiJoin<S> {
    type State = JoinState<S>;
    type Output = BTreeMap<S::Key, S::Joined>;

    const ID: irrigation_core::ComponentId = irrigation_core::ComponentId(0x00BB_0001);
    const NAME: &'static str = "collections::EquiJoin";

    fn component_id(&self) -> irrigation_core::ComponentId {
        irrigation_core::ComponentId(irrigation_core::hash::hash_one(std::any::type_name::<S>()))
    }

    fn output_views(&self) -> Vec<&'static ViewDescriptor> {
        S::target_view().into_iter().collect()
    }

    fn dependency_updates(&self) -> DependencyUpdates {
        DependencyUpdates::Incremental
    }

    fn initialize(
        &self,
        cx: &mut GeneralCx,
    ) -> irrigation_core::Result<(Self::State, Self::Output)> {
        let (left_view, left_field) = S::left();
        let (right_view, right_field) = S::right();
        let left_keys: irrigation_store::persistent::PMap<S::Key, ()> =
            cx.read_typed(structure_unit(left_view, left_field))?;
        let right_keys: irrigation_store::persistent::PMap<S::Key, ()> =
            cx.read_typed(structure_unit(right_view, right_field))?;
        let mut state = JoinState::default();
        for (key, _) in left_keys.iter() {
            if right_keys.contains_key(key) {
                refresh_key::<S>(cx, &mut state, key)?;
            }
        }
        publish::<S>(cx, &state.joined);
        Ok((state.clone(), state.joined))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        state: &Self::State,
        changes: &ChangeBatch,
    ) -> irrigation_core::Result<(Self::State, Self::Output)> {
        let mut state = state.clone();
        let mut refreshed: HashSet<S::Key> = HashSet::new();
        // A structural change moves the key set itself, so the per-key path
        // cannot see newly joinable keys. Re-read the key sets and refresh only
        // keys whose sides actually moved.
        let structural = changes
            .changes()
            .iter()
            .any(|change| change.unit.entity.is_none());
        if structural {
            let (left_view, left_field) = S::left();
            let (right_view, right_field) = S::right();
            let left_keys: irrigation_store::persistent::PMap<S::Key, ()> =
                cx.read_typed(structure_unit(left_view, left_field))?;
            let right_keys: irrigation_store::persistent::PMap<S::Key, ()> =
                cx.read_typed(structure_unit(right_view, right_field))?;
            let existing: Vec<S::Key> = state.joined.keys().cloned().collect();
            for key in existing {
                if !left_keys.contains_key(&key) || !right_keys.contains_key(&key) {
                    state.left.remove(&key);
                    state.right.remove(&key);
                    state.joined.remove(&key);
                    continue;
                }
                // The read below is the only read for this key in this run.
                match joined_for::<S>(cx, &key)? {
                    Some((left, right))
                        if state.left.get(&key) == Some(&left)
                            && state.right.get(&key) == Some(&right) => {}
                    Some((left, right)) => {
                        let joined = S::combine(&left, &right);
                        state.left.insert(key.clone(), left);
                        state.right.insert(key.clone(), right);
                        refreshed.insert(key.clone());
                        state.joined.insert(key, joined);
                    }
                    None => {
                        state.left.remove(&key);
                        state.right.remove(&key);
                        state.joined.remove(&key);
                    }
                }
            }
            for (key, _) in left_keys.iter() {
                if right_keys.contains_key(key) && !state.joined.contains_key(key) {
                    refresh_key::<S>(cx, &mut state, key)?;
                    refreshed.insert(key.clone());
                }
            }
            publish::<S>(cx, &state.joined);
            return Ok((state.clone(), state.joined));
        }
        let (left_view, left_field) = S::left();
        let (right_view, right_field) = S::right();
        let mut seen: HashSet<S::Key> = HashSet::new();
        for change in changes.changes() {
            let unit = &change.unit;
            let is_join_unit = (unit.view == left_view && unit.field == left_field)
                || (unit.view == right_view && unit.field == right_field);
            if !is_join_unit {
                continue;
            }
            let Some(key) = entity_key(unit) else {
                continue;
            };
            let Some(key) = key.as_any().downcast_ref::<S::Key>() else {
                continue;
            };
            if !seen.insert(key.clone()) {
                continue;
            }
            refresh_key::<S>(cx, &mut state, key)?;
            refreshed.insert(key.clone());
        }
        retain_dependencies::<S>(cx, &state.joined, &refreshed, false)?;
        publish::<S>(cx, &state.joined);
        Ok((state.clone(), state.joined))
    }
}

/// Reads a unit as a concrete value, used by adapters that stage their own
/// outputs.
#[must_use]
pub fn downcast<T: Clone + 'static>(value: &Value) -> Option<T> {
    value.downcast_ref::<T>().cloned()
}

/// A closure-configured equi-join.
///
/// This is the ordinary authoring form: the two map fields and the combination
/// are values, and `publish_to` names the optional output map. The type-level
/// [`EquiJoin`] remains for callers that prefer a named `JoinSpec`.
pub struct Join<K, L, R, J, F> {
    left: crate::field::MapField<K, L>,
    right: crate::field::MapField<K, R>,
    combine: F,
    target: Option<crate::field::MapField<K, J>>,
}

impl<K, L, R, J, F> Join<K, L, R, J, F>
where
    K: Ord + Hash + Clone + Debug + Send + Sync + 'static,
    L: Clone + PartialEq + Send + Sync + 'static,
    R: Clone + PartialEq + Send + Sync + 'static,
    J: Clone + PartialEq + Send + Sync + 'static,
    F: Fn(&L, &R) -> J + Send + Sync + 'static,
{
    /// Builds a join over two map fields.
    #[must_use]
    pub fn new(
        left: crate::field::MapField<K, L>,
        right: crate::field::MapField<K, R>,
        combine: F,
    ) -> Self {
        Self {
            left,
            right,
            combine,
            target: None,
        }
    }

    /// Publishes one entry per joined key into `target`.
    #[must_use]
    pub fn publish_to(mut self, target: crate::field::MapField<K, J>) -> Self {
        self.target = Some(target);
        self
    }

    fn left_ids(&self) -> (ViewId, FieldId) {
        (self.left.view(), self.left.id())
    }

    fn right_ids(&self) -> (ViewId, FieldId) {
        (self.right.view(), self.right.id())
    }
}

impl<K, L, R, J, F> std::fmt::Debug for Join<K, L, R, J, F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Join")
    }
}

/// Retained state of a closure-configured join.
pub struct FieldJoinState<K, L, R, J> {
    left: BTreeMap<K, L>,
    right: BTreeMap<K, R>,
    joined: BTreeMap<K, J>,
}

impl<K, L, R, J> Clone for FieldJoinState<K, L, R, J>
where
    K: Clone,
    L: Clone,
    R: Clone,
    J: Clone,
{
    fn clone(&self) -> Self {
        Self {
            left: self.left.clone(),
            right: self.right.clone(),
            joined: self.joined.clone(),
        }
    }
}

impl<K, L, R, J> PartialEq for FieldJoinState<K, L, R, J>
where
    K: PartialEq,
    L: PartialEq,
    R: PartialEq,
    J: PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        self.left == other.left && self.right == other.right && self.joined == other.joined
    }
}

impl<K, L, R, J> std::fmt::Debug for FieldJoinState<K, L, R, J> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FieldJoinState")
            .field("joined", &self.joined.len())
            .finish()
    }
}

impl<K, L, R, J> Default for FieldJoinState<K, L, R, J> {
    fn default() -> Self {
        Self {
            left: BTreeMap::new(),
            right: BTreeMap::new(),
            joined: BTreeMap::new(),
        }
    }
}

impl<K, L, R, J, F> GeneralComponent for Join<K, L, R, J, F>
where
    K: Ord + Hash + Clone + Debug + Send + Sync + 'static,
    L: Clone + PartialEq + Send + Sync + 'static,
    R: Clone + PartialEq + Send + Sync + 'static,
    J: Clone + PartialEq + Send + Sync + 'static,
    F: Fn(&L, &R) -> J + Send + Sync + 'static,
{
    type State = FieldJoinState<K, L, R, J>;
    type Output = BTreeMap<K, J>;

    const ID: irrigation_core::ComponentId = irrigation_core::ComponentId(0x00BB_0002);
    const NAME: &'static str = "collections::Join";

    fn component_id(&self) -> irrigation_core::ComponentId {
        irrigation_core::ComponentId(irrigation_core::hash::hash_one(&(
            std::any::type_name::<Self>(),
            self.left.view(),
            self.left.id(),
            self.right.view(),
            self.right.id(),
        )))
    }

    fn output_views(&self) -> Vec<&'static ViewDescriptor> {
        self.target
            .iter()
            .map(crate::field::Field::descriptor)
            .collect()
    }

    fn dependency_updates(&self) -> DependencyUpdates {
        DependencyUpdates::Incremental
    }

    fn initialize(
        &self,
        cx: &mut GeneralCx,
    ) -> irrigation_core::Result<(Self::State, Self::Output)> {
        let left_keys: irrigation_store::persistent::PMap<K, ()> =
            cx.read_typed(self.left.structure_unit())?;
        let right_keys: irrigation_store::persistent::PMap<K, ()> =
            cx.read_typed(self.right.structure_unit())?;
        let mut state = FieldJoinState::default();
        for (key, _) in left_keys.iter() {
            if right_keys.contains_key(key) {
                refresh_field_key(self, cx, &mut state, key)?;
            }
        }
        publish_field_join(self, cx, &state.joined);
        Ok((state.clone(), state.joined))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        state: &Self::State,
        changes: &ChangeBatch,
    ) -> irrigation_core::Result<(Self::State, Self::Output)> {
        let mut state = state.clone();
        let mut refreshed: HashSet<K> = HashSet::new();
        let structural = changes
            .changes()
            .iter()
            .any(|change| change.unit.entity.is_none());
        let (left_view, left_field) = self.left_ids();
        let (right_view, right_field) = self.right_ids();
        if structural {
            let left_keys: irrigation_store::persistent::PMap<K, ()> =
                cx.read_typed(self.left.structure_unit())?;
            let right_keys: irrigation_store::persistent::PMap<K, ()> =
                cx.read_typed(self.right.structure_unit())?;
            let existing: Vec<K> = state.joined.keys().cloned().collect();
            for key in existing {
                if !left_keys.contains_key(&key) || !right_keys.contains_key(&key) {
                    state.left.remove(&key);
                    state.right.remove(&key);
                    state.joined.remove(&key);
                    continue;
                }
                match joined_field(self, cx, &key)? {
                    Some((left, right))
                        if state.left.get(&key) == Some(&left)
                            && state.right.get(&key) == Some(&right) => {}
                    Some((left, right)) => {
                        let joined = (self.combine)(&left, &right);
                        state.left.insert(key.clone(), left);
                        state.right.insert(key.clone(), right);
                        refreshed.insert(key.clone());
                        state.joined.insert(key, joined);
                    }
                    None => {
                        state.left.remove(&key);
                        state.right.remove(&key);
                        state.joined.remove(&key);
                    }
                }
            }
            for (key, _) in left_keys.iter() {
                if right_keys.contains_key(key) && !state.joined.contains_key(key) {
                    refresh_field_key(self, cx, &mut state, key)?;
                    refreshed.insert(key.clone());
                }
            }
            publish_field_join(self, cx, &state.joined);
            return Ok((state.clone(), state.joined));
        }
        let mut seen: HashSet<K> = HashSet::new();
        for change in changes.changes() {
            let unit = &change.unit;
            let is_join_unit = (unit.view == left_view && unit.field == left_field)
                || (unit.view == right_view && unit.field == right_field);
            if !is_join_unit {
                continue;
            }
            let Some(key) = entity_key(unit) else {
                continue;
            };
            let Some(key) = key.as_any().downcast_ref::<K>() else {
                continue;
            };
            if !seen.insert(key.clone()) {
                continue;
            }
            refresh_field_key(self, cx, &mut state, key)?;
            refreshed.insert(key.clone());
        }
        retain_field_dependencies(self, cx, &state.joined, &refreshed, false)?;
        publish_field_join(self, cx, &state.joined);
        Ok((state.clone(), state.joined))
    }
}

fn joined_field<K, L, R, J, F>(
    join: &Join<K, L, R, J, F>,
    cx: &mut GeneralCx,
    key: &K,
) -> irrigation_core::Result<Option<(L, R)>>
where
    K: Ord + Hash + Clone + Debug + Send + Sync + 'static,
    L: Clone + PartialEq + Send + Sync + 'static,
    R: Clone + PartialEq + Send + Sync + 'static,
    J: Clone + PartialEq + Send + Sync + 'static,
    F: Fn(&L, &R) -> J + Send + Sync + 'static,
{
    let left = join.left.entry(key.clone()).read_in(cx)?;
    let right = join.right.entry(key.clone()).read_in(cx)?;
    Ok(match (left, right) {
        (Some(left), Some(right)) => Some((left, right)),
        _ => None,
    })
}

fn refresh_field_key<K, L, R, J, F>(
    join: &Join<K, L, R, J, F>,
    cx: &mut GeneralCx,
    state: &mut FieldJoinState<K, L, R, J>,
    key: &K,
) -> irrigation_core::Result<()>
where
    K: Ord + Hash + Clone + Debug + Send + Sync + 'static,
    L: Clone + PartialEq + Send + Sync + 'static,
    R: Clone + PartialEq + Send + Sync + 'static,
    J: Clone + PartialEq + Send + Sync + 'static,
    F: Fn(&L, &R) -> J + Send + Sync + 'static,
{
    match joined_field(join, cx, key)? {
        Some((left, right)) => {
            let joined = (join.combine)(&left, &right);
            state.left.insert(key.clone(), left);
            state.right.insert(key.clone(), right);
            state.joined.insert(key.clone(), joined);
        }
        None => {
            state.left.remove(key);
            state.right.remove(key);
            state.joined.remove(key);
        }
    }
    Ok(())
}

fn retain_field_dependencies<K, L, R, J, F>(
    join: &Join<K, L, R, J, F>,
    cx: &mut GeneralCx,
    joined: &BTreeMap<K, J>,
    refreshed: &HashSet<K>,
    structural_read: bool,
) -> irrigation_core::Result<()>
where
    K: Ord + Hash + Clone + Debug + Send + Sync + 'static,
    L: Clone + PartialEq + Send + Sync + 'static,
    R: Clone + PartialEq + Send + Sync + 'static,
    J: Clone + PartialEq + Send + Sync + 'static,
    F: Fn(&L, &R) -> J + Send + Sync + 'static,
{
    if !structural_read {
        cx.observe(join.left.structure_unit())?;
        cx.observe(join.right.structure_unit())?;
    }
    for key in joined.keys() {
        if refreshed.contains(key) {
            continue;
        }
        join.left.entry(key.clone()).observe_in(cx)?;
        join.right.entry(key.clone()).observe_in(cx)?;
    }
    Ok(())
}

fn publish_field_join<K, L, R, J, F>(
    join: &Join<K, L, R, J, F>,
    cx: &mut GeneralCx,
    joined: &BTreeMap<K, J>,
) where
    K: Ord + Hash + Clone + Debug + Send + Sync + 'static,
    L: Clone + PartialEq + Send + Sync + 'static,
    R: Clone + PartialEq + Send + Sync + 'static,
    J: PartialEq + Clone + Send + Sync + 'static,
    F: Fn(&L, &R) -> J + Send + Sync + 'static,
{
    let Some(target) = &join.target else {
        return;
    };
    cx.replace_all();
    for (key, value) in joined {
        target.entry(key.clone()).set(cx, value.clone());
    }
}
