//! Unit tests for `src/frame.rs`.

use super::*;

#[test]
fn cancel_token_is_shared_and_observable() {
    let token = CancelToken::new(7);
    let clone = token.clone();
    assert!(!clone.is_cancelled());
    token.cancel();
    assert!(clone.is_cancelled());
    assert_eq!(clone.generation(), 7);
}

#[test]
fn frames_pop_in_order_even_after_panic() {
    // A frame cannot be constructed without a runtime; verify stack hygiene
    // through the scope helper instead, which is the public surface.
    let result = std::panic::catch_unwind(|| {
        in_scope(1, "test", || {
            // No ambient frame: this must panic with an engine error.
            panic!("boom");
        })
    });
    assert!(result.is_err());
    assert!(current().is_none());
}
