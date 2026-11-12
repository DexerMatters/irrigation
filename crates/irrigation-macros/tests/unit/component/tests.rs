//! Unit tests for `src/component.rs`.

use super::*;
use quote::quote;

#[test]
fn strict_components_reject_undeclared_calls() {
    let tokens = expand_component(
        quote!(strict),
        quote!(
            fn c(x: u64) -> u64 {
                helper(x)
            }
        ),
    );
    assert!(
        tokens.to_string().contains("opaque call"),
        "strict mode must reject an undeclared call"
    );
}

#[test]
fn strict_components_allow_declared_primitives() {
    let tokens = expand_component(
        quote!(strict, allows = [helper]),
        quote!(
            fn c(x: u64) -> u64 {
                helper(x)
            }
        ),
    );
    assert!(!tokens.to_string().contains("opaque call"));
}

#[test]
fn mutable_locals_are_rejected() {
    let tokens = expand_component(
        quote!(),
        quote!(
            fn c(x: u64) -> u64 {
                let mut out = Vec::new();
                out.push(x);
                out.len() as u64
            }
        ),
    );
    assert!(
        tokens.to_string().contains("let mut"),
        "mutable locals must be rejected instead of silently losing writes"
    );
}

#[test]
fn attributes_follow_every_generated_item() {
    let tokens = expand_component(
        quote!(),
        quote!(
            /// Docs for the component.
            #[cfg(feature = "demo")]
            fn c(x: u64) -> u64 {
                x
            }
        ),
    )
    .to_string();
    assert!(
        tokens.contains("Docs for the component."),
        "the docs must reach the public entry point: {tokens}"
    );
    // The private implementation, the argument struct, the entry point,
    // the spec struct, and the spec impl.
    assert_eq!(
        tokens.matches("cfg (feature = \"demo\")").count(),
        5,
        "every generated item must carry the gate: {tokens}"
    );
}

#[test]
fn root_components_gate_their_marker_too() {
    let tokens = expand_component(
        quote!(),
        quote!(
            #[cfg(feature = "demo")]
            fn c(view: &Widgets) -> u64 {
                view.count()
            }
        ),
    )
    .to_string();
    assert_eq!(
        tokens.matches("cfg (feature = \"demo\")").count(),
        5,
        "the entry point, implementation, root marker, impl, and output constants all need the gate: {tokens}"
    );
}

#[test]
fn function_only_attributes_stay_on_functions() {
    let tokens = expand_component(
        quote!(),
        quote!(
            #[inline]
            fn c(x: u64) -> u64 {
                x
            }
        ),
    )
    .to_string();
    assert_eq!(
        tokens.matches("inline").count(),
        2,
        "`#[inline]` applies to the two generated functions only: {tokens}"
    );
}

#[test]
fn components_are_permissive_by_default() {
    let tokens = expand_component(
        quote!(),
        quote!(
            fn c(x: u64) -> u64 {
                helper(x)
            }
        ),
    );
    assert!(!tokens.to_string().contains("opaque call"));
}
