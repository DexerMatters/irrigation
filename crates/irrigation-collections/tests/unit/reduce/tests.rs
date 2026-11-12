//! Unit tests for `src/reduce.rs`.

use super::*;
use irrigation_store::aggregate::AggSeq;

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
fn reduced_unit_eq_compares_the_sequence_or_one_item() {
    use irrigation_core::key::any_key;

    let view = ViewId(1);
    let field = FieldId(0);
    let old: AggSeq<u64, u64> = AggSeq::new(Sum::leaf, Sum::combine, Sum::identity()).push(1);
    let same: AggSeq<u64, u64> = AggSeq::new(Sum::leaf, Sum::combine, Sum::identity()).push(1);
    let next: AggSeq<u64, u64> = AggSeq::new(Sum::leaf, Sum::combine, Sum::identity()).push(2);
    let structure = UnitKey::field(view, field, UnitKind::Structure);
    let value = UnitKey::entity(view, field, UnitKind::Value, any_key(0u64));
    assert_eq!(
        <Reduced<Sum> as ViewField>::unit_eq(&old, &same, &structure),
        Some(true)
    );
    assert_eq!(
        <Reduced<Sum> as ViewField>::unit_eq(&old, &next, &structure),
        Some(false)
    );
    assert_eq!(
        <Reduced<Sum> as ViewField>::unit_eq(&old, &next, &value),
        Some(false)
    );
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
        &irrigation_core::value::value_of(projected.clone())
    ));
}
