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

/// How a component is installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComponentKind {
    /// Installed once as a runtime root.
    Root,
    /// Interned and called by another component.
    Callable,
}

/// Policy parsed from `#[component(...)]`.
#[derive(Clone, Debug, Default)]
pub struct ComponentConfig {
    /// Reject plain function calls that are not declared primitives.
    pub strict: bool,
    /// Declared primitive paths allowed in strict mode.
    pub allowed: Vec<String>,
    /// Explicit root/callable kind; `None` keeps the legacy signature rule.
    pub kind: Option<ComponentKind>,
    /// Explicit key type for a keyed callable component.
    pub keyed: Option<syn::Type>,
    /// Static write scopes planned at the start of a callable component.
    pub plans: Vec<syn::Expr>,
    /// View whose first field supplies keys to a keyed component.
    pub keys: Option<syn::Type>,
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
            syn::Meta::Path(path) if path.is_ident("root") || path.is_ident("callable") => {
                if config.kind.is_some() {
                    return Err(syn::Error::new_spanned(
                        path,
                        "only one of `root` and `callable` is allowed",
                    ));
                }
                config.kind = Some(if path.is_ident("root") {
                    ComponentKind::Root
                } else {
                    ComponentKind::Callable
                });
            }
            syn::Meta::NameValue(value) if value.path.is_ident("keyed") => {
                config.keyed = Some(syn::parse2(value.value.to_token_stream())?);
            }
            syn::Meta::NameValue(value) if value.path.is_ident("keys") => {
                config.keys = Some(syn::parse2(value.value.to_token_stream())?);
            }
            syn::Meta::NameValue(value) if value.path.is_ident("plans") => {
                let syn::Expr::Array(array) = value.value else {
                    return Err(syn::Error::new_spanned(
                        value.value,
                        "expected `plans = [scope, ...]`",
                    ));
                };
                config.plans.extend(array.elems);
            }
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
                    "expected `root`, `callable`, `keyed = Type`, `keys = View`/`(View, accessor)`, `plans = [scope, ...]`, `strict`, or `allows = [path, ...]`",
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

/// Whether an attribute is a lint-level policy, which is valid on every item
/// the macros generate and must therefore follow all of them.
pub fn is_lint_policy(attr: &Attribute) -> bool {
    let path = attr.path();
    ["allow", "expect", "warn", "deny", "forbid"]
        .iter()
        .any(|name| path.is_ident(name))
}
