//! `#[incremental_data]`: a schema-declared View.
//!
//! The declaration is a View whose fields are stock collection shapes. Field
//! attributes such as `#[collection(Map)]` are schema annotations and are
//! stripped before the ordinary View expansion; indexes and projections are
//! ordinary components over the resulting handles.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Attribute, ItemStruct};

pub fn expand(args: TokenStream, input: TokenStream) -> TokenStream {
    let mut item: ItemStruct = match syn::parse2(input) {
        Ok(item) => item,
        Err(error) => return error.to_compile_error(),
    };
    for field in &mut item.fields {
        strip_schema_attrs(&mut field.attrs);
    }
    let name = item.ident.clone();
    let view_args = if args.is_empty() {
        quote!(derived)
    } else {
        args
    };
    let expanded = crate::view::expand(view_args, quote!(#item));
    quote! {
        #expanded
        impl irrigation::IncrementalData for #name {}
    }
}

fn strip_schema_attrs(attrs: &mut Vec<Attribute>) {
    attrs.retain(|attr| {
        let path = attr.path();
        !(path.is_ident("collection") || path.is_ident("index") || path.is_ident("projection"))
    });
}
