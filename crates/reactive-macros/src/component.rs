//! `#[component]` and `#[reactive]` expansion.
//!
//! A component function is split into:
//!
//! * `__name_impl` — the original body with every trackable expression lowered
//!   to a primitive node;
//! * `name` — the public entry point that interns the invocation;
//! * an argument struct and a `ComponentSpec` implementation;
//! * for components whose parameters are all View references, a root marker
//!   that `Runtime::install` accepts.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{FnArg, Item, ItemFn, ItemMod, Pat, ReturnType, Type, Visibility};

use crate::attr::take_component;
use crate::lower::Lowerer;
use crate::names::pascal_case;

/// Expands one `#[component]` function.
pub fn expand_component(input: TokenStream) -> TokenStream {
    match syn::parse2::<ItemFn>(input) {
        Ok(item) => match expand_fn(item) {
            Ok(tokens) => tokens,
            Err(error) => error.to_compile_error(),
        },
        Err(error) => error.to_compile_error(),
    }
}

/// Expands a module, processing every `#[component]` item inside it.
pub fn expand_module(input: TokenStream) -> TokenStream {
    let module: ItemMod = match syn::parse2(input) {
        Ok(module) => module,
        Err(error) => return error.to_compile_error(),
    };
    let Some((_, items)) = module.content.clone() else {
        return syn::Error::new_spanned(&module, "`#[reactive]` requires an inline module")
            .to_compile_error();
    };
    let mut expanded = Vec::new();
    for item in items {
        match item {
            Item::Fn(mut function) => {
                if take_component(&mut function.attrs) {
                    match expand_fn(function) {
                        Ok(tokens) => expanded.push(tokens),
                        Err(error) => expanded.push(error.to_compile_error()),
                    }
                } else {
                    expanded.push(quote!(#function));
                }
            }
            other => expanded.push(quote!(#other)),
        }
    }
    let attrs = &module.attrs;
    let vis = &module.vis;
    let ident = &module.ident;
    quote! {
        #(#attrs)*
        #vis mod #ident {
            #(#expanded)*
        }
    }
}

fn expand_fn(item: ItemFn) -> syn::Result<TokenStream> {
    let name = item.sig.ident.clone();
    let name_str = name.to_string();
    let vis = item.vis.clone();
    let pascal = pascal_case(&name_str);
    let impl_name = format_ident!("__{}_impl", name_str);
    let args_ident = format_ident!("__{}Args", pascal);
    let spec_ident = format_ident!("__{}Spec", pascal);
    let root_ident = format_ident!("{}", pascal);

    let params = collect_params(&item)?;
    let root_style = params.iter().all(|(_, ty)| is_view_reference(ty));
    if !root_style && params.iter().any(|(_, ty)| matches!(ty, Type::Reference(_))) {
        return Err(syn::Error::new_spanned(
            &item.sig,
            "component parameters must be owned values; a component that takes only \
             `&View` references is installed as a root, not invoked",
        ));
    }

    let outputs = output_types(&item.sig.output);
    let arity = outputs.len();
    let user_output: Type = if arity == 1 {
        outputs[0].clone()
    } else {
        syn::parse_quote!((#(#outputs),*))
    };

    // Lower the body into tracked primitives.
    let mut lowerer = Lowerer::new(name_str.clone());
    let body = lowerer.block(&item.block);
    let body_block: syn::Block = syn::parse2(body)?;

    let mut impl_fn = item.clone();
    impl_fn.sig.ident = impl_name.clone();
    impl_fn.vis = Visibility::Inherited;
    *impl_fn.block = body_block;
    impl_fn.attrs.clear();

    let param_names: Vec<&syn::Ident> = params.iter().map(|(name, _)| name).collect();
    let param_types: Vec<&Type> = params.iter().map(|(_, ty)| ty).collect();
    let param_tokens: Vec<&FnArg> = item.sig.inputs.iter().collect();
    let return_type = item.sig.output.clone();

    let project_impl = if arity == 1 {
        quote! {
            fn project(output: &Self::Output, _index: usize) -> reactive::Value {
                reactive::value_of(output.clone())
            }
        }
    } else {
        let arms: Vec<TokenStream> = (0..arity)
            .map(|index| {
                let member = syn::Index::from(index);
                quote! { #index => reactive::value_of(output.#member.clone()), }
            })
            .collect();
        quote! {
            fn project(output: &Self::Output, index: usize) -> reactive::Value {
                match index {
                    #( #arms )*
                    _ => panic!("component output index out of range"),
                }
            }
        }
    };

    let reconstruct_impl = if arity == 1 {
        let ty = &outputs[0];
        quote! {
            fn reconstruct(parts: &[reactive::Value]) -> Self::Output {
                parts[0]
                    .downcast_ref::<#ty>()
                    .expect("component output type")
                    .clone()
            }
        }
    } else {
        let fields: Vec<TokenStream> = (0..arity)
            .map(|index| {
                let ty = &outputs[index];
                quote! {
                    parts[#index]
                        .downcast_ref::<#ty>()
                        .expect("component output type")
                        .clone(),
                }
            })
            .collect();
        quote! {
            fn reconstruct(parts: &[reactive::Value]) -> Self::Output {
                (#( #fields )*)
            }
        }
    };

    let eq_impl = if arity == 1 {
        let ty = &outputs[0];
        quote! {
            fn projection_eq(_index: usize, left: &reactive::Value, right: &reactive::Value) -> bool {
                reactive::values_equal::<#ty>(left, right)
            }
        }
    } else {
        let arms: Vec<TokenStream> = (0..arity)
            .map(|index| {
                let ty = &outputs[index];
                quote! { #index => reactive::values_equal::<#ty>(left, right), }
            })
            .collect();
        quote! {
            fn projection_eq(index: usize, left: &reactive::Value, right: &reactive::Value) -> bool {
                match index {
                    #( #arms )*
                    _ => false,
                }
            }
        }
    };

    let projection_impl = quote! {
        #project_impl
        #reconstruct_impl
        #eq_impl
    };

    let id_tokens = quote! {
        const ID: reactive::ComponentId = reactive::ComponentId(
            reactive::const_hash(concat!(module_path!(), "::", #name_str)) as u32,
        );
        const NAME: &'static str = concat!(module_path!(), "::", #name_str);
        const ARITY: usize = #arity;
    };

    if root_style {
        let mut view_types: Vec<Type> = params
            .iter()
            .map(|(_, ty)| match ty {
                Type::Reference(reference) => (*reference.elem).clone(),
                other => other.clone(),
            })
            .collect();
        // A root component's outputs are View handles: installing the root
        // registers their namespaces so definitions have somewhere to land.
        for output in &outputs {
            if is_view_type(output) {
                view_types.push(output.clone());
            }
        }
        let arg_bindings: Vec<syn::Ident> = params
            .iter()
            .enumerate()
            .map(|(index, _)| format_ident!("__arg{}", index))
            .collect();
        let root_call_args: Vec<TokenStream> =
            arg_bindings.iter().map(|binding| quote!(&#binding)).collect();
        Ok(quote! {
            #impl_fn

            #vis fn #name(#(#param_tokens),*) #return_type {
                #impl_name(#(#param_names),*)
            }

            #vis struct #root_ident;

            impl reactive::RootComponent for #root_ident {
                type Output = #user_output;

                #id_tokens

                const VIEWS: &'static [&'static reactive::ViewDescriptor] = &[
                    #( <#view_types as reactive::ViewSchema>::DESCRIPTOR, )*
                ];

                fn run() -> Self::Output {
                    #( let #arg_bindings: #view_types = ::std::default::Default::default(); )*
                    #impl_name(#(#root_call_args),*)
                }

                #projection_impl
            }
        })
    } else {
        let call_args: Vec<TokenStream> = param_names
            .iter()
            .map(|name| quote!(args.#name.clone()))
            .collect();
        Ok(quote! {
            #impl_fn

            #[derive(Clone, PartialEq, Hash)]
            struct #args_ident {
                #( #param_names: #param_types, )*
            }

            #vis fn #name(#(#param_tokens),*) #return_type {
                reactive::call_component::<#spec_ident>(#args_ident {
                    #( #param_names, )*
                })
            }

            struct #spec_ident;

            impl reactive::ComponentSpec for #spec_ident {
                type Args = #args_ident;
                type Output = #user_output;

                #id_tokens

                fn run(args: &Self::Args) -> Self::Output {
                    #impl_name(#(#call_args),*)
                }

                #projection_impl
            }
        })
    }
}

fn collect_params(item: &ItemFn) -> syn::Result<Vec<(syn::Ident, Type)>> {
    let mut params = Vec::new();
    for input in &item.sig.inputs {
        match input {
            FnArg::Receiver(receiver) => {
                return Err(syn::Error::new_spanned(
                    receiver,
                    "components are free functions, not methods",
                ));
            }
            FnArg::Typed(typed) => {
                let Pat::Ident(ident) = &*typed.pat else {
                    return Err(syn::Error::new_spanned(
                        &typed.pat,
                        "component parameters must be simple identifiers",
                    ));
                };
                params.push((ident.ident.clone(), (*typed.ty).clone()));
            }
        }
    }
    Ok(params)
}

/// Heuristic used for root outputs: an owned path type with an uppercase
/// initial that is not a standard container.
fn is_view_type(ty: &Type) -> bool {
    let Type::Path(path) = ty else {
        return false;
    };
    if path.qself.is_some() {
        return false;
    }
    let Some(segment) = path.path.segments.last() else {
        return false;
    };
    let name = segment.ident.to_string();
    name.chars().next().is_some_and(char::is_uppercase)
        && !matches!(
            name.as_str(),
            "String"
                | "Vec"
                | "Option"
                | "Result"
                | "Box"
                | "Rc"
                | "Arc"
                | "HashMap"
                | "BTreeMap"
                | "HashSet"
                | "BTreeSet"
                | "VecDeque"
                | "Path"
                | "PathBuf"
                | "Cow"
        )
}

fn is_view_reference(ty: &Type) -> bool {
    let Type::Reference(reference) = ty else {
        return false;
    };
    if reference.mutability.is_some() {
        return false;
    }
    let Type::Path(path) = &*reference.elem else {
        return false;
    };
    let Some(segment) = path.path.segments.last() else {
        return false;
    };
    let name = segment.ident.to_string();
    name.chars().next().is_some_and(char::is_uppercase)
        && !matches!(
            name.as_str(),
            "String" | "Vec" | "Option" | "Result" | "Box" | "Rc" | "Arc" | "Path" | "PathBuf"
        )
}

fn output_types(output: &ReturnType) -> Vec<Type> {
    match output {
        ReturnType::Default => vec![syn::parse_quote!(())],
        ReturnType::Type(_, ty) => match &**ty {
            Type::Tuple(tuple) if tuple.elems.is_empty() => vec![syn::parse_quote!(())],
            Type::Tuple(tuple) => tuple.elems.iter().cloned().collect(),
            other => vec![other.clone()],
        },
    }
}
