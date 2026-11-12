//! Unit tests for `src/ops.rs`.

use super::*;

#[test]
fn engine_panic_round_trips_through_catch() {
    let error = catch_engine(|| panic_engine(ComputeError::Cancelled)).unwrap_err();
    assert_eq!(error, ComputeError::Cancelled);
}

#[test]
fn user_panic_is_reported_as_panic() {
    let error = catch_engine(|| panic!("bad input")).unwrap_err();
    assert!(matches!(error, ComputeError::Panicked { message } if message.contains("bad input")));
}

#[test]
fn eval_outside_a_computation_panics_with_engine_payload() {
    let error = catch_engine(|| eval(SiteId(1), || 1u32)).unwrap_err();
    assert!(matches!(
        error,
        ComputeError::OutsideComputation { operation: "eval" }
    ));
}
