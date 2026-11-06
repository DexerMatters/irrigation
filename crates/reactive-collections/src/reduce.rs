//! Reduced sequence fields.
//!
//! A `Reduced<R>` field stores an [`AggSeq`] whose cached aggregate is the
//! declared lawful reduction `R`. A point edit therefore recomputes only a
//! logarithmic ancestor path, and reading the aggregate is constant time.

use std::any::Any;
use std::marker::PhantomData;

use reactive_core::descriptor::{FieldKind, UnitKey, UnitKind};
use reactive_core::ids::{FieldId, ViewId};
use reactive_core::value::{Value, value_of};
use reactive_runtime::edit::Edit;
use reactive_runtime::field::{ViewFieldHandle, structure_unit};
use reactive_runtime::helpers::{define_unit, read_unit_value};
use reactive_store::aggregate::AggSeq;
use reactive_store::field::ViewField;

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
        key: &dyn reactive_core::key::AnyKey,
        value: Value,
    ) -> Self::Storage {
        if let Some(sequence) = value.downcast_ref::<AggSeq<R::Aggregate, R::Item>>() {
            return sequence.clone();
        }
        let Some(item) = value.downcast_ref::<R::Item>() else {
            return storage.clone();
        };
        match key.as_any().downcast_ref::<u64>() {
            Some(index) => storage.insert(*index as usize, item.clone()),
            None => storage.push(item.clone()),
        }
    }

    fn retract(storage: &Self::Storage, key: &dyn reactive_core::key::AnyKey) -> Self::Storage {
        match key.as_any().downcast_ref::<u64>() {
            Some(index) => storage.remove(*index as usize).0,
            None => AggSeq::new(R::leaf, R::combine, R::identity()),
        }
    }

    fn contains(_storage: &Self::Storage, _key: &dyn reactive_core::key::AnyKey) -> bool {
        true
    }

    fn structure(storage: &Self::Storage) -> Option<reactive_core::descriptor::Structure> {
        Some(reactive_core::descriptor::Structure {
            value: value_of(storage.clone()),
            eq: |left, right| {
                left.downcast_ref::<AggSeq<R::Aggregate, R::Item>>()
                    == right.downcast_ref::<AggSeq<R::Aggregate, R::Item>>()
            },
        })
    }

    fn get(storage: &Self::Storage, key: &dyn reactive_core::key::AnyKey) -> Option<Value> {
        match key.as_any().downcast_ref::<u64>() {
            Some(index) => storage
                .get(*index as usize)
                .map(|item| value_of(item.clone())),
            None => Some(value_of(storage.aggregate())),
        }
    }
}

/// Tracked read handle for a reduced sequence field.
#[derive(Clone)]
pub struct ReducedRef<R: Reduction> {
    view: ViewId,
    field: FieldId,
    content: ReducedContent<R>,
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
    pub fn new(view: ViewId, field: FieldId, content: ReducedContent<R>) -> Self {
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
    /// Inserts one element.
    pub fn insert(&mut self, index: usize, item: R::Item) {
        *self.storage = self.storage.insert(index, item.clone());
        self.edit.set(
            UnitKey::entity(
                self.view,
                self.field,
                UnitKind::Value,
                reactive_core::key::any_key(index as u64),
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
        let (next, _) = self.storage.remove(index);
        *self.storage = next;
        self.stage();
    }

    /// Replaces one element.
    pub fn set(&mut self, index: usize, item: R::Item) {
        *self.storage = self.storage.set(index, item.clone());
        self.edit.set(
            UnitKey::entity(
                self.view,
                self.field,
                UnitKind::Value,
                reactive_core::key::any_key(index as u64),
            ),
            item,
        );
        self.stage();
    }

    fn stage(&mut self) {
        let sequence = self.storage.clone();
        self.edit
            .set(structure_unit(self.view, self.field), sequence);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reactive_store::aggregate::AggSeq;

    #[derive(Clone, PartialEq)]
    struct Sum;

    impl Reduction for Sum {
        type Item = u64;
        type Aggregate = u64;

        fn leaf(item: &u64) -> u64 {
            *item
        }

        fn combine(left: &u64, right: &u64) -> u64 {
            *left + *right
        }

        fn identity() -> u64 {
            0
        }
    }

    #[test]
    fn reduced_structure_projects_the_sequence() {
        let sequence: AggSeq<u64, u64> = AggSeq::new(Sum::leaf, Sum::combine, Sum::identity())
            .push(1)
            .push(2);
        let structure = <Reduced<Sum> as ViewField>::structure(&sequence).expect("structure");
        let projected = structure
            .value
            .downcast_ref::<AggSeq<u64, u64>>()
            .expect("aggregate sequence");
        assert_eq!(projected.to_vec(), vec![1, 2]);
        assert!((structure.eq)(
            &structure.value,
            &reactive_core::value::value_of(projected.clone())
        ));
    }
}
