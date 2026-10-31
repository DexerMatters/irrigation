//! Attribute parsing helpers.

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

/// Removes `#[component]` from an attribute list, reporting whether it was
/// present.
pub fn take_component(attrs: &mut Vec<Attribute>) -> bool {
    let mut found = false;
    attrs.retain(|attr| {
        if attr.path().is_ident("component") {
            found = true;
            false
        } else {
            true
        }
    });
    found
}
