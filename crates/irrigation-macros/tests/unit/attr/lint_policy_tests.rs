//! Unit tests for `src/attr.rs`.

use super::*;
use syn::parse_quote;

/// A policy attribute applies to everything the macro emits.
#[test]
fn allow_is_forwardable() {
    let attr: Attribute = parse_quote!(#[allow(dead_code)]);
    assert!(is_lint_policy(&attr));
    assert!(is_forwardable_lint_policy(&attr));
}

/// An expectation asserts that the lint fires *here*; copying it onto generated
/// items makes it unfulfilled there, which is a lint the user did not write.
#[test]
fn expect_is_not_forwardable() {
    let attr: Attribute = parse_quote!(#[expect(unused_variables)]);
    assert!(is_lint_policy(&attr), "it is still a lint attribute");
    assert!(
        !is_forwardable_lint_policy(&attr),
        "but it must not be copied onto generated items"
    );
}

#[test]
fn other_policies_are_forwardable() {
    let deny: Attribute = parse_quote!(#[deny(unsafe_code)]);
    let warn: Attribute = parse_quote!(#[warn(missing_docs)]);
    let forbid: Attribute = parse_quote!(#[forbid(unsafe_code)]);
    for attr in [deny, warn, forbid] {
        assert!(is_forwardable_lint_policy(&attr));
    }
}
