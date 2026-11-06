//! Attribute parsing helpers.

use proc_macro2::TokenStream;
use quote::ToTokens;
use syn::parse::Parser;
use syn::spanned::Spanned;
use syn::{Attribute, Ident, Result};

/// Which kind of View an attribute declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewKindArg {
    /// Populated by edit transactions.
    Source,
    /// Populated by component definitions.
    Derived,
}

/// What `#[view(...)]` declared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewArgs {
    /// Source or derived.
    pub kind: ViewKindArg,
    /// The View declined to spell its values, so a generic drawing shows
    /// identity only. The escape hatch for an element type that cannot be
    /// `Debug`.
    pub hide: bool,
}

/// Parses `#[view(source)]`, `#[view(derived)]`, plus the optional `hide`.
pub fn parse_view_args(attr: &Attribute) -> Result<ViewArgs> {
    let parser = syn::punctuated::Punctuated::<Ident, syn::Token![,]>::parse_terminated;
    let mut kind = None;
    let mut hide = false;
    for ident in parser.parse2(attr.meta.require_list()?.tokens.clone())? {
        match ident.to_string().as_str() {
            "source" => kind = Some(ViewKindArg::Source),
            "derived" => kind = Some(ViewKindArg::Derived),
            "hide" => hide = true,
            other => {
                return Err(syn::Error::new(
                    ident.span(),
                    format!("expected `source`, `derived` or `hide`, found `{other}`"),
                ));
            }
        }
    }
    match (kind, hide) {
        (Some(kind), hide) => Ok(ViewArgs { kind, hide }),
        (None, _) => Err(syn::Error::new(
            attr.span(),
            "expected `source` or `derived`",
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
