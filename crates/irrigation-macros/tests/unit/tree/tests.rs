//! Unit tests for `src/tree.rs`.

use super::*;
use quote::quote;

#[test]
fn two_variants_share_an_accessor_when_they_agree() {
    let tokens = expand(
        quote!(root = Item, view = Tree),
        quote! {
            mod tree {
                #[derive(Clone, Debug, PartialEq)]
                pub enum Item {
                    Leaf { value: u64 },
                    Pair { value: u64, other: u64 },
                }
            }
        },
    )
    .to_string();
    assert!(!tokens.contains("compile_error"), "{tokens}");
    assert_eq!(
        tokens.matches("pub fn value").count(),
        1,
        "one accessor serves both variants: {tokens}"
    );
    assert!(
        tokens.contains("Some (ItemKind :: Leaf)") && tokens.contains("Some (ItemKind :: Pair)"),
        "the accessor dispatches on the discriminant: {tokens}"
    );
}

#[test]
fn two_variants_cannot_share_a_name_with_different_types() {
    let tokens = expand(
        quote!(root = Item, view = Tree),
        quote! {
            mod tree {
                #[derive(Clone, Debug, PartialEq)]
                pub enum Item {
                    Leaf { value: u64 },
                    Named { value: String },
                }
            }
        },
    )
    .to_string();
    assert!(
        tokens.contains("compile_error"),
        "a shared name with two types must be rejected at the source: {tokens}"
    );
    assert!(
        tokens.contains("`Leaf`") && tokens.contains("`Named`"),
        "the diagnostic must name both variants: {tokens}"
    );
}
