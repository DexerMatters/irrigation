//! `#[general_component]` expansion.
//!
//! Two forms share one attribute:
//!
//! * an inherent `impl` with `fn run(cx)` for a stateless producer; the macro
//!   adds the `GeneralComponent` impl;
//! * a `GeneralComponent` trait impl for a component with retained state; the
//!   macro fills the identity and View constants and checks the update policy.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::parse::Parser;
use syn::{
    Expr, GenericArgument, ImplItem, ItemImpl, PathArguments, ReturnType, Type, parse_quote,
};

#[derive(Default)]
struct GeneralConfig {
    reads: Vec<Type>,
    writes: Vec<Type>,
    incremental: bool,
    id: Option<Expr>,
}

fn parse_config(args: TokenStream) -> syn::Result<GeneralConfig> {
    let mut config = GeneralConfig::default();
    if args.is_empty() {
        return Ok(config);
    }
    let parser = syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated;
    for meta in parser.parse2(args)? {
        match meta {
            syn::Meta::Path(path) if path.is_ident("incremental") => config.incremental = true,
            syn::Meta::List(list)
                if list.path.is_ident("reads") || list.path.is_ident("writes") =>
            {
                let parser = syn::punctuated::Punctuated::<Type, syn::Token![,]>::parse_terminated;
                let types = parser.parse2(list.tokens.clone())?;
                if list.path.is_ident("reads") {
                    config.reads.extend(types);
                } else {
                    config.writes.extend(types);
                }
            }
            syn::Meta::List(list) if list.path.is_ident("id") => {
                let parser = syn::punctuated::Punctuated::<Expr, syn::Token![,]>::parse_terminated;
                let values = parser.parse2(list.tokens.clone())?;
                if values.len() != 1 {
                    return Err(syn::Error::new_spanned(
                        list,
                        "expected `id(0x...)` with one value",
                    ));
                }
                config.id = values.into_iter().next();
            }
            other => {
                return Err(syn::Error::new_spanned(
                    other,
                    "expected `reads(..)`, `writes(..)`, `incremental`, or `id(..)`",
                ));
            }
        }
    }
    Ok(config)
}

/// Expands one `#[general_component]` impl.
pub fn expand(args: TokenStream, input: TokenStream) -> TokenStream {
    match expand_inner(args, input) {
        Ok(tokens) => tokens,
        Err(error) => error.to_compile_error(),
    }
}

fn expand_inner(args: TokenStream, input: TokenStream) -> syn::Result<TokenStream> {
    let config = parse_config(args)?;
    let item: ItemImpl = syn::parse2(input)?;
    if item.trait_.is_some() {
        expand_trait_impl(item, &config)
    } else {
        expand_inherent_impl(item, &config)
    }
}

fn view_constants(views: &[Type], name: &str) -> TokenStream {
    let name = format_ident!("{name}");
    if views.is_empty() {
        return quote!(#name: &'static [&'static irrigation::ViewDescriptor] = &[]);
    }
    quote! {
        #name: &'static [&'static irrigation::ViewDescriptor] = &[
            #( <#views as irrigation::ViewSchema>::DESCRIPTOR, )*
        ]
    }
}

fn identity(self_ty: &Type, id: Option<Expr>) -> (TokenStream, TokenStream) {
    let name = quote!(concat!(module_path!(), "::", stringify!(#self_ty)));
    let id = id.map_or_else(
        || quote!(irrigation::ComponentId(irrigation::const_hash(#name))),
        |id| quote!(irrigation::ComponentId(#id)),
    );
    (id, name)
}

fn expand_inherent_impl(item: ItemImpl, config: &GeneralConfig) -> syn::Result<TokenStream> {
    if config.incremental {
        return Err(syn::Error::new_spanned(
            &item,
            "the function form is stateless and cannot be `incremental`; use a `GeneralComponent` impl",
        ));
    }
    let self_ty = (*item.self_ty).clone();
    let Some(run) = item.items.iter().find_map(|item| match item {
        ImplItem::Fn(function) if function.sig.ident == "run" => Some(function),
        _ => None,
    }) else {
        return Err(syn::Error::new_spanned(
            &item,
            "the function form requires `fn run(cx: &mut GeneralCx) -> Result<...>`",
        ));
    };
    if run.sig.inputs.len() != 1 {
        return Err(syn::Error::new_spanned(
            &run.sig,
            "`run` takes exactly one `&mut GeneralCx` parameter",
        ));
    }
    let output = result_output(&run.sig.output)?;
    let (id, name) = identity(&self_ty, config.id.clone());
    let views = view_constants(&config.reads, "VIEWS");
    let outputs = view_constants(&config.writes, "OUTPUT_VIEWS");
    let impl_block = quote! {
        impl irrigation::GeneralComponent for #self_ty {
            type State = ();
            type Output = #output;

            const ID: irrigation::ComponentId = #id;
            const NAME: &'static str = #name;
            const #views;
            const #outputs;

            fn initialize(
                &self,
                cx: &mut irrigation::GeneralCx,
            ) -> irrigation::Result<((), #output)> {
                Self::run(cx).map(|output| ((), output))
            }
        }
    };
    Ok(quote! {
        #item
        #impl_block
    })
}

fn result_output(output: &ReturnType) -> syn::Result<Type> {
    let ReturnType::Type(_, ty) = output else {
        return Err(syn::Error::new_spanned(
            output,
            "`run` must return `Result<()>` or `Result<T>`",
        ));
    };
    let Type::Path(path) = &**ty else {
        return Err(syn::Error::new_spanned(ty, "expected `Result<...>`"));
    };
    let Some(segment) = path.path.segments.last() else {
        return Err(syn::Error::new_spanned(ty, "expected `Result<...>`"));
    };
    if segment.ident != "Result" {
        return Err(syn::Error::new_spanned(ty, "expected `Result<...>`"));
    }
    let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return Err(syn::Error::new_spanned(ty, "expected `Result<...>`"));
    };
    arguments
        .args
        .iter()
        .find_map(|argument| match argument {
            GenericArgument::Type(ty) => Some(ty.clone()),
            _ => None,
        })
        .ok_or_else(|| syn::Error::new_spanned(ty, "expected `Result<T>`"))
}

fn expand_trait_impl(mut item: ItemImpl, config: &GeneralConfig) -> syn::Result<TokenStream> {
    let self_ty = (*item.self_ty).clone();
    let trait_name = item
        .trait_
        .as_ref()
        .and_then(|(path, _)| path.segments.last())
        .map(|segment| segment.ident.to_string())
        .unwrap_or_default();
    if trait_name != "GeneralComponent" {
        return Err(syn::Error::new_spanned(
            &item,
            "`#[general_component]` applies to an inherent `impl` or a `GeneralComponent` impl",
        ));
    }
    let has_const = |ident: &str| {
        item.items
            .iter()
            .any(|item| matches!(item, ImplItem::Const(value) if value.ident == ident))
    };
    let has_fn = |ident: &str| {
        item.items
            .iter()
            .any(|item| matches!(item, ImplItem::Fn(function) if function.sig.ident == ident))
    };
    let has_id = has_const("ID");
    let has_name = has_const("NAME");
    let has_views = has_const("VIEWS");
    let has_outputs = has_const("OUTPUT_VIEWS");
    let has_update = has_fn("update");
    let has_policy = has_fn("dependency_updates");
    let (id, name) = identity(&self_ty, config.id.clone());
    if !has_id {
        item.items.push(parse_quote! {
            const ID: irrigation::ComponentId = #id;
        });
    }
    if !has_name {
        item.items.push(parse_quote! {
            const NAME: &'static str = #name;
        });
    }
    if !config.reads.is_empty() && !has_views {
        let views = view_constants(&config.reads, "VIEWS");
        item.items.push(parse_quote!(const #views;));
    }
    if !config.writes.is_empty() && !has_outputs {
        let outputs = view_constants(&config.writes, "OUTPUT_VIEWS");
        item.items.push(parse_quote!(const #outputs;));
    }
    if config.incremental && !has_update {
        return Err(syn::Error::new_spanned(
            &item,
            "an `incremental` component must implement `update`",
        ));
    }
    if config.incremental && !has_policy {
        item.items.push(parse_quote! {
            fn dependency_updates(&self) -> irrigation::DependencyUpdates {
                irrigation::DependencyUpdates::Incremental
            }
        });
    }
    Ok(quote!(#item))
}
