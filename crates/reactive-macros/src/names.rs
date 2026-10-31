//! Stable identifier derivation and name helpers for generated code.

use proc_macro2::{Ident, Span};
use quote::format_ident;

/// FNV-1a over a string, used to derive stable compile-time identities.
#[must_use]
pub fn fnv1a(value: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

/// Derives a View identity from its fully qualified name.
#[must_use]
pub fn view_id(name: &str) -> u32 {
    fnv1a(name) as u32
}

/// Derives a component identity from its fully qualified name.
#[must_use]
pub fn component_id(name: &str) -> u32 {
    fnv1a(name) as u32
}

/// Derives one expression-site identity.
#[must_use]
pub fn site_id(component: u64, counter: u32) -> u32 {
    (fnv1a(&format!("{component}:{counter}")) as u32) | 1
}

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

/// Builds a generated identifier.
#[must_use]
pub fn ident(prefix: &str, name: &str) -> Ident {
    format_ident!("__{prefix}_{name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_are_stable_and_distinct() {
        assert_eq!(view_id("a::Options"), view_id("a::Options"));
        assert_ne!(view_id("a::Options"), view_id("a::Types"));
        assert_ne!(site_id(7, 0), site_id(7, 1));
    }

    #[test]
    fn pascal_case_handles_snake_case() {
        assert_eq!(pascal_case("analyze_types").to_string(), "AnalyzeTypes");
    }
}
