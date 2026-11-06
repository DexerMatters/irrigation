//! Edit transactions.
//!
//! An edit stages unit updates against a known source snapshot. At commit the
//! runtime normalizes the transaction by comparing final values, publishes a
//! new revision, and invalidates the affected dependency cone. A transaction
//! whose final state equals the previous state publishes nothing.

use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use reactive_core::descriptor::{UnitKey, UnitKind};
use reactive_core::hash::FxHashMap;
use reactive_core::ids::{Revision, ViewId};
use reactive_core::value::{Value, value_of};

use crate::inner::Inner;
use crate::snapshot::EditableView;
use crate::state::{NodeRecord, NodeState, invalidate};

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

    /// Registers a View namespace if it is not registered yet.
    pub fn ensure_view(&mut self, descriptor: &'static reactive_core::descriptor::ViewDescriptor) {
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
            UnitKey::field(view, reactive_core::ids::FieldId(0), UnitKind::Content),
            content,
            eq,
        );
    }

    /// Reads the current committed value of a unit, if any.
    ///
    /// This is a transactional read, not a tracked observation: it exists so an
    /// editor can compute a delta from the state it is editing.
    #[must_use]
    pub fn current<T: Clone + Any>(&self, key: &UnitKey) -> Option<T> {
        let state = self.inner.lock();
        let node = state.units.get(key)?;
        state
            .node(*node)
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

    /// Allocates a fresh entity identity for a source collection.
    pub fn alloc_entity(&mut self) -> u64 {
        self.inner.alloc_entity()
    }

    /// Applies the transaction, returning whether a new revision was published.
    pub(crate) fn commit(&mut self) -> bool {
        // Only the observer uses these; the commit path never branches on them.
        #[cfg(feature = "observe")]
        let started = std::time::Instant::now();
        #[cfg(feature = "observe")]
        let staged = self.updates.len() as u32;
        let mut state = self.inner.lock();
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
        for index in order {
            let update = &self.updates[index];
            if update.key.kind == UnitKind::Content {
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
                    let record = state.node_mut(node);
                    record.value = Some(value);
                    record.changed_at = revision;
                    record.verified_at = revision;
                    record.suspect_at = 0;
                    record.state = NodeState::Ready;
                    changed.push(node);
                }
                if let Some(region) = state.regions.get_mut(&update.key.view) {
                    region.content = Arc::clone(&update.value);
                }
                continue;
            }
            let node = match state.units.get(&update.key) {
                Some(node) => *node,
                None => {
                    // A missing unit has no node; compare against the content's
                    // current value so setting the default is a no-op.
                    let current = state.regions.get(&update.key.view).and_then(|region| {
                        (region.descriptor.content.get)(&region.content, &update.key)
                    });
                    if current
                        .as_ref()
                        .is_some_and(|current| (update.eq)(current, &update.value))
                    {
                        continue;
                    }
                    let record = NodeRecord::unit("unit", update.key.clone());
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
            if same {
                continue;
            }
            {
                let record = state.node_mut(node);
                record.value = Some(Arc::clone(&update.value));
                record.changed_at = revision;
                record.verified_at = revision;
                record.suspect_at = 0;
                record.state = NodeState::Ready;
                record.failure = None;
            }
            changed.push(node);
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
            return false;
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
            let mut views: smallvec::SmallVec<[reactive_core::ids::ViewId; 8]> =
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
        true
    }
}

/// Compares two type-erased values of the same concrete type.
fn values_equal<T: PartialEq + Any>(left: &Value, right: &Value) -> bool {
    match (left.downcast_ref::<T>(), right.downcast_ref::<T>()) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}
