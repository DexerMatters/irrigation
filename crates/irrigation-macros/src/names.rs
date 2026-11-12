//! Stable identifier derivation and name helpers for generated code.

use proc_macro2::{Ident, Span};

/// Converts `snake_case` to `PascalCase`.
#[must_use]
pub fn pascal_case(name: &str) -> Ident {
    let mut result = String::new();
    for part in name.split('_') {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            result.extend(first.to_uppercase());
            result.push_str(chars.as_str());
        }
    }
    Ident::new(&result, Span::call_site())
}

#[cfg(test)]
#[path = "../tests/unit/names/tests.rs"]
mod tests;
