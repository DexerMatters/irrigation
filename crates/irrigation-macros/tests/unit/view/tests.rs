//! Unit tests for `src/view.rs`.

use super::*;
use quote::quote;

#[test]
fn attributes_gate_every_generated_item() {
    let tokens = expand(
        quote!(source),
        quote!(
            /// Docs for the view.
            #[cfg(feature = "demo")]
            struct Numbers {
                values: u64,
            }
        ),
    )
    .to_string();
    assert!(
        tokens.contains("Docs for the view."),
        "the docs must reach the handle: {tokens}"
    );
    // The intent is "every generated item carries the gate", stated as such:
    // a literal count breaks whenever an item is added or removed for an
    // unrelated reason, which says nothing about gating.
    let gate = "cfg (feature = \"demo\")";
    let mut checked = 0;
    for (index, _) in tokens.match_indices("fn __") {
        let window = &tokens[index.saturating_sub(160)..index];
        assert!(
            window.contains(gate),
            "a generated item is not gated: {window}"
        );
        checked += 1;
    }
    assert!(
        checked >= 10,
        "the expansion generated almost nothing: {tokens}"
    );
}

#[test]
fn a_cfg_gated_field_is_rejected_at_the_field() {
    let tokens = expand(
        quote!(source),
        quote!(
            struct Numbers {
                /// Shown.
                shown: u64,
                #[cfg(any())]
                hidden: MissingType,
            }
        ),
    )
    .to_string();
    assert!(
        tokens.contains("compile_error"),
        "a gated field must not reach the generated content: {tokens}"
    );
    assert!(
        tokens.contains("Gate the whole View item"),
        "the diagnostic must name the supported alternative: {tokens}"
    );
}

#[test]
fn derives_are_not_forwarded_to_the_handle() {
    let tokens = expand(
        quote!(source),
        quote!(
            #[derive(Default)]
            struct Numbers {
                values: u64,
            }
        ),
    )
    .to_string();
    assert!(
        !tokens.contains("derive (Default)"),
        "an input derive must not be replayed on the handle: {tokens}"
    );
}
