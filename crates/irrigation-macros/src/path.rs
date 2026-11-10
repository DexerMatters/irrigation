//! Resolves the user-facing crate path.
//!
//! Generated code names `irrigation::...` in hundreds of places. A dependent
//! crate may rename the package (`irr = { package = "irrigation" }`), in which
//! case the literal name does not resolve. Each macro expansion is rewritten
//! once, after generation, so every generated path uses the dependency key the
//! user actually wrote.

use proc_macro_crate::{FoundCrate, crate_name};
use proc_macro2::{Group, Ident, Span, TokenStream, TokenTree};
use quote::quote;

/// The path to the facade crate in this expansion.
#[must_use]
pub fn irrigation_path() -> TokenStream {
    match crate_name("irrigation") {
        // Integration tests belong to the facade package but are separate crates;
        // the facade also has `extern crate self as irrigation`, so this spelling
        // is valid in both places.
        Ok(FoundCrate::Itself) => quote!(irrigation),
        Ok(FoundCrate::Name(name)) => {
            let ident = Ident::new(&name, Span::call_site());
            quote!(#ident)
        }
        Err(_) => quote!(irrigation),
    }
}

/// Rewrites literal `irrigation::` paths in generated tokens.
#[must_use]
pub fn rewrite(stream: TokenStream) -> TokenStream {
    let mut path: Vec<TokenTree> = irrigation_path().into_iter().collect();
    path.push(TokenTree::Punct(proc_macro2::Punct::new(
        ':',
        proc_macro2::Spacing::Joint,
    )));
    path.push(TokenTree::Punct(proc_macro2::Punct::new(
        ':',
        proc_macro2::Spacing::Alone,
    )));
    rewrite_tokens(stream, &path)
}

fn rewrite_tokens(stream: TokenStream, path: &[TokenTree]) -> TokenStream {
    let tokens: Vec<TokenTree> = stream.into_iter().collect();
    let mut out = Vec::with_capacity(tokens.len());
    let mut index = 0;
    while index < tokens.len() {
        if is_path_start(&tokens, index) {
            out.extend(path.iter().cloned());
            index += 3;
            continue;
        }
        match &tokens[index] {
            TokenTree::Group(group) => {
                let mut rewritten =
                    Group::new(group.delimiter(), rewrite_tokens(group.stream(), path));
                rewritten.set_span(group.span());
                out.push(TokenTree::Group(rewritten));
            }
            other => out.push(other.clone()),
        }
        index += 1;
    }
    out.into_iter().collect()
}

fn is_path_start(tokens: &[TokenTree], index: usize) -> bool {
    if !matches!(&tokens[index], TokenTree::Ident(ident) if ident == "irrigation") {
        return false;
    }
    let Some(TokenTree::Punct(first)) = tokens.get(index + 1) else {
        return false;
    };
    let Some(TokenTree::Punct(second)) = tokens.get(index + 2) else {
        return false;
    };
    first.as_char() == ':'
        && second.as_char() == ':'
        && first.spacing() == proc_macro2::Spacing::Joint
}

#[cfg(test)]
mod tests {
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
}
