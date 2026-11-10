//! Edit transactions.
//!
//! An edit stages unit updates against a known source snapshot. At commit the
//! runtime normalizes the transaction by comparing final values, publishes a
//! new revision, and invalidates the affected dependency cone. A transaction
//! whose final state equals the previous state publishes nothing.

use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use irrigation_core::descriptor::{UnitKey, UnitKind, ViewKind};
use irrigation_core::error::{ComputeError, Result};
use irrigation_core::hash::{FxHashMap, FxHashSet};
use irrigation_core::ids::{Revision, ViewId};
use irrigation_core::value::{Value, value_of};

use crate::inner::{CellDelta, Inner, owner_key, payload_key};
use crate::snapshot::EditableView;
use crate::state::{NodeRecord, State, invalidate};

/// One staged unit update.
#[derive(Debug)]
pub struct UnitUpdate {
    /// The unit being written.
    pub key: UnitKey,
    /// The new value.
    pub value: Value,
    /// Semantic equality for the value type.
    pub eq: fn(&Value, &Value) -> bool,
}

/// A transaction over source Views.
///
/// The transaction holds no lock while user code runs; it stages updates and
/// applies them atomically in [`Edit::commit`].
pub struct Edit {
    inner: Arc<Inner>,
    updates: Vec<UnitUpdate>,
    label: String,
}

impl std::fmt::Debug for Edit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Edit")
            .field("label", &self.label)
            .field("updates", &self.updates.len())
            .finish()
    }
}

impl Edit {
    pub(crate) fn new(inner: Arc<Inner>) -> Self {
        Self {
            inner,
            updates: Vec::new(),
            label: String::new(),
        }
    }

    /// The label of this transaction.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Returns the editor for a View.
    pub fn view<V: EditableView>(&mut self) -> V::Editor<'_> {
        V::edit(self)
    }

    /// Applies a closure to one View editor.
    ///
    /// This is the scoped form of [`Edit::view`]: the editor's whole-content
    /// commit runs when the closure returns, and repeated calls compose
    /// because the generated editor starts from staged content.
    pub fn with_view<V: EditableView, T>(&mut self, f: impl FnOnce(&mut V::Editor<'_>) -> T) -> T {
        let mut editor = V::edit(self);
        f(&mut editor)
    }

    /// Registers a View namespace if it is not registered yet.
    pub fn ensure_view(
        &mut self,
        descriptor: &'static irrigation_core::descriptor::ViewDescriptor,
    ) {
        self.inner.register_view(descriptor);
    }

    /// Stages a typed unit write.
    pub fn set<T>(&mut self, key: UnitKey, value: T)
    where
        T: PartialEq + Clone + Send + Sync + 'static,
    {
        self.updates.push(UnitUpdate {
            key,
            value: value_of(value),
            eq: values_equal::<T>,
        });
    }

    /// Stages a type-erased unit write.
    pub fn set_value(&mut self, key: UnitKey, value: Value, eq: fn(&Value, &Value) -> bool) {
        self.updates.push(UnitUpdate { key, value, eq });
    }

    /// Stages a replacement for a View's immutable content.
    pub fn set_content(
        &mut self,
        view: ViewId,
        content: Arc<dyn Any + Send + Sync>,
        eq: fn(&Value, &Value) -> bool,
    ) {
        self.set_value(
            UnitKey::field(view, irrigation_core::ids::FieldId(0), UnitKind::Content),
            content,
            eq,
        );
    }

    /// Reads the current committed value of a unit, if any.
    ///
    /// This is a transactional read, not a tracked observation: it exists so an
    /// editor can compute a delta from the state it is editing. A cell a
    /// whole-region replacement left stale is re-derived first, so the answer is
    /// the one the snapshot of that revision serves. A cell only the accessor
    /// that materialized it can re-derive has no such answer until that accessor
    /// reads it: the pre-replacement payload is not this revision's value.
    #[must_use]
    pub fn current<T: Clone + Any>(&self, key: &UnitKey) -> Option<T> {
        let mut state = self.inner.lock();
        let node = *state.units.get(key)?;
        if !Inner::refresh_cell(&mut state, key, node) {
            return None;
        }
        state
            .node(node)
            .value
            .as_ref()?
            .downcast_ref::<T>()
            .cloned()
    }

    /// Reads the current content of a View.
    ///
    /// # Panics
    ///
    /// Panics when the View is not registered or its content has another type.
    #[must_use]
    pub fn content<C: Any + Send + Sync>(&self, view: ViewId) -> Arc<C> {
        let state = self.inner.lock();
        let region = state
            .regions
            .get(&view)
            .unwrap_or_else(|| panic!("view {view:?} is not registered"));
        Arc::clone(&region.content)
            .downcast::<C>()
            .unwrap_or_else(|_| panic!("view {view:?} content has an unexpected type"))
    }

    /// Reads the content a new editor should start from.
    ///
    /// This is the committed content with the updates already staged in this
    /// transaction overlaid in staging order. A View edited twice in one
    /// transaction therefore sees the first editor's writes instead of
    /// silently starting from the committed content again.
    ///
    /// # Panics
    ///
    /// Panics when the View is not registered or its content has another type.
    #[must_use]
    pub fn staged_content<C: Any + Send + Sync>(&self, view: ViewId) -> Arc<C> {
        let (descriptor, mut content) = {
            let state = self.inner.lock();
            let region = state
                .regions
                .get(&view)
                .unwrap_or_else(|| panic!("view {view:?} is not registered"));
            (region.descriptor, Arc::clone(&region.content))
        };
        for update in &self.updates {
            if update.key.view != view {
                continue;
            }
            content = if update.key.kind == UnitKind::Content {
                Arc::clone(&update.value)
            } else {
                (descriptor.content.define)(&content, &update.key, Arc::clone(&update.value))
            };
        }
        content
            .downcast::<C>()
            .unwrap_or_else(|_| panic!("view {view:?} content has an unexpected type"))
    }

    /// Allocates a fresh entity identity for a source collection.
    pub fn alloc_entity(&mut self) -> u64 {
        self.inner.alloc_entity()
    }

    /// Applies the transaction, returning whether a new revision was published.
    ///
    /// # Errors
    ///
    /// Fails, leaving the runtime untouched, when the transaction writes a unit
    /// that belongs to a provider: every unit of a derived View, and any unit a
    /// live provider claim covers.
    pub(crate) fn commit(&mut self) -> Result<bool> {
        // Only the observer uses these; the commit path never branches on them.
        #[cfg(feature = "observe")]
        let started = std::time::Instant::now();
        #[cfg(feature = "observe")]
        let staged = self.updates.len() as u32;
        let mut state = self.inner.lock();
        check_host_writes(&state, &self.updates)?;
        let revision: Revision = state.revision + 1;
        let mut changed = Vec::new();
        // One transaction can write a unit more than once: a tree editor clears
        // a node and reinstalls it. Only the final value counts, both for the
        // published revision and for cutoff.
        let mut last: FxHashMap<&UnitKey, usize> = FxHashMap::default();
        for (index, update) in self.updates.iter().enumerate() {
            last.insert(&update.key, index);
        }
        let mut order: Vec<usize> = last.into_values().collect();
        order.sort_unstable();
        // A content replacement staged with the same value as the committed
        // content marks a transaction that intends to leave the View alone.
        // Unit writes staged alongside it are normalized against that content
        // below.
        let mut content_unchanged: FxHashMap<ViewId, bool> = FxHashMap::default();
        for update in &self.updates {
            if update.key.kind != UnitKind::Content {
                continue;
            }
            let same = state.units.get(&update.key).is_some_and(|node| {
                state
                    .node(*node)
                    .value
                    .as_ref()
                    .is_some_and(|old| (update.eq)(old, &update.value))
            });
            content_unchanged.insert(update.key.view, same);
        }
        // A transaction that stages unit writes beside a whole-region
        // replacement claims that those writes are the View's whole content
        // delta: every editor stages the cells it touched, so the replacement is
        // their sum. The publisher checks the claim per cell, so the claim is
        // only worth which cells this transaction writes — and only what the
        // write publishes: a staged cell write that normalizes away below
        // touches nothing, so it cannot stand in for a cell the replacement then
        // leaves alone.
        //
        // The decision is made once, here, against the state the transaction
        // found, and the publish loop consumes these answers. A write that
        // publishes nothing is a write whose final value is the state it found,
        // so it must not be re-measured after an earlier update in the same
        // transaction replaced the content it was measured against.
        let retractions = staged_retractions(&self.updates, &order);
        let mut installed: FxHashMap<ViewId, FxHashSet<UnitKey>> = FxHashMap::default();
        let mut payloads: FxHashMap<ViewId, FxHashSet<UnitKey>> = FxHashMap::default();
        let mut stated: FxHashMap<ViewId, bool> = FxHashMap::default();
        let mut publishes: Vec<bool> = Vec::with_capacity(order.len());
        for index in &order {
            let update = &self.updates[*index];
            // A removal owns its entity's payload. A `Value` write staged
            // beside the `Membership = false` states a value the transaction's
            // own presence — and the content it publishes — mark absent, so it
            // is not part of the final state: the retraction is decided from
            // the writes that publish, the same set the delta claim is taken
            // from. Other entity units stay ordinary writes; a graph stages
            // its adjacency index for the node it removes.
            let removed = update.key.kind == UnitKind::Value
                && update.key.entity.is_some()
                && retractions.contains(&owner_key(&update.key));
            let publishes_write = !removed
                && update.key.kind != UnitKind::Content
                && (retractions.contains(&update.key)
                    || unit_write_publishes(&state, update, &content_unchanged));
            publishes.push(publishes_write);
            if !publishes_write {
                continue;
            }
            // The delta names the units this transaction writes by their own
            // key. A retraction installs nothing, but it writes both cells of
            // the entity: the pair must stay exempt from the replacement, or it
            // would reopen a payload the retraction just cleared.
            let written = installed.entry(update.key.view).or_default();
            if retractions.contains(&update.key) {
                written.insert(payload_key(&update.key));
            }
            written.insert(update.key.clone());
            if !retractions.contains(&update.key) {
                // A plain write's payload must still be what the content this
                // commit publishes projects; a retraction already owns the
                // generation of the cells it clears.
                payloads
                    .entry(update.key.view)
                    .or_default()
                    .insert(update.key.clone());
            }
            stated.insert(update.key.view, true);
        }
        let mut replaced_views: Vec<ViewId> = Vec::new();
        for (index, publishes_write) in order.into_iter().zip(publishes) {
            let update = &self.updates[index];
            if update.key.kind == UnitKind::Content {
                // A registered View's whole content is its field-0 content
                // cell, and it publishes that cell together with the
                // structure derived from it. A namespace with no region yet,
                // and a content-kind key that is not that cell, has only the
                // unit itself to write.
                let canonical = update.key.field == irrigation_core::ids::FieldId(0);
                let none: FxHashSet<UnitKey> = FxHashSet::default();
                let delta = CellDelta {
                    installed: installed.get(&update.key.view).unwrap_or(&none),
                    stated: stated.get(&update.key.view) == Some(&true),
                };
                if canonical
                    && Inner::publish_region_content(
                        &mut state,
                        update.key.view,
                        Arc::clone(&update.value),
                        update.eq,
                        revision,
                        delta,
                        &mut changed,
                    )
                {
                    replaced_views.push(update.key.view);
                    continue;
                }
                let node = match state.units.get(&update.key) {
                    Some(node) => *node,
                    None => {
                        let record = NodeRecord::unit("view content", update.key.clone());
                        let node = state.alloc(record);
                        state.units.insert(Arc::new(update.key.clone()), node);
                        node
                    }
                };
                let same = state
                    .node(node)
                    .value
                    .as_ref()
                    .is_some_and(|old| (update.eq)(old, &update.value));
                if !same {
                    let value = Arc::clone(&update.value);
                    let value_version = state.alloc_value_version();
                    state
                        .node_mut(node)
                        .install_value(Some(value), value_version, revision);
                    changed.push(node);
                }
                continue;
            }
            if retractions.contains(&update.key) {
                // Retraction mirrors installation: the primitive clears the
                // entity's payload cell and derives the membership cell from it,
                // whichever of the two the editor staged.
                Inner::retract_entity(&mut state, &update.key, revision, &mut changed);
                continue;
            }
            if !publishes_write {
                continue;
            }
            let node = match state.units.get(&update.key) {
                Some(node) => *node,
                None => Inner::ensure_unit_node(&mut state, update.key.clone(), None),
            };
            let value_version = state.alloc_value_version();
            state.node_mut(node).install_value(
                Some(Arc::clone(&update.value)),
                value_version,
                revision,
            );
            changed.push(node);
        }
        // A cell write is measured against the state the transaction found, so
        // one can publish a payload the replacement it commits does not
        // project. The pass runs after the whole publish loop: a write staged
        // after the replacement is installed after it too.
        if !replaced_views.is_empty() {
            for view in replaced_views {
                let Some(written) = payloads.get(&view) else {
                    continue;
                };
                Inner::reconcile_written_cells(&mut state, view, written, revision, &mut changed);
            }
        }
        if changed.is_empty() {
            #[cfg(feature = "observe")]
            let current = state.revision;
            drop(state);
            // A no-op transaction is a real outcome: the page must be able to
            // say "no revision published" rather than showing nothing.
            #[cfg(feature = "observe")]
            self.inner
                .emit(crate::observe::RuntimeEvent::TransactionCommitted {
                    label: Arc::from(self.label.as_str()),
                    revision: current,
                    staged,
                    changed: 0,
                    published: false,
                    invalidated: 0,
                    views: smallvec::SmallVec::new(),
                    duration_ns: started.elapsed().as_nanos() as u64,
                });
            return Ok(false);
        }
        state.revision = revision;
        // Conflicts belong to the revision that produced them.
        state.pending_conflicts.clear();
        self.inner.publish_revision(revision);
        state.stats.revisions.fetch_add(1, Ordering::Relaxed);
        let invalidate_started = std::time::Instant::now();
        let invalidated = invalidate(&mut state, changed.iter().copied(), revision);
        state.stats.invalidation_ns.fetch_add(
            invalidate_started.elapsed().as_nanos() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
        let seeds = changed;
        drop(state);
        #[cfg(feature = "observe")]
        {
            let mut views: smallvec::SmallVec<[irrigation_core::ids::ViewId; 8]> =
                smallvec::SmallVec::new();
            {
                let state = self.inner.lock();
                for node in &seeds {
                    let record = state.nodes.get(node.index());
                    let Some(unit) = record.and_then(|record| record.unit.as_ref()) else {
                        continue;
                    };
                    if views.len() < 8 && !views.contains(&unit.view) {
                        views.push(unit.view);
                    }
                }
            }
            self.inner
                .emit(crate::observe::RuntimeEvent::TransactionCommitted {
                    label: Arc::from(self.label.as_str()),
                    revision,
                    staged,
                    changed: seeds.len() as u32,
                    published: true,
                    invalidated,
                    views,
                    duration_ns: started.elapsed().as_nanos() as u64,
                });
            self.inner.emit_invalidated(revision, &seeds, invalidated);
        }
        #[cfg(not(feature = "observe"))]
        {
            let _ = (seeds, invalidated);
        }
        Ok(true)
    }
}

/// Rejects a write to a unit that is a provider's to publish.
///
/// A derived View's content and units belong to the computation that publishes
/// them, and a provider may define a unit in any registered View, source
/// included. Publishing a host value over one leaves the region content and the
/// unit table disagreeing at the same revision, and writes past the duplicate-
/// definition arbitration that keeps one definition per unit.
fn check_host_writes(state: &State, updates: &[UnitUpdate]) -> Result<()> {
    for update in updates {
        // An unregistered View has no descriptor to classify it; the raw edit
        // path stays open for hosts that seed one themselves.
        if let Some(descriptor) = state.views.get(&update.key.view)
            && descriptor.kind == ViewKind::Derived
        {
            return Err(ComputeError::DefinitionDenied {
                unit: format!("{:?}", update.key),
                reason: String::from("a derived View's units are defined by its provider"),
            });
        }
        // A content replacement names the View, not any cell a provider owns,
        // so the per-key test below cannot see what it would overwrite.
        if update.key.kind == UnitKind::Content {
            check_content_replacement(state, update)?;
            continue;
        }
        if state.unit_owned_by_live_provider(&update.key) {
            return Err(ComputeError::DefinitionDenied {
                unit: format!("{:?}", update.key),
                reason: String::from("the unit is owned by a live provider"),
            });
        }
    }
    Ok(())
}

/// Rejects a content replacement that would overwrite a provider-owned cell.
///
/// A provider's claim is recorded against the cell it defines, while a
/// replacement is staged as one whole-View value. The refusal therefore decides
/// per owned cell: each one is restored from the committed content into a mask,
/// and the replacement is accepted only when it already agrees with that mask.
/// Host-owned cells are the replacement's own, so an edit that leaves the
/// provider's cells alone still commits.
fn check_content_replacement(state: &State, update: &UnitUpdate) -> Result<()> {
    let Some(region) = state.regions.get(&update.key.view) else {
        return Ok(());
    };
    if region.owners.is_empty() {
        return Ok(());
    }
    let descriptor = region.descriptor;
    let mut masked = Arc::clone(&update.value);
    for (key, slot) in region.units.iter() {
        let Some(unit) = key.as_any().downcast_ref::<UnitKey>() else {
            continue;
        };
        // A structure is the field's projection and content has no cell of its
        // own; neither is a provider-owned cell a replacement could overwrite.
        if matches!(unit.kind, UnitKind::Content | UnitKind::Structure) {
            continue;
        }
        if !state.unit_owned_by_live_provider(unit) {
            continue;
        }
        if unit.kind == UnitKind::Membership {
            // Presence is not a payload: the replacement either keeps the
            // provider's membership or it does not.
            if (descriptor.content.contains)(&region.content, unit)
                != (descriptor.content.contains)(&masked, unit)
            {
                return Err(content_denial(update, Some(unit)));
            }
            continue;
        }
        // A whole-field `Value` cell has no entity, so the content vtable does
        // not answer it: `get` reads entity cells, and `retract` would empty
        // the field. The committed value is the provider's own staged one,
        // held by the unit node — the same draft the content was defined from.
        // Retracting it instead would refuse a replacement that agrees with
        // the provider, and would accept one that empties its field.
        let committed = (descriptor.content.get)(&region.content, unit).or_else(|| {
            if unit.entity.is_none() && unit.kind == UnitKind::Value {
                state.node(slot.node).value.clone()
            } else {
                None
            }
        });
        masked = match committed {
            Some(committed) => (descriptor.content.define)(&masked, unit, committed),
            None => (descriptor.content.retract)(&masked, unit),
        };
    }
    if (descriptor.content.eq)(&masked, &update.value) {
        Ok(())
    } else {
        Err(content_denial(update, None))
    }
}

/// The refusal a content replacement gets for one provider-owned cell, or for
/// the replacement as a whole when the mask cannot name a single cell.
fn content_denial(update: &UnitUpdate, cell: Option<&UnitKey>) -> ComputeError {
    let reason = match cell {
        Some(cell) => {
            format!("the content would replace {cell:?}, which a live provider owns")
        }
        None => String::from("the content would replace a unit a live provider owns"),
    };
    ComputeError::DefinitionDenied {
        unit: format!("{:?}", update.key),
        reason,
    }
}

/// Whether one staged unit write would publish a new value.
///
/// This is the normalization decision for one write, and the pre-pass in
/// [`Edit::commit`] makes it exactly once per staged write. The publish loop
/// consumes the stored answer instead of asking again: by the time it reaches a
/// write, an earlier update in the same transaction may have replaced the
/// content this function measured the write against.
///
/// A write that publishes nothing must not be counted as a cell a whole-region
/// replacement already stated.
///
/// When the unit has a committed cell of its own, the cell's value decides.
/// A missing unit has no such value: its previous state is the View content's,
/// so the content decides — and only when this transaction leaves that content
/// alone, because otherwise the write defines the state on top of the new
/// content rather than restating the old one.
fn unit_write_publishes(
    state: &State,
    update: &UnitUpdate,
    content_unchanged: &FxHashMap<ViewId, bool>,
) -> bool {
    if let Some(node) = state.units.get(&update.key) {
        return state
            .node(*node)
            .value
            .as_ref()
            .is_none_or(|old| !(update.eq)(old, &update.value));
    }
    let Some(region) = state.regions.get(&update.key.view) else {
        return true;
    };
    let unchanged = content_unchanged.get(&update.key.view) == Some(&true);
    // A field-level structure unit is the family's projection of the content:
    // the payload every writer installs, and never the storage `get` answers
    // the same key with. It is bound once and decides both whether the write
    // restates the committed state and whether it matches the committed
    // content.
    let projection = if update.key.kind == UnitKind::Structure && update.key.entity.is_none() {
        (region.descriptor.content.structure)(&region.content, update.key.field)
    } else {
        None
    };
    let same = unchanged
        && match &projection {
            Some(structure) => (structure.eq)(&structure.value, &update.value),
            None => match update.key.kind {
                UnitKind::Membership => {
                    let present =
                        (region.descriptor.content.contains)(&region.content, &update.key);
                    (update.eq)(&value_of(present), &update.value)
                }
                _ => match (region.descriptor.content.get)(&region.content, &update.key) {
                    Some(current) => (update.eq)(&current, &update.value),
                    None => {
                        // The View may encode absence as an ordinary payload; a
                        // write is a no-op exactly when defining it leaves the
                        // committed content unchanged.
                        let next = (region.descriptor.content.define)(
                            &region.content,
                            &update.key,
                            Arc::clone(&update.value),
                        );
                        (region.descriptor.content.eq)(&region.content, &next)
                    }
                },
            },
        };
    // A missing unit may still be present in unchanged content; compare against
    // it so setting the default is a no-op.
    !same
        && !match &projection {
            Some(structure) => (structure.eq)(&structure.value, &update.value),
            None => (region.descriptor.content.get)(&region.content, &update.key)
                .as_ref()
                .is_some_and(|current| (update.eq)(current, &update.value)),
        }
}

/// The entities a transaction retracts: every staged `Membership = false`.
///
/// Collection editors stage an entity's cells separately, so a removal names
/// the retraction through the membership cell while the payload write staged
/// beside it names the value the entity had. One entity has one final state:
/// the removal owns both cells, and [`Edit::commit`] drops the payload write
/// instead of publishing a value its own presence marks absent. Leaving that
/// payload certified behind an absent member would let a later membership
/// re-arm serve the removed value. The kernel owns the one retraction policy;
/// this decides which entities the edit path hands it.
fn staged_retractions(updates: &[UnitUpdate], order: &[usize]) -> FxHashSet<UnitKey> {
    order
        .iter()
        .filter_map(|index| {
            let update = &updates[*index];
            if update.key.kind != UnitKind::Membership
                || update.key.entity.is_none()
                || update.value.downcast_ref::<bool>() != Some(&false)
            {
                return None;
            }
            Some(update.key.clone())
        })
        .collect()
}

/// Compares two type-erased values of the same concrete type.
fn values_equal<T: PartialEq + Any>(left: &Value, right: &Value) -> bool {
    match (left.downcast_ref::<T>(), right.downcast_ref::<T>()) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}
