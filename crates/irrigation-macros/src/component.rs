//! `#[component]` and `#[irrigation]` expansion.
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
use quote::{ToTokens, format_ident, quote};
use syn::{Attribute, FnArg, Item, ItemFn, ItemMod, Pat, ReturnType, Type, Visibility};

use crate::attr::{
    ComponentConfig, ComponentKind, is_lint_policy, parse_component_config, take_component,
};
use crate::lower::Lowerer;
use crate::names::pascal_case;

/// Expands one `#[component]` function.
pub fn expand_component(args: TokenStream, input: TokenStream) -> TokenStream {
    let config = match parse_component_config(args) {
        Ok(config) => config,
        Err(error) => return error.to_compile_error(),
    };
    match syn::parse2::<ItemFn>(input) {
        Ok(item) => match expand_fn(item, &config) {
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
        return syn::Error::new_spanned(&module, "`#[irrigation]` requires an inline module")
            .to_compile_error();
    };
    let mut expanded = Vec::new();
    for item in items {
        match item {
            Item::Fn(mut function) => {
                if let Some(args) = take_component(&mut function.attrs) {
                    let config = match parse_component_config(args) {
                        Ok(config) => config,
                        Err(error) => {
                            expanded.push(error.to_compile_error());
                            continue;
                        }
                    };
                    match expand_fn(function, &config) {
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

fn expand_fn(item: ItemFn, config: &ComponentConfig) -> syn::Result<TokenStream> {
    let name = item.sig.ident.clone();
    let name_str = name.to_string();
    let vis = item.vis.clone();
    let pascal = pascal_case(&name_str);
    let impl_name = format_ident!("__{}_impl", name_str);
    let args_ident = format_ident!("{}Args", pascal);
    let spec_ident = format_ident!("{}", pascal);
    let legacy_args_ident = format_ident!("__{}Args", pascal);
    let legacy_spec_ident = format_ident!("__{}Spec", pascal);
    let root_ident = format_ident!("{}", pascal);

    let params = collect_params(&item)?;
    let all_view_refs = params.iter().all(|(_, ty)| is_view_reference(ty));
    let root_style = match config.kind {
        Some(ComponentKind::Root) => {
            if !all_view_refs {
                return Err(syn::Error::new_spanned(
                    &item.sig,
                    "`#[component(root)]` parameters must all be `&View` references; \
                     install-time configuration needs a `GeneralComponent`",
                ));
            }
            true
        }
        Some(ComponentKind::Callable) => false,
        None => all_view_refs,
    };
    if !root_style
        && params
            .iter()
            .any(|(_, ty)| matches!(ty, Type::Reference(_)) && !is_view_reference(ty))
    {
        return Err(syn::Error::new_spanned(
            &item.sig,
            "component parameters must be owned values or `&View` references; \
             a borrowed value with a host lifetime cannot be interned",
        ));
    }

    let keyed = if let Some(key_type) = &config.keyed {
        if config.kind != Some(ComponentKind::Callable) {
            return Err(syn::Error::new_spanned(
                &item.sig,
                "a keyed component must be declared `#[component(callable, keyed = Type)]`",
            ));
        }
        if params.len() != 1 {
            return Err(syn::Error::new_spanned(
                &item.sig,
                "a keyed component currently takes exactly one key parameter",
            ));
        }
        let (param_name, param_type) = &params[0];
        if param_type.to_token_stream().to_string() != key_type.to_token_stream().to_string() {
            return Err(syn::Error::new_spanned(
                param_type,
                "the key parameter type must match `keyed = Type`",
            ));
        }
        Some((key_type, param_name))
    } else {
        None
    };

    if config.keys.is_some()
        && (config.kind != Some(ComponentKind::Callable) || config.keyed.is_none())
    {
        return Err(syn::Error::new_spanned(
            &item.sig,
            "`keys = View` requires `#[component(callable, keyed = Type, ...)]`",
        ));
    }

    if !config.plans.is_empty() && config.kind != Some(ComponentKind::Callable) {
        return Err(syn::Error::new_spanned(
            &item.sig,
            "`plans = [...]` requires `#[component(callable, ...)]`",
        ));
    }
    let plan_statement = if config.plans.is_empty() {
        quote!()
    } else {
        let plans = &config.plans;
        quote! {
            irrigation::plan_ports(&[#(#plans),*], "component plans");
        }
    };

    let outputs = output_types(&item.sig.output);
    let arity = outputs.len();
    let user_output: Type = if arity == 1 {
        outputs[0].clone()
    } else {
        syn::parse_quote!((#(#outputs),*))
    };

    // Lower the body into tracked primitives. The label lets a top-level
    // `return` leave the component without leaving the lowered function early
    // in a way that would bypass the generated body scope.
    let mut lowerer = Lowerer::new();
    lowerer.set_policy(config.strict, config.allowed.clone());
    lowerer.set_return_label(syn::Lifetime::new(
        "'__irrigation_component",
        proc_macro2::Span::call_site(),
    ));
    let body = lowerer.block(&item.block);
    let body_block: syn::Block = syn::parse2(quote!({
        '__irrigation_component: {
            #plan_statement
            #body
        }
    }))?;

    // The user's attributes describe the item being replaced, so they follow
    // its replacement. They are classified by target: a `#[cfg]` must hide the
    // whole generated unit, `#[inline]` belongs on functions only, and an
    // unknown attribute macro is only safe where the user put it — on a
    // function.
    let attrs = item.attrs.clone();
    let impl_fn_attrs = attributes_for(&attrs, AttrSite::ImplFn);
    let public_fn_attrs = attributes_for(&attrs, AttrSite::PublicFn);
    let struct_attrs = attributes_for(&attrs, AttrSite::Struct);
    let impl_attrs = attributes_for(&attrs, AttrSite::Impl);

    let mut impl_fn = item.clone();
    impl_fn.sig.ident = impl_name.clone();
    impl_fn.vis = Visibility::Inherited;
    *impl_fn.block = body_block;
    impl_fn.attrs = impl_fn_attrs;

    let param_names: Vec<&syn::Ident> = params.iter().map(|(name, _)| name).collect();
    let param_tokens: Vec<&FnArg> = item.sig.inputs.iter().collect();
    let return_type = item.sig.output.clone();

    let (keys_view_tokens, keys_accessor) = match config.keys.as_ref() {
        None => (None, None),
        Some(syn::Type::Tuple(tuple)) => {
            if tuple.elems.len() != 2 {
                return Err(syn::Error::new_spanned(
                    tuple,
                    "`keys = (View, accessor)` expects exactly two elements",
                ));
            }
            let view = tuple.elems[0].clone();
            let accessor = match &tuple.elems[1] {
                syn::Type::Path(path) => path.path.get_ident().cloned(),
                _ => None,
            };
            let Some(accessor) = accessor else {
                return Err(syn::Error::new_spanned(
                    &tuple.elems[1],
                    "the key accessor must be a method name",
                ));
            };
            (Some(view), Some(accessor))
        }
        Some(view) => (Some(view.clone()), Some(format_ident!("field0"))),
    };
    let keyed_type_token = config.keyed.as_ref();
    let call_all_method =
        if let (Some(keys_view), Some(keys_accessor)) = (keys_view_tokens, keys_accessor) {
            let key_type = keyed_type_token.expect("keys requires keyed");
            quote! {
                    /// Enumerates keys from the configured View/accessor and calls
                    /// [`Self::call_keyed`].
                    pub fn call_all(
                        runtime: &irrigation::Runtime,
                    ) -> ::std::vec::Vec<irrigation::Result<#user_output>>
                    where
                        #key_type: Clone + Send + 'static,
                    {
                        let snapshot = runtime.snapshot();
                        let source = snapshot.view::<#keys_view>();
                        let keys = source.#keys_accessor();
                        Self::call_keyed_from(runtime, &keys)
                    }
            }
        } else {
            quote!()
        };

    let keyed_impl = keyed.map(|(key_type, param_name)| {
        quote! {
            impl #spec_ident {
                /// Calls one independent invocation per key and returns their
                /// outputs in key order. Keys run on the runtime's worker pool;
                /// a key that reads another waits through the normal unit
                /// protocol.
                pub fn call_keyed(
                    runtime: &irrigation::Runtime,
                    keys: impl IntoIterator<Item = #key_type>,
                ) -> ::std::vec::Vec<irrigation::Result<#user_output>>
                where
                    #key_type: Clone + Send + 'static,
                {
                    let keys: ::std::vec::Vec<#key_type> = keys.into_iter().collect();
                    if keys.is_empty() {
                        return ::std::vec::Vec::new();
                    }
                    let workers = runtime.worker_count().min(keys.len());
                    if workers <= 1 {
                        return keys
                            .into_iter()
                            .map(|key| {
                                runtime.call::<#spec_ident>(#args_ident { #param_name: key })
                            })
                            .collect();
                    }
                    let slots: ::std::vec::Vec<
                        ::std::sync::Mutex<::std::option::Option<irrigation::Result<#user_output>>>,
                    > = (0..keys.len())
                        .map(|_| ::std::sync::Mutex::new(::std::option::Option::None))
                        .collect();
                    let next = ::std::sync::atomic::AtomicUsize::new(0);
                    ::std::thread::scope(|scope| {
                        for _ in 0..workers {
                            let keys = &keys;
                            let slots = &slots;
                            let next = &next;
                            scope.spawn(move || loop {
                                let index =
                                    next.fetch_add(1, ::std::sync::atomic::Ordering::SeqCst);
                                if index >= keys.len() {
                                    break;
                                }
                                let result = ::std::panic::catch_unwind(
                                    ::std::panic::AssertUnwindSafe(|| {
                                        runtime.call::<#spec_ident>(#args_ident {
                                            #param_name: keys[index].clone(),
                                        })
                                    }),
                                )
                                .unwrap_or_else(|_| {
                                    Err(irrigation::ComputeError::internal(
                                        "keyed producer panicked",
                                    ))
                                });
                                *slots[index].lock().expect("keyed slot") = Some(result);
                            });
                        }
                    });
                    slots
                        .into_iter()
                        .map(|slot| {
                            slot.into_inner()
                                .expect("keyed slot")
                                .unwrap_or_else(|| {
                                    Err(irrigation::ComputeError::internal(
                                        "keyed producer produced no output",
                                    ))
                                })
                        })
                        .collect()
                }

                /// Enumerates keys from a collection handle and calls
                /// [`Self::call_keyed`].
                pub fn call_keyed_from(
                    runtime: &irrigation::Runtime,
                    source: &impl irrigation::KeySet<#key_type>,
                ) -> ::std::vec::Vec<irrigation::Result<#user_output>>
                where
                    #key_type: Clone + Send + 'static,
                {
                    Self::call_keyed(runtime, source.keys())
                }
                #call_all_method
            }
        }
    });

    let project_impl = if arity == 1 {
        quote! {
            fn project(output: &Self::Output, _index: usize) -> irrigation::Value {
                irrigation::value_of(output.clone())
            }
        }
    } else {
        let arms: Vec<TokenStream> = (0..arity)
            .map(|index| {
                let member = syn::Index::from(index);
                quote! { #index => irrigation::value_of(output.#member.clone()), }
            })
            .collect();
        quote! {
            fn project(output: &Self::Output, index: usize) -> irrigation::Value {
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
            fn reconstruct(parts: &[irrigation::Value]) -> Self::Output {
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
            fn reconstruct(parts: &[irrigation::Value]) -> Self::Output {
                (#( #fields )*)
            }
        }
    };

    let eq_impl = if arity == 1 {
        let ty = &outputs[0];
        quote! {
            fn projection_eq(_index: usize, left: &irrigation::Value, right: &irrigation::Value) -> bool {
                irrigation::values_equal::<#ty>(left, right)
            }
        }
    } else {
        let arms: Vec<TokenStream> = (0..arity)
            .map(|index| {
                let ty = &outputs[index];
                quote! { #index => irrigation::values_equal::<#ty>(left, right), }
            })
            .collect();
        quote! {
            fn projection_eq(index: usize, left: &irrigation::Value, right: &irrigation::Value) -> bool {
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
        const ID: irrigation::ComponentId = irrigation::ComponentId(
            irrigation::const_hash(concat!(module_path!(), "::", #name_str)),
        );
        const NAME: &'static str = concat!(module_path!(), "::", #name_str);
        const ARITY: usize = #arity;
    };

    let output_names: Vec<syn::Ident> = (0..arity).map(|i| format_ident!("OUTPUT_{i}")).collect();
    let output_indices: Vec<usize> = (0..arity).collect();

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
        let root_call_args: Vec<TokenStream> = arg_bindings
            .iter()
            .map(|binding| quote!(&#binding))
            .collect();
        Ok(quote! {
            #impl_fn

            #(#public_fn_attrs)*
            #vis fn #name(#(#param_tokens),*) #return_type {
                // A component body executes in its own child scope even when a
                // caller invokes it as a plain function: sibling component
                // bodies must not share expression-site identity.
                irrigation::scope(
                    <#root_ident as irrigation::RootComponent>::ID.0,
                    #name_str,
                    move || #impl_name(#(#param_names),*),
                )
            }

            #(#struct_attrs)*
            #vis struct #root_ident;

            #(#impl_attrs)*
            impl irrigation::RootComponent for #root_ident {
                type Output = #user_output;

                #id_tokens

                const VIEWS: &'static [&'static irrigation::ViewDescriptor] = &[
                    #( <#view_types as irrigation::ViewSchema>::DESCRIPTOR, )*
                ];

                fn run() -> Self::Output {
                    #( let #arg_bindings: #view_types = ::std::default::Default::default(); )*
                    #impl_name(#(#root_call_args),*)
                }

                #projection_impl
            }

            #(#impl_attrs)*
            impl #root_ident {
                #( pub const #output_names: usize = #output_indices; )*
            }
        })
    } else {
        // A callable component may take borrowed View handles. The invocation
        // argument is the owned ZST; `run` borrows it for the lowered body.
        let arg_types: Vec<TokenStream> = params
            .iter()
            .map(|(_, ty)| match ty {
                Type::Reference(reference) => (*reference.elem).to_token_stream(),
                other => other.to_token_stream(),
            })
            .collect();
        let call_args: Vec<TokenStream> = params
            .iter()
            .map(|(name, ty)| {
                if is_view_reference(ty) {
                    quote!(&args.#name)
                } else {
                    quote!(args.#name.clone())
                }
            })
            .collect();
        let construct_args: Vec<TokenStream> = params
            .iter()
            .map(|(name, ty)| {
                if is_view_reference(ty) {
                    quote!(*#name)
                } else {
                    quote!(#name)
                }
            })
            .collect();
        Ok(quote! {
            #impl_fn

            #(#struct_attrs)*
            #[derive(Clone, PartialEq, Hash)]
            #vis struct #args_ident {
                #( #vis #param_names: #arg_types, )*
            }

            #[doc(hidden)]
            #vis type #legacy_args_ident = #args_ident;

            #(#public_fn_attrs)*
            #vis fn #name(#(#param_tokens),*) #return_type {
                irrigation::call_component::<#spec_ident>(#args_ident {
                    #( #param_names: #construct_args, )*
                })
            }

            #(#struct_attrs)*
            #vis struct #spec_ident;

            #[doc(hidden)]
            #vis type #legacy_spec_ident = #spec_ident;

            #keyed_impl

            #(#impl_attrs)*
            impl irrigation::ComponentSpec for #spec_ident {
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

/// Which generated item an attribute is being forwarded to.
#[derive(Clone, Copy)]
enum AttrSite {
    /// The private function that runs the lowered body.
    ImplFn,
    /// The public component entry point.
    PublicFn,
    /// A generated struct (argument record, component spec, root marker).
    Struct,
    /// A generated trait impl.
    Impl,
}

fn attributes_for(attrs: &[Attribute], site: AttrSite) -> Vec<Attribute> {
    attrs
        .iter()
        .filter(|attr| applies_to(attr, site))
        .cloned()
        .collect()
}

fn applies_to(attr: &Attribute, site: AttrSite) -> bool {
    let path = attr.path();
    if path.is_ident("cfg") || path.is_ident("cfg_attr") {
        return true;
    }
    if is_lint_policy(attr) {
        return crate::attr::is_forwardable_lint_policy(attr);
    }
    if path.is_ident("non_exhaustive") {
        return matches!(site, AttrSite::Struct);
    }
    if ["inline", "track_caller", "cold", "target_feature"]
        .iter()
        .any(|name| path.is_ident(name))
    {
        return matches!(site, AttrSite::ImplFn | AttrSite::PublicFn);
    }
    if path.is_ident("doc") || path.is_ident("must_use") {
        return matches!(site, AttrSite::PublicFn | AttrSite::Struct);
    }
    // Only the entry point: generated impls name the generated structs, and a
    // `#[deprecated]` there would warn from code the user did not write.
    if path.is_ident("deprecated") {
        return matches!(site, AttrSite::PublicFn);
    }
    matches!(site, AttrSite::ImplFn | AttrSite::PublicFn)
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

#[cfg(test)]
mod tests {
    use super::*;
    use quote::quote;

    #[test]
    fn strict_components_reject_undeclared_calls() {
        let tokens = expand_component(
            quote!(strict),
            quote!(
                fn c(x: u64) -> u64 {
                    helper(x)
                }
            ),
        );
        assert!(
            tokens.to_string().contains("opaque call"),
            "strict mode must reject an undeclared call"
        );
    }

    #[test]
    fn strict_components_allow_declared_primitives() {
        let tokens = expand_component(
            quote!(strict, allows = [helper]),
            quote!(
                fn c(x: u64) -> u64 {
                    helper(x)
                }
            ),
        );
        assert!(!tokens.to_string().contains("opaque call"));
    }

    #[test]
    fn mutable_locals_are_rejected() {
        let tokens = expand_component(
            quote!(),
            quote!(
                fn c(x: u64) -> u64 {
                    let mut out = Vec::new();
                    out.push(x);
                    out.len() as u64
                }
            ),
        );
        assert!(
            tokens.to_string().contains("let mut"),
            "mutable locals must be rejected instead of silently losing writes"
        );
    }

    #[test]
    fn attributes_follow_every_generated_item() {
        let tokens = expand_component(
            quote!(),
            quote!(
                /// Docs for the component.
                #[cfg(feature = "demo")]
                fn c(x: u64) -> u64 {
                    x
                }
            ),
        )
        .to_string();
        assert!(
            tokens.contains("Docs for the component."),
            "the docs must reach the public entry point: {tokens}"
        );
        // The private implementation, the argument struct, the entry point,
        // the spec struct, and the spec impl.
        assert_eq!(
            tokens.matches("cfg (feature = \"demo\")").count(),
            5,
            "every generated item must carry the gate: {tokens}"
        );
    }

    #[test]
    fn root_components_gate_their_marker_too() {
        let tokens = expand_component(
            quote!(),
            quote!(
                #[cfg(feature = "demo")]
                fn c(view: &Widgets) -> u64 {
                    view.count()
                }
            ),
        )
        .to_string();
        assert_eq!(
            tokens.matches("cfg (feature = \"demo\")").count(),
            5,
            "the entry point, implementation, root marker, impl, and output constants all need the gate: {tokens}"
        );
    }

    #[test]
    fn function_only_attributes_stay_on_functions() {
        let tokens = expand_component(
            quote!(),
            quote!(
                #[inline]
                fn c(x: u64) -> u64 {
                    x
                }
            ),
        )
        .to_string();
        assert_eq!(
            tokens.matches("inline").count(),
            2,
            "`#[inline]` applies to the two generated functions only: {tokens}"
        );
    }

    #[test]
    fn components_are_permissive_by_default() {
        let tokens = expand_component(
            quote!(),
            quote!(
                fn c(x: u64) -> u64 {
                    helper(x)
                }
            ),
        );
        assert!(!tokens.to_string().contains("opaque call"));
    }
}
