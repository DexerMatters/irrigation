//! Edit transactions.
//!
//! An edit stages unit updates against a known source snapshot. At commit the
//! runtime normalizes the transaction by comparing final values, publishes a
//! new revision, and invalidates the affected dependency cone. A transaction
//! whose final state equals the previous state publishes nothing.

use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use reactive_core::error::Result;
use reactive_core::ids::{Revision, ViewId};
use reactive_core::value::{Value, value_of};
use reactive_store::descriptor::{UnitKey, UnitKind};

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

    /// Labels the transaction for diagnostics.
    pub fn set_label(&mut self, label: impl Into<String>) {
        self.label = label.into();
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
        state.node(*node).value.as_ref()?.downcast_ref::<T>().cloned()
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

    /// Descriptions of the staged unit updates, for diagnostics and tests.
    #[must_use]
    pub fn staged_units(&self) -> Vec<String> {
        self.updates
            .iter()
            .map(|update| format!("{:?}", update.key))
            .collect()
    }

    /// The number of staged updates.
    #[must_use]
    pub fn update_count(&self) -> usize {
        self.updates.len()
    }

    /// Applies the transaction, returning whether a new revision was published.
    pub(crate) fn commit(&mut self) -> Result<bool> {
        if self.updates.is_empty() {
            return Ok(false);
        }
        let mut state = self.inner.lock();
        let revision: Revision = state.revision + 1;
        let mut changed = Vec::new();
        for update in &self.updates {
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
                    let current = state
                        .regions
                        .get(&update.key.view)
                        .and_then(|region| {
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
            return Ok(false);
        }
        state.revision = revision;
        self.inner.publish_revision(revision);
        state.stats.revisions.fetch_add(1, Ordering::Relaxed);
        invalidate(&mut state, changed, revision);
        Ok(true)
    }
}

/// Compares two type-erased values of the same concrete type.
fn values_equal<T: PartialEq + Any>(left: &Value, right: &Value) -> bool {
    match (left.downcast_ref::<T>(), right.downcast_ref::<T>()) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}


