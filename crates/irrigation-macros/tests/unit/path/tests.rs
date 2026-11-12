//! Unit tests for `src/path.rs`.

use super::*;

#[test]
fn rewrites_a_path_and_keeps_other_tokens() {
    let tokens = quote!(let x: irrigation::Value = irrigation::value_of(1u32););
    let mut path = quote!(irr).into_iter().collect::<Vec<_>>();
    path.push(TokenTree::Punct(proc_macro2::Punct::new(
        ':',
        proc_macro2::Spacing::Joint,
    )));
    path.push(TokenTree::Punct(proc_macro2::Punct::new(
        ':',
        proc_macro2::Spacing::Alone,
    )));
    let text = rewrite_tokens(tokens, &path).to_string();
    assert!(!text.contains("irrigation ::"), "{text}");
    assert!(text.contains("irr ::"), "{text}");
}

#[test]
fn leaves_a_lone_identifier() {
    let tokens = quote!(
        fn irrigation() {}
    );
    let text = rewrite(tokens).to_string();
    assert!(text.contains("irrigation"), "{text}");
}
