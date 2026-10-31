//! `#[tree_view]` expansion.
//!
//! The tree group macro must see every mutually recursive type at once, which
//! is why it is a module-level attribute rather than a derive. The expansion is
//! implemented in `tree_expand.rs`; this module keeps the entry point small.

use proc_macro2::TokenStream;

/// Expands a tree View group.
pub fn expand(_args: TokenStream, input: TokenStream) -> TokenStream {
    syn::Error::new_spanned(
        input,
        "`#[tree_view]` is not implemented yet; use source Views with `List`/`Map` fields",
    )
    .to_compile_error()
}
