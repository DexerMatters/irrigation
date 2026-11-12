//! Unit tests for `src/lib.rs`.

use super::*;

/// The callable path runs under a renamed facade too.
///
/// Compiling proves the macros emit `irr::`, not that the emitted paths work; this drives the
/// callable's generated spec and args, which is the macro family whose expansion differs most
/// from a component's.
#[test]
fn a_callable_runs_under_a_renamed_dependency() {
    let runtime = Runtime::new();
    runtime
        .edit(|edit| edit.view::<Input>().values().push(5))
        .expect("edit");
    assert_eq!(
        runtime
            .call::<First>(FirstArgs { input: Input })
            .expect("callable"),
        Some(5)
    );
}
