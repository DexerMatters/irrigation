//! Unit tests for `src/field.rs`.

use super::*;

#[test]
fn scalar_unit_eq_compares_the_whole_value() {
    let view = irrigation_core::ids::ViewId(1);
    let field = irrigation_core::ids::FieldId(0);
    let value = irrigation_core::descriptor::UnitKey::field(
        view,
        field,
        irrigation_core::descriptor::UnitKind::Value,
    );
    assert_eq!(
        <Scalar<u32> as ViewField>::unit_eq(&1, &1, &value),
        Some(true)
    );
    assert_eq!(
        <Scalar<u32> as ViewField>::unit_eq(&1, &2, &value),
        Some(false)
    );
}

#[test]
fn scalar_field_round_trips_values() {
    let empty = <Scalar<u32> as ViewField>::empty();
    assert_eq!(empty, 0);
    let defined = <Scalar<u32> as ViewField>::define(&empty, &(), value_of(7u32));
    assert_eq!(defined, 7);
    assert_eq!(
        <Scalar<u32> as ViewField>::get(&defined, &())
            .and_then(|value| value.downcast_ref::<u32>().copied()),
        Some(7)
    );
}

#[test]
fn scalar_field_ignores_a_mismatched_value() {
    let scalar = <Scalar<u32> as ViewField>::define(&0, &(), value_of(7u32));
    assert_eq!(
        <Scalar<u32> as ViewField>::define(&scalar, &(), value_of("seven")),
        7,
        "a mismatched value must not reset the storage"
    );
}

#[test]
fn scalar_field_retracts_to_empty() {
    assert_eq!(<Scalar<u32> as ViewField>::retract(&7, &()), 0);
}

use irrigation_core::value::value_of;
