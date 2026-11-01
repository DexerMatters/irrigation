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

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::hash::Hash;
use std::sync::Arc;

use reactive_core::descriptor::UnitKey;
use reactive_core::ids::{FieldId, ViewId};
use reactive_core::key::any_key;
use reactive_core::value::Value;
use reactive_runtime::field::structure_unit;
use reactive_runtime::general::{ChangeBatch, DependencyUpdates, GeneralComponent, GeneralCx};

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
}

/// Stages one definition per joined key on the optional target View.
fn publish<S: JoinSpec>(cx: &mut GeneralCx, joined: &BTreeMap<S::Key, S::Joined>) {
    let Some((view, field)) = S::target() else {
        return;
    };
    for (key, value) in joined {
        cx.define(
            UnitKey::entity(
                view,
                field,
                reactive_core::descriptor::UnitKind::Value,
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
    key: &Arc<dyn reactive_core::key::AnyKey>,
) -> UnitKey {
    UnitKey::entity(
        view,
        field,
        reactive_core::descriptor::UnitKind::Membership,
        Arc::clone(key),
    )
}

fn value(view: ViewId, field: FieldId, key: &Arc<dyn reactive_core::key::AnyKey>) -> UnitKey {
    UnitKey::entity(
        view,
        field,
        reactive_core::descriptor::UnitKind::Value,
        Arc::clone(key),
    )
}

fn entity_key(unit: &UnitKey) -> Option<Arc<dyn reactive_core::key::AnyKey>> {
    unit.entity.as_ref().map(Arc::clone)
}

/// Reads one side's value for `key`, or `None` when the key is absent.
fn read_side<V: Clone + PartialEq + Send + Sync + 'static>(
    cx: &mut GeneralCx,
    view: ViewId,
    field: FieldId,
    key: &Arc<dyn reactive_core::key::AnyKey>,
) -> reactive_core::Result<Option<V>> {
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
) -> reactive_core::Result<Option<(S::Left, S::Right)>> {
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

fn refresh_key<S: JoinSpec>(
    cx: &mut GeneralCx,
    state: &mut JoinState<S>,
    key: &S::Key,
) -> reactive_core::Result<()> {
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

    const ID: reactive_core::ComponentId = reactive_core::ComponentId(0x00BB_0001);
    const NAME: &'static str = "collections::EquiJoin";

    fn dependency_updates(&self) -> DependencyUpdates {
        DependencyUpdates::Incremental
    }

    fn initialize(&self, cx: &mut GeneralCx) -> reactive_core::Result<(Self::State, Self::Output)> {
        let (left_view, left_field) = S::left();
        let (right_view, right_field) = S::right();
        let left_keys: reactive_store::persistent::PMap<S::Key, ()> =
            cx.read_typed(structure_unit(left_view, left_field))?;
        let right_keys: reactive_store::persistent::PMap<S::Key, ()> =
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
    ) -> reactive_core::Result<(Self::State, Self::Output)> {
        let mut state = state.clone();
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
            let left_keys: reactive_store::persistent::PMap<S::Key, ()> =
                cx.read_typed(structure_unit(left_view, left_field))?;
            let right_keys: reactive_store::persistent::PMap<S::Key, ()> =
                cx.read_typed(structure_unit(right_view, right_field))?;
            let existing: Vec<S::Key> = state.joined.keys().cloned().collect();
            for key in existing {
                if !left_keys.contains_key(&key) || !right_keys.contains_key(&key) {
                    state.left.remove(&key);
                    state.right.remove(&key);
                    state.joined.remove(&key);
                    continue;
                }
                let sides = joined_for::<S>(cx, &key)?;
                match sides {
                    Some((left, right))
                        if state.left.get(&key) == Some(&left)
                            && state.right.get(&key) == Some(&right) => {}
                    Some((left, right)) => {
                        let joined = S::combine(&left, &right);
                        state.left.insert(key.clone(), left);
                        state.right.insert(key.clone(), right);
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
                }
            }
            publish::<S>(cx, &state.joined);
            return Ok((state.clone(), state.joined));
        }
        let (left_view, left_field) = S::left();
        let (right_view, right_field) = S::right();
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
            refresh_key::<S>(cx, &mut state, key)?;
        }
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
