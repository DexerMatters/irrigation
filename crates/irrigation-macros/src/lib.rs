//! Procedural macros for the reactive incremental language.
//!
//! The macros lower a Rust-shaped subset into typed builder calls:
//!
//! * `#[view]` declares a View namespace and its generated handles;
//! * `#[component]` turns a function into a registered generator whose body is
//!   lowered to tracked primitives;
//! * `#[irrigation]` groups components;
//! * `#[tree_view]` declares a mutually recursive ownership-tree View.

#![warn(missing_docs)]

mod attr;
mod component;
mod general;
mod lower;
mod names;
mod path;
mod schema;
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
    path::rewrite(view::expand(args.into(), input.into())).into()
}

/// Declares a schema View over stock collection fields.
///
/// `#[incremental_data]` is a View whose fields are collections (Map, Set,
/// List, Graph, Tree, Scalar). Field-level `#[collection(...)]`, `#[index]`,
/// and `#[projection]` annotations are schema documentation and are stripped
/// before expansion; indexes and projections are ordinary components over the
/// generated handles.
#[proc_macro_attribute]
pub fn incremental_data(args: TokenStream, input: TokenStream) -> TokenStream {
    path::rewrite(schema::expand(args.into(), input.into())).into()
}

/// Turns a function into a reactive component generator.
#[proc_macro_attribute]
pub fn component(args: TokenStream, input: TokenStream) -> TokenStream {
    path::rewrite(component::expand_component(args.into(), input.into())).into()
}

/// Declares a module of components.
#[proc_macro_attribute]
pub fn irrigation(_args: TokenStream, input: TokenStream) -> TokenStream {
    path::rewrite(component::expand_module(input.into())).into()
}

/// Turns an impl of a general component into the full trait implementation.
///
/// An inherent `impl` with `fn run(cx)` is the stateless producer form; a
/// `GeneralComponent` impl is the retained-state form. `reads(..)`, `writes(..)`,
/// `incremental`, and `id(..)` declare what the macro cannot infer.
#[proc_macro_attribute]
pub fn general_component(args: TokenStream, input: TokenStream) -> TokenStream {
    path::rewrite(general::expand(args.into(), input.into())).into()
}

/// Declares a mutually recursive ownership-tree View group.
#[proc_macro_attribute]
pub fn tree_view(args: TokenStream, input: TokenStream) -> TokenStream {
    path::rewrite(tree::expand(args.into(), input.into())).into()
}
