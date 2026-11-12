//! Unit tests for `src/error.rs`.

use super::*;

#[test]
fn cancellation_is_retryable_and_duplicates_are_permanent() {
    assert!(ComputeError::Cancelled.is_cancelled());
    assert!(!ComputeError::Cancelled.is_permanent());
    assert!(
        ComputeError::DuplicateDefinition {
            unit: "Types[ExprRef(1)]".into(),
            first: "infer".into(),
            second: "check".into(),
        }
        .is_permanent()
    );
}

#[test]
fn display_includes_both_definers() {
    let error = ComputeError::DuplicateDefinition {
        unit: "Types[x]".into(),
        first: "a.rs:10".into(),
        second: "b.rs:20".into(),
    };
    let rendered = error.to_string();
    assert!(rendered.contains("a.rs:10"));
    assert!(rendered.contains("b.rs:20"));
}
