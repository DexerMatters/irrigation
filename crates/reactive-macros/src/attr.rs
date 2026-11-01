//! Attribute parsing helpers.

use proc_macro2::TokenStream;
use quote::ToTokens;
use syn::parse::Parser;
use syn::{Attribute, Ident, Result};

/// Which kind of View an attribute declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewKindArg {
    /// Populated by edit transactions.
    Source,
    /// Populated by component definitions.
    Derived,
}

/// Parses `#[view(source)]` or `#[view(derived)]`.
pub fn parse_view_kind(attr: &Attribute) -> Result<ViewKindArg> {
    let ident: Ident = attr.parse_args()?;
    match ident.to_string().as_str() {
        "source" => Ok(ViewKindArg::Source),
        "derived" => Ok(ViewKindArg::Derived),
        other => Err(syn::Error::new(
            ident.span(),
            format!("expected `source` or `derived`, found `{other}`"),
        )),
    }
}

/// Policy parsed from `#[component(...)]`.
#[derive(Clone, Debug, Default)]
pub struct ComponentConfig {
    /// Reject plain function calls that are not declared primitives.
    pub strict: bool,
    /// Declared primitive paths allowed in strict mode.
    pub allowed: Vec<String>,
}

/// Parses `#[component(strict, allows = [path, ...])]`.
pub fn parse_component_config(args: TokenStream) -> Result<ComponentConfig> {
    let mut config = ComponentConfig::default();
    if args.is_empty() {
        return Ok(config);
    }
    let parser = syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated;
    for meta in parser.parse2(args)? {
        match meta {
            syn::Meta::Path(path) if path.is_ident("strict") => config.strict = true,
            syn::Meta::NameValue(value) if value.path.is_ident("allows") => {
                let syn::Expr::Array(array) = value.value else {
                    return Err(syn::Error::new_spanned(
                        value.value,
                        "expected `allows = [path, ...]`",
                    ));
                };
                for element in array.elems {
                    let syn::Expr::Path(path) = element else {
                        return Err(syn::Error::new_spanned(
                            element,
                            "`allows` entries must be function paths",
                        ));
                    };
                    config.allowed.push(path.path.to_token_stream().to_string());
                }
            }
            other => {
                return Err(syn::Error::new_spanned(
                    other,
                    "expected `strict` and/or `allows = [path, ...]`",
                ));
            }
        }
    }
    Ok(config)
}

/// Removes `#[component(...)]` from an attribute list, returning its arguments.
pub fn take_component(attrs: &mut Vec<Attribute>) -> Option<TokenStream> {
    let mut args = None;
    attrs.retain(|attr| {
        if attr.path().is_ident("component") {
            args = Some(match &attr.meta {
                syn::Meta::Path(_) => TokenStream::new(),
                syn::Meta::List(list) => list.tokens.clone(),
                syn::Meta::NameValue(_) => TokenStream::new(),
            });
            false
        } else {
            true
        }
    });
    args
}
