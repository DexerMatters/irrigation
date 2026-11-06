//! Procedural macros for the reactive incremental language.
//!
//! The macros lower a Rust-shaped subset into typed builder calls:
//!
//! * `#[view]` declares a View namespace and its generated handles;
//! * `#[component]` turns a function into a registered generator whose body is
//!   lowered to tracked primitives;
//! * `#[reactive]` groups components;
//! * `#[tree_view]` declares a mutually recursive ownership-tree View.

#![warn(missing_docs)]

mod attr;
mod component;
mod lower;
mod names;
mod tree;
mod view;

use proc_macro::TokenStream;

/// Declares a View.
///
/// `#[view(source)]` declares a View populated by edit transactions;
/// `#[view(derived)]` declares a View populated by component definitions.
///
/// A View's element types must be `Debug`: the board draws every View, and
/// spells its values with their `Debug` form when the host registers no
/// renderer. A View whose values cannot be spelled opts out with
/// `#[view(derived, hide)]` — its plate then shows rows and change marks only.
#[proc_macro_attribute]
pub fn view(args: TokenStream, input: TokenStream) -> TokenStream {
    view::expand(args.into(), input.into()).into()
}

/// Turns a function into a reactive component generator.
#[proc_macro_attribute]
pub fn component(args: TokenStream, input: TokenStream) -> TokenStream {
    component::expand_component(args.into(), input.into()).into()
}

/// Declares a module of components.
#[proc_macro_attribute]
pub fn reactive(_args: TokenStream, input: TokenStream) -> TokenStream {
    component::expand_module(input.into()).into()
}

/// Declares a mutually recursive ownership-tree View group.
#[proc_macro_attribute]
pub fn tree_view(args: TokenStream, input: TokenStream) -> TokenStream {
    tree::expand(args.into(), input.into()).into()
}
