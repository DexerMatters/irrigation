//! Unit tests for `src/http.rs`.

use super::*;

/// The slot comes back even when the connection thread panics.
///
/// A plain decrement after the handler would leak one slot per panic, and `MAX_CONNECTIONS` leaks
/// would leave the server refusing everything with no way to recover short of a restart.
#[test]
fn a_panicking_connection_releases_its_slot() {
    let live = Arc::new(std::sync::atomic::AtomicUsize::new(1));
    let counter = Arc::clone(&live);
    let joined = std::thread::spawn(move || {
        let _slot = LiveGuard(counter);
        panic!("a handler panicked");
    })
    .join();
    assert!(joined.is_err(), "the thread panicked as intended");
    assert_eq!(
        live.load(Ordering::Acquire),
        0,
        "the slot is released by the guard, not by the code after the handler"
    );
}
