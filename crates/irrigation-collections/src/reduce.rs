//! Reduced sequence fields.
//!
//! A `Reduced<R>` field stores an [`AggSeq`] whose cached aggregate is the
//! declared lawful reduction `R`. A point edit therefore recomputes only a
//! logarithmic ancestor path, and reading the aggregate is constant time.

use std::any::Any;
use std::marker::PhantomData;

use irrigation_core::descriptor::{FieldKind, UnitKey, UnitKind};
use irrigation_core::error::{ComputeError, PreconditionReason};
use irrigation_core::ids::{FieldId, ViewId};
use irrigation_core::ops::panic_engine;
use irrigation_core::value::{Value, value_of};
use irrigation_runtime::edit::Edit;
use irrigation_runtime::field::{ViewFieldHandle, structure_unit};
use irrigation_runtime::helpers::{define_unit, read_unit_value};
use irrigation_store::aggregate::AggSeq;
use irrigation_store::field::ViewField;

/// A declared lawful reduction.
///
/// The identity and the associative combine are explicit: the plan forbids
/// silently reassociating floating-point addition or an arbitrary closure.
pub trait Reduction: Clone + PartialEq + Send + Sync + 'static {
    /// Element type.
    type Item: Clone + PartialEq + Send + Sync + 'static;
    /// Aggregate value type.
    type Aggregate: Clone + PartialEq + Send + Sync + 'static;

    /// Projects one element into the aggregate domain.
    fn leaf(item: &Self::Item) -> Self::Aggregate;

    /// Associative combination of two aggregates.
    fn combine(left: &Self::Aggregate, right: &Self::Aggregate) -> Self::Aggregate;

    /// Identity of the reduction.
    fn identity() -> Self::Aggregate;
}

/// Content accessor for a reduced sequence.
type ReducedContent<R> =
    fn(&dyn Any) -> AggSeq<<R as Reduction>::Aggregate, <R as Reduction>::Item>;

/// A sequence field with a cached lawful reduction.
pub struct Reduced<R>(PhantomData<fn() -> R>);

impl<R> std::fmt::Debug for Reduced<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Reduced")
    }
}

impl<R: Reduction> ViewField for Reduced<R> {
    type Storage = AggSeq<R::Aggregate, R::Item>;

    const KIND: FieldKind = FieldKind::List;

    fn empty() -> Self::Storage {
        AggSeq::new(R::leaf, R::combine, R::identity())
    }

    fn len(storage: &Self::Storage) -> Option<usize> {
        Some(storage.len())
    }

    fn define(
        storage: &Self::Storage,
        key: &dyn irrigation_core::key::AnyKey,
        value: Value,
    ) -> Self::Storage {
        if let Some(sequence) = value.downcast_ref::<AggSeq<R::Aggregate, R::Item>>() {
            return sequence.clone();
        }
        let Some(item) = value.downcast_ref::<R::Item>() else {
            return storage.clone();
        };
        match key.as_any().downcast_ref::<u64>() {
            // A positional definition is total like every sibling family: an
            // index past the end is ignored instead of tripping `AggSeq::insert`,
            // which is already how `retract` treats the same key.
            Some(index) if *index as usize <= storage.len() => {
                storage.insert(*index as usize, item.clone())
            }
            Some(_) => storage.clone(),
            None => storage.push(item.clone()),
        }
    }

    fn retract(storage: &Self::Storage, key: &dyn irrigation_core::key::AnyKey) -> Self::Storage {
        match key.as_any().downcast_ref::<u64>() {
            Some(index) => storage.remove(*index as usize).0,
            None => AggSeq::new(R::leaf, R::combine, R::identity()),
        }
    }

    fn contains(_storage: &Self::Storage, _key: &dyn irrigation_core::key::AnyKey) -> bool {
        true
    }

    fn structure(storage: &Self::Storage) -> Option<irrigation_core::descriptor::Structure> {
        Some(irrigation_core::descriptor::Structure {
            value: value_of(storage.clone()),
            eq: |left, right| {
                left.downcast_ref::<AggSeq<R::Aggregate, R::Item>>()
                    == right.downcast_ref::<AggSeq<R::Aggregate, R::Item>>()
            },
        })
    }

    fn get(storage: &Self::Storage, key: &dyn irrigation_core::key::AnyKey) -> Option<Value> {
        match key.as_any().downcast_ref::<u64>() {
            Some(index) => storage
                .get(*index as usize)
                .map(|item| value_of(item.clone())),
            None => Some(value_of(storage.aggregate())),
        }
    }

    fn summary(storage: &Self::Storage) -> Option<Value> {
        Some(value_of(storage.aggregate()))
    }

    fn unit_eq(old: &Self::Storage, new: &Self::Storage, unit: &UnitKey) -> Option<bool> {
        match unit.kind {
            UnitKind::Structure => Some(old == new),
            UnitKind::Value => match unit.entity.as_deref() {
                None => Some(old == new),
                Some(entity) => {
                    let index = entity.as_any().downcast_ref::<u64>()?;
                    Some(old.get(*index as usize) == new.get(*index as usize))
                }
            },
            _ => None,
        }
    }
}

/// Tracked read handle for a reduced sequence field.
pub struct ReducedRef<R: Reduction> {
    view: ViewId,
    field: FieldId,
    content: ReducedContent<R>,
}

impl<R: Reduction> Copy for ReducedRef<R> {}

impl<R: Reduction> Clone for ReducedRef<R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<R: Reduction> PartialEq for ReducedRef<R> {
    fn eq(&self, other: &Self) -> bool {
        self.view == other.view && self.field == other.field
    }
}

impl<R: Reduction> Eq for ReducedRef<R> {}

impl<R: Reduction> std::fmt::Debug for ReducedRef<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReducedRef").finish_non_exhaustive()
    }
}

impl<R: Reduction> ReducedRef<R> {
    /// Builds a handle for one reduced field.
    #[must_use]
    pub const fn new(view: ViewId, field: FieldId, content: ReducedContent<R>) -> Self {
        Self {
            view,
            field,
            content,
        }
    }

    fn sequence(&self) -> AggSeq<R::Aggregate, R::Item> {
        read_unit_value(structure_unit(self.view, self.field), |content| {
            Some((self.content)(content))
        })
    }

    /// The cached aggregate.
    #[must_use]
    pub fn aggregate(&self) -> R::Aggregate {
        self.sequence().aggregate()
    }

    /// Number of elements.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sequence().len()
    }

    /// Returns `true` when the sequence is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Reads one element.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<R::Item> {
        self.sequence().get(index).cloned()
    }

    /// Defines the whole sequence from the currently executing computation.
    #[track_caller]
    pub fn define_all<I>(&self, items: I, label: &'static str)
    where
        I: IntoIterator<Item = R::Item>,
    {
        irrigation_runtime::helpers::claim_field(self.view, self.field, label);
        let mut sequence = AggSeq::new(R::leaf, R::combine, R::identity());
        for item in items {
            sequence = sequence.push(item);
        }
        define_unit(
            UnitKey::field(self.view, self.field, UnitKind::Value),
            sequence,
            label,
        );
    }

    /// Reads every element in order.
    #[must_use]
    pub fn to_vec(&self) -> Vec<R::Item> {
        self.sequence().to_vec()
    }
}

impl<R: Reduction> ViewFieldHandle for Reduced<R> {
    type Handle = ReducedRef<R>;
    type Editor<'a> = ReducedEditor<'a, R>;

    fn handle(
        view: ViewId,
        field: FieldId,
        content: fn(&dyn Any) -> Self::Storage,
    ) -> Self::Handle {
        ReducedRef::new(view, field, content)
    }

    fn editor<'a>(
        edit: &'a mut Edit,
        view: ViewId,
        field: FieldId,
        storage: &'a mut Self::Storage,
    ) -> Self::Editor<'a> {
        ReducedEditor {
            edit,
            view,
            field,
            storage,
        }
    }
}

/// Transactional editor for a reduced sequence field.
pub struct ReducedEditor<'a, R: Reduction> {
    edit: &'a mut Edit,
    view: ViewId,
    field: FieldId,
    storage: &'a mut AggSeq<R::Aggregate, R::Item>,
}

impl<R: Reduction> std::fmt::Debug for ReducedEditor<'_, R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReducedEditor").finish_non_exhaustive()
    }
}

impl<R: Reduction> ReducedEditor<'_, R> {
    /// Inserts one element, failing when `index` is past the end.
    #[track_caller]
    pub fn insert(&mut self, index: usize, item: R::Item) {
        self.require_range(index, false);
        *self.storage = self.storage.insert(index, item.clone());
        self.edit.set(
            UnitKey::entity(
                self.view,
                self.field,
                UnitKind::Value,
                irrigation_core::key::any_key(index as u64),
            ),
            item,
        );
        self.stage();
    }

    /// Appends one element.
    pub fn push(&mut self, item: R::Item) {
        let index = self.storage.len();
        self.insert(index, item);
    }

    /// Removes one element.
    pub fn remove(&mut self, index: usize) {
        self.require_range(index, true);
        let (next, _) = self.storage.remove(index);
        *self.storage = next;
        // Identity here is the position, so a removal renumbers everything after
        // it: those cells have to be restaged with their new values, and the cell
        // at the old last position no longer exists. Restaging alone would leave
        // it serving an element the field no longer holds anywhere.
        let len = self.storage.len();
        for position in index..len {
            if let Some(item) = self.storage.get(position).cloned() {
                self.edit.set(
                    UnitKey::entity(
                        self.view,
                        self.field,
                        UnitKind::Value,
                        irrigation_core::key::any_key(position as u64),
                    ),
                    item,
                );
            }
        }
        self.edit.retract(UnitKey::entity(
            self.view,
            self.field,
            UnitKind::Value,
            irrigation_core::key::any_key(len as u64),
        ));
        self.stage();
    }

    /// Replaces one element, failing when `index` is out of range.
    #[track_caller]
    pub fn set(&mut self, index: usize, item: R::Item) {
        self.require_range(index, true);
        *self.storage = self.storage.set(index, item.clone());
        self.edit.set(
            UnitKey::entity(
                self.view,
                self.field,
                UnitKind::Value,
                irrigation_core::key::any_key(index as u64),
            ),
            item,
        );
        self.stage();
    }

    /// Fails with the engine's precondition error when `index` cannot be
    /// addressed, matching the other derived editors.
    #[track_caller]
    fn require_range(&self, index: usize, present: bool) {
        let in_range = if present {
            index < self.storage.len()
        } else {
            index <= self.storage.len()
        };
        if in_range {
            return;
        }
        let condition = if present {
            format!("reduced index {index} present")
        } else {
            format!("reduced index {index} in range")
        };
        panic_engine(ComputeError::PreconditionUnmet {
            condition,
            ports: vec![format!("{:?}", structure_unit(self.view, self.field))],
            reason: PreconditionReason::ValueMismatch,
        });
    }

    fn stage(&mut self) {
        let sequence = self.storage.clone();
        self.edit
            .set(structure_unit(self.view, self.field), sequence);
    }
}

#[cfg(test)]
#[path = "../tests/unit/reduce/tests.rs"]
mod tests;
