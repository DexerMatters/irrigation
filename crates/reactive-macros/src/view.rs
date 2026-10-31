//! `#[view(source)]` and `#[view(derived)]` expansion.
//!
//! A View declaration generates:
//!
//! * a zero-sized handle whose accessors are tracked reads;
//! * an immutable content type and its vtable operations;
//! * a snapshot handle whose reads are pure;
//! * for source Views, a transaction editor;
//! * for derived Views, definition methods.

use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::{
    Data, DeriveInput, Field, Fields, GenericArgument, Ident, PathArguments, Result, Type,
    Visibility,
};

use crate::attr::ViewKindArg;

/// The shape of one View field.
#[derive(Clone)]
enum FieldShape {
    Scalar(Type),
    Map(Type, Type),
    List(Type),
    Set(Type),
    Graph(Type, Type),
    Tree(Type),
}

/// One declared field after parsing.
struct FieldSpec {
    ident: Ident,
    ty: Type,
    shape: FieldShape,
    index: u16,
    named: bool,
    vis: Visibility,
}

/// Expands a View declaration.
pub fn expand(args: TokenStream, input: TokenStream) -> TokenStream {
    match expand_inner(args, input) {
        Ok(tokens) => tokens,
        Err(error) => error.to_compile_error(),
    }
}

fn expand_inner(args: TokenStream, input: TokenStream) -> Result<TokenStream> {
    let attr: syn::Attribute = syn::parse_quote!(#[view(#args)]);
    let kind = crate::attr::parse_view_kind(&attr)?;
    let item: DeriveInput = syn::parse2(input)?;
    let name = item.ident.clone();
    let vis = item.vis.clone();
    let name_str = name.to_string();

    let Data::Struct(data) = &item.data else {
        return Err(syn::Error::new_spanned(
            &item,
            "`#[view]` can only be applied to a struct",
        ));
    };

    let mut fields = Vec::new();
    match &data.fields {
        Fields::Named(named) => {
            for (index, field) in named.named.iter().enumerate() {
                fields.push(parse_field(field, index, true)?);
            }
        }
        Fields::Unnamed(unnamed) => {
            for (index, field) in unnamed.unnamed.iter().enumerate() {
                fields.push(parse_field(field, index, false)?);
            }
        }
        Fields::Unit => {}
    }

    let derived = matches!(kind, ViewKindArg::Derived);
    for field in &fields {
        match &field.shape {
            FieldShape::Graph(_, _) | FieldShape::Tree(_) => {
                return Err(syn::Error::new_spanned(
                    &field.ty,
                    "graph and tree fields are not yet supported by `#[view]`; \
                     use `#[tree_view]` for ownership trees",
                ));
            }
            _ => {}
        }
    }

    let kind_ident = match kind {
        ViewKindArg::Source => quote!(Source),
        ViewKindArg::Derived => quote!(Derived),
    };
    let view_const = format_ident!("__{}_view_id", name_str.to_lowercase());
    let content_ident = format_ident!("{}Content", name);
    let snapshot_ident = format_ident!("{}Snapshot", name);
    let editor_ident = format_ident!("{}Editor", name);
    let descriptor_ident = format_ident!("__{}_descriptor", name_str.to_lowercase());
    let prefix = name_str.to_lowercase();

    let empty_fn = format_ident!("__{}_empty", prefix);
    let define_fn = format_ident!("__{}_define", prefix);
    let retract_fn = format_ident!("__{}_retract", prefix);
    let contains_fn = format_ident!("__{}_contains", prefix);
    let get_fn = format_ident!("__{}_get", prefix);
    let eq_fn = format_ident!("__{}_eq", prefix);
    let debug_fn = format_ident!("__{}_debug", prefix);

    let markers: Vec<TokenStream> = fields
        .iter()
        .map(|field| marker_for(&field.shape, derived))
        .collect();
    let field_idents: Vec<&Ident> = fields.iter().map(|field| &field.ident).collect();
    let field_names: Vec<String> = fields
        .iter()
        .map(|field| field.ident.to_string())
        .collect();
    let field_vis: Vec<&Visibility> = fields.iter().map(|field| &field.vis).collect();
    let indices: Vec<u16> = fields.iter().map(|field| field.index).collect();

    // ---------------------------------------------------------------- content
    let content = quote! {
        #[derive(Clone, PartialEq)]
        #vis struct #content_ident {
            #( #field_vis #field_idents: <#markers as reactive::ViewField>::Storage, )*
        }

        impl #content_ident {
            #vis fn empty() -> Self {
                Self {
                    #( #field_idents: <#markers as reactive::ViewField>::empty(), )*
                }
            }
        }
    };

    // --------------------------------------------------------------- vtable
    let define_arms: Vec<TokenStream> = fields
        .iter()
        .map(|field| {
            let ident = &field.ident;
            let index = field.index;
            let marker = marker_for(&field.shape, derived);
            match field.shape {
                _ => quote! {
                    #index => {
                        let entity = key.entity.as_deref().unwrap_or(&());
                        next.#ident = <#marker as reactive::ViewField>::define(&next.#ident, entity, value);
                    },
                },
            }
        })
        .collect();

    let retract_arms: Vec<TokenStream> = fields
        .iter()
        .map(|field| {
            let ident = &field.ident;
            let index = field.index;
            let marker = marker_for(&field.shape, derived);
            match field.shape {
                FieldShape::Scalar(_) => quote! { #index => {}, },
                _ => quote! {
                    #index => {
                        let entity = key.entity.as_deref().unwrap_or(&());
                        next.#ident = <#marker as reactive::ViewField>::retract(&next.#ident, entity);
                    },
                },
            }
        })
        .collect();

    let contains_arms: Vec<TokenStream> = fields
        .iter()
        .map(|field| {
            let ident = &field.ident;
            let index = field.index;
            let marker = marker_for(&field.shape, derived);
            match field.shape {
                FieldShape::Scalar(_) => quote! { #index => true, },
                _ => quote! {
                    #index => key.entity.as_deref().is_some_and(|entity| {
                        <#marker as reactive::ViewField>::contains(&next.#ident, entity)
                    }),
                },
            }
        })
        .collect();

    let get_arms: Vec<TokenStream> = fields
        .iter()
        .map(|field| {
            let ident = &field.ident;
            let index = field.index;
            let marker = marker_for(&field.shape, derived);
            match field.shape {
                FieldShape::Scalar(_) => quote! {
                    #index => Some(reactive::value_of(next.#ident.clone())),
                },
                _ => quote! {
                    #index => match key.kind {
                        reactive::UnitKind::Structure => Some(reactive::value_of(next.#ident.clone())),
                        reactive::UnitKind::Membership => key.entity.as_deref().map(|entity| {
                            reactive::value_of(<#marker as reactive::ViewField>::contains(&next.#ident, entity))
                        }),
                        reactive::UnitKind::Value => key.entity.as_deref().and_then(|entity| {
                            <#marker as reactive::ViewField>::get(&next.#ident, entity)
                        }),
                        _ => None,
                    },
                },
            }
        })
        .collect();

    let vtable = quote! {
        fn #empty_fn() -> ::std::sync::Arc<dyn ::std::any::Any + Send + Sync> {
            ::std::sync::Arc::new(#content_ident::empty())
        }

        fn #define_fn(
            content: &::std::sync::Arc<dyn ::std::any::Any + Send + Sync>,
            key: &reactive::UnitKey,
            value: reactive::Value,
        ) -> ::std::sync::Arc<dyn ::std::any::Any + Send + Sync> {
            let mut next = content
                .downcast_ref::<#content_ident>()
                .cloned()
                .unwrap_or_else(#content_ident::empty);
            match key.field.0 {
                #( #define_arms )*
                _ => {}
            }
            ::std::sync::Arc::new(next)
        }

        fn #retract_fn(
            content: &::std::sync::Arc<dyn ::std::any::Any + Send + Sync>,
            key: &reactive::UnitKey,
        ) -> ::std::sync::Arc<dyn ::std::any::Any + Send + Sync> {
            let mut next = content
                .downcast_ref::<#content_ident>()
                .cloned()
                .unwrap_or_else(#content_ident::empty);
            match key.field.0 {
                #( #retract_arms )*
                _ => {}
            }
            ::std::sync::Arc::new(next)
        }

        fn #contains_fn(
            content: &::std::sync::Arc<dyn ::std::any::Any + Send + Sync>,
            key: &reactive::UnitKey,
        ) -> bool {
            let Some(next) = content.downcast_ref::<#content_ident>() else {
                return false;
            };
            match key.field.0 {
                #( #contains_arms )*
                _ => false,
            }
        }

        fn #get_fn(
            content: &::std::sync::Arc<dyn ::std::any::Any + Send + Sync>,
            key: &reactive::UnitKey,
        ) -> Option<reactive::Value> {
            let next = content.downcast_ref::<#content_ident>()?;
            match key.field.0 {
                #( #get_arms )*
                _ => None,
            }
        }

        fn #eq_fn(
            left: &::std::sync::Arc<dyn ::std::any::Any + Send + Sync>,
            right: &::std::sync::Arc<dyn ::std::any::Any + Send + Sync>,
        ) -> bool {
            left.downcast_ref::<#content_ident>() == right.downcast_ref::<#content_ident>()
        }

        fn #debug_fn(
            _content: &::std::sync::Arc<dyn ::std::any::Any + Send + Sync>,
        ) -> String {
            String::from(#name_str)
        }

        const #view_const: reactive::ViewId = reactive::ViewId(
            reactive::const_hash(concat!(module_path!(), "::", #name_str)) as u32,
        );

        static #descriptor_ident: reactive::ViewDescriptor = reactive::ViewDescriptor {
            id: #view_const,
            name: concat!(module_path!(), "::", #name_str),
            kind: reactive::ViewKind::#kind_ident,
            fields: &[
                #( reactive::FieldDescriptor {
                    name: #field_names,
                    kind: <#markers as reactive::ViewField>::KIND,
                }, )*
            ],
            content: reactive::ContentVtable {
                empty: #empty_fn,
                define: #define_fn,
                retract: #retract_fn,
                contains: #contains_fn,
                get: #get_fn,
                eq: #eq_fn,
                debug: #debug_fn,
            },
        };
    };

    // ------------------------------------------------------------ live reads
    let live_accessors: Vec<TokenStream> = fields
        .iter()
        .map(|field| {
            let ident = &field.ident;
            let index = field.index;
            let marker = marker_for(&field.shape, derived);
            match &field.shape {
                FieldShape::Scalar(ty) => quote! {
                    #vis fn #ident(&self) -> #ty {
                        <#marker as reactive::ViewFieldHandle>::handle(
                            #view_const,
                            reactive::FieldId(#index),
                            |content| content
                                .downcast_ref::<#content_ident>()
                                .expect(#name_str)
                                .#ident
                                .clone(),
                        ).read()
                    }
                },
                _ => quote! {
                    #vis fn #ident(&self) -> <#marker as reactive::ViewFieldHandle>::Handle {
                        <#marker as reactive::ViewFieldHandle>::handle(
                            #view_const,
                            reactive::FieldId(#index),
                            |content| content
                                .downcast_ref::<#content_ident>()
                                .expect(#name_str)
                                .#ident
                                .clone(),
                        )
                    }
                },
            }
        })
        .collect();

    // -------------------------------------------------------- snapshot reads
    let snapshot_accessors: Vec<TokenStream> = fields
        .iter()
        .map(|field| {
            let ident = &field.ident;
            match &field.shape {
                FieldShape::Scalar(ty) => quote! {
                    #vis fn #ident(&self) -> #ty { self.content.#ident.clone() }
                },
                FieldShape::Map(key, value) => {
                    let getter = format_ident!("{}_get", ident);
                    quote! {
                        #vis fn #ident(&self) -> Vec<(#key, #value)> {
                            self.content.#ident.iter().map(|(key, value)| (key.clone(), value.clone())).collect()
                        }
                        #vis fn #getter(&self, key: &#key) -> Option<#value> {
                            self.content.#ident.get(key).cloned()
                        }
                    }
                }
                FieldShape::List(elem) => {
                    if derived {
                        quote! {
                            #vis fn #ident(&self) -> Vec<#elem> {
                                self.content.#ident.iter().cloned().collect()
                            }
                        }
                    } else {
                        quote! {
                            #vis fn #ident(&self) -> Vec<#elem> {
                                self.content.#ident.iter().map(|(_, value)| value.clone()).collect()
                            }
                        }
                    }
                }
                FieldShape::Set(elem) => quote! {
                    #vis fn #ident(&self) -> Vec<#elem> {
                        self.content.#ident.iter().cloned().collect()
                    }
                },
                FieldShape::Graph(_, _) | FieldShape::Tree(_) => quote! {},
            }
        })
        .collect();

    // ------------------------------------------------------------ edit access
    let edit_accessors: Vec<TokenStream> = if derived {
        Vec::new()
    } else {
        fields
            .iter()
            .map(|field| {
                let ident = &field.ident;
                let setter = format_ident!("set_{}", ident);
                let index = field.index;
                let marker = marker_for(&field.shape, derived);
                match &field.shape {
                    FieldShape::Scalar(ty) => quote! {
                        #vis fn #setter(&mut self, value: #ty) {
                            self.content.#ident = value;
                            self.edit.set(
                                reactive::UnitKey::field(
                                    #view_const,
                                    reactive::FieldId(#index),
                                    reactive::UnitKind::Value,
                                ),
                                self.content.#ident.clone(),
                            );
                        }
                    },
                    _ => quote! {
                        #vis fn #ident(&mut self) -> <#marker as reactive::ViewFieldHandle>::Editor<'_> {
                            <#marker as reactive::ViewFieldHandle>::editor(
                                &mut *self.edit,
                                #view_const,
                                reactive::FieldId(#index),
                                &mut self.content.#ident,
                            )
                        }
                    },
                }
            })
            .collect()
    };

    // ------------------------------------------------------ derived definitions
    let define_accessors: Vec<TokenStream> = if derived {
        fields
            .iter()
            .map(|field| {
                let ident = &field.ident;
                let define_name = format_ident!("define_{}", ident);
                let index = field.index;
                match &field.shape {
                    FieldShape::Scalar(ty) => quote! {
                        #vis fn #define_name(&self, value: #ty) {
                            reactive::ensure_view(<#name as reactive::ViewSchema>::DESCRIPTOR);
                            reactive::define_unit(
                                reactive::UnitKey::field(
                                    #view_const,
                                    reactive::FieldId(#index),
                                    reactive::UnitKind::Value,
                                ),
                                value,
                                #name_str,
                            );
                        }
                    },
                    FieldShape::List(elem) => quote! {
                        #vis fn #define_name<I>(&self, items: I)
                        where I: IntoIterator<Item = #elem> {
                            reactive::ensure_view(<#name as reactive::ViewSchema>::DESCRIPTOR);
                            self.#ident().define_all(items, #name_str);
                        }
                    },
                    FieldShape::Set(elem) => quote! {
                        #vis fn #define_name<I>(&self, members: I)
                        where I: IntoIterator<Item = #elem> {
                            reactive::ensure_view(<#name as reactive::ViewSchema>::DESCRIPTOR);
                            self.#ident().define_all(members, #name_str);
                        }
                    },
                    FieldShape::Map(key, value) => quote! {
                        #vis fn #define_name(&self, key: #key, value: #value) {
                            reactive::ensure_view(<#name as reactive::ViewSchema>::DESCRIPTOR);
                            self.#ident().define(key, value, #name_str);
                        }
                    },
                    FieldShape::Graph(_, _) | FieldShape::Tree(_) => quote! {},
                }
            })
            .collect()
    } else {
        Vec::new()
    };

    // ------------------------------------------------ single-field forwarding
    let forwarding = single_field_forwarding(&fields, &name, &name_str, derived);

    let schema_impl = quote! {
        impl reactive::ViewSchema for #name {
            const DESCRIPTOR: &'static reactive::ViewDescriptor = &#descriptor_ident;
            type Content = #content_ident;
            type Snapshot<'a> = #snapshot_ident<'a>;

            fn snapshot<'a>(
                content: &'a ::std::sync::Arc<dyn ::std::any::Any + Send + Sync>,
            ) -> Self::Snapshot<'a> {
                #snapshot_ident {
                    content: content.downcast_ref::<#content_ident>().expect(#name_str),
                }
            }
        }

        #vis struct #snapshot_ident<'a> {
            content: &'a #content_ident,
        }

        impl #snapshot_ident<'_> {
            #( #snapshot_accessors )*
        }
    };

    let source_impl = if derived {
        quote! {
            impl reactive::DerivedView for #name {}

            impl #name {
                #( #define_accessors )*
                #forwarding
            }
        }
    } else {
        quote! {
            impl reactive::SourceView for #name {}

            impl reactive::EditableView for #name {
                type Editor<'a> = #editor_ident<'a>;

                fn edit<'a>(edit: &'a mut reactive::Edit) -> Self::Editor<'a> {
                    edit.ensure_view(<#name as reactive::ViewSchema>::DESCRIPTOR);
                    let content = (*edit.content::<#content_ident>(#view_const)).clone();
                    #editor_ident { edit, content }
                }
            }

            #vis struct #editor_ident<'a> {
                edit: &'a mut reactive::Edit,
                content: #content_ident,
            }

            impl #editor_ident<'_> {
                #( #edit_accessors )*
            }

            impl Drop for #editor_ident<'_> {
                fn drop(&mut self) {
                    self.edit.set_content(
                        #view_const,
                        ::std::sync::Arc::new(self.content.clone()),
                        #eq_fn,
                    );
                }
            }
        }
    };

    Ok(quote! {
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        #vis struct #name;

        #content
        #vtable
        #schema_impl
        #source_impl

        impl #name {
            #( #live_accessors )*
        }
    })
}

fn parse_field(field: &Field, index: usize, named: bool) -> Result<FieldSpec> {
    let ident = field
        .ident
        .clone()
        .unwrap_or_else(|| format_ident!("field{}", index, span = Span::call_site()));
    let shape = shape_of(&field.ty)?;
    Ok(FieldSpec {
        ident,
        ty: field.ty.clone(),
        shape,
        index: index as u16,
        named,
        vis: field.vis.clone(),
    })
}

fn shape_of(ty: &Type) -> Result<FieldShape> {
    let Type::Path(path) = ty else {
        return Ok(FieldShape::Scalar(ty.clone()));
    };
    let Some(segment) = path.path.segments.last() else {
        return Ok(FieldShape::Scalar(ty.clone()));
    };
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return Ok(FieldShape::Scalar(ty.clone()));
    };
    let types: Vec<Type> = args
        .args
        .iter()
        .filter_map(|arg| match arg {
            GenericArgument::Type(ty) => Some(ty.clone()),
            _ => None,
        })
        .collect();
    Ok(match (segment.ident.to_string().as_str(), types.len()) {
        ("Map", 2) => FieldShape::Map(types[0].clone(), types[1].clone()),
        ("List", 1) => FieldShape::List(types[0].clone()),
        ("Set", 1) => FieldShape::Set(types[0].clone()),
        ("Graph", 2) => FieldShape::Graph(types[0].clone(), types[1].clone()),
        ("Tree", 1) => FieldShape::Tree(types[0].clone()),
        _ => FieldShape::Scalar(ty.clone()),
    })
}

fn marker_for(shape: &FieldShape, derived: bool) -> TokenStream {
    match shape {
        FieldShape::Scalar(ty) => quote!(reactive::Scalar<#ty>),
        FieldShape::Map(key, value) => quote!(reactive::Map<#key, #value>),
        FieldShape::List(elem) => {
            if derived {
                quote!(reactive::DerivedList<#elem>)
            } else {
                quote!(reactive::SourceList<#elem>)
            }
        }
        FieldShape::Set(elem) => {
            if derived {
                quote!(reactive::DerivedSet<#elem>)
            } else {
                quote!(reactive::SourceSet<#elem>)
            }
        }
        FieldShape::Graph(node, edge) => quote!(reactive::Graph<#node, #edge>),
        FieldShape::Tree(node) => quote!(reactive::Tree<#node>),
    }
}

fn single_field_forwarding(
    fields: &[FieldSpec],
    name: &Ident,
    name_str: &str,
    derived: bool,
) -> TokenStream {
    let _ = name;
    if fields.len() != 1 || fields[0].named {
        return quote!();
    }
    let field = &fields[0];
    let accessor = &field.ident;
    match &field.shape {
        FieldShape::Map(key, value) => quote! {
            /// Reads one entry.
            #[must_use]
            pub fn get(&self, key: &#key) -> Option<#value> {
                self.#accessor().get(key)
            }
            /// Reports key presence.
            #[must_use]
            pub fn contains_key(&self, key: &#key) -> bool {
                self.#accessor().contains_key(key)
            }
            /// Number of entries.
            #[must_use]
            pub fn len(&self) -> usize {
                self.#accessor().len()
            }
            /// Returns `true` when no entry is defined.
            #[must_use]
            pub fn is_empty(&self) -> bool {
                self.len() == 0
            }
            /// Keys in order.
            #[must_use]
            pub fn keys(&self) -> Vec<#key> {
                self.#accessor().keys()
            }
            /// Defines one entry.
            pub fn define(&self, key: #key, value: #value) {
                reactive::ensure_view(<Self as reactive::ViewSchema>::DESCRIPTOR);
                self.#accessor().define(key, value, #name_str);
            }
            /// Defines every entry of an iterator.
            pub fn from<I>(items: I) -> Self
            where
                I: IntoIterator<Item = (#key, #value)>,
            {
                let handle = Self;
                for (key, value) in items {
                    handle.define(key, value);
                }
                handle
            }
        },
        FieldShape::List(elem) => quote! {
            /// Reads every element.
            #[must_use]
            pub fn to_vec(&self) -> Vec<#elem> {
                self.#accessor().to_vec()
            }
            /// Number of elements.
            #[must_use]
            pub fn len(&self) -> usize {
                self.#accessor().len()
            }
            /// Returns `true` when empty.
            #[must_use]
            pub fn is_empty(&self) -> bool {
                self.#accessor().is_empty()
            }
            /// Reads one element.
            #[must_use]
            pub fn get(&self, index: usize) -> Option<#elem> {
                self.#accessor().get(index)
            }
            /// Defines the whole list.
            pub fn define_all<I>(&self, items: I)
            where
                I: IntoIterator<Item = #elem>,
            {
                reactive::ensure_view(<Self as reactive::ViewSchema>::DESCRIPTOR);
                self.#accessor().define_all(items, #name_str);
            }
            /// Defines the whole list.
            pub fn from<I>(items: I) -> Self
            where
                I: IntoIterator<Item = #elem>,
            {
                let handle = Self;
                handle.define_all(items);
                handle
            }
        },
        FieldShape::Set(elem) => quote! {
            /// Reads every member.
            #[must_use]
            pub fn to_vec(&self) -> Vec<#elem> {
                self.#accessor().to_vec()
            }
            /// Number of members.
            #[must_use]
            pub fn len(&self) -> usize {
                self.#accessor().len()
            }
            /// Returns `true` when empty.
            #[must_use]
            pub fn is_empty(&self) -> bool {
                self.len() == 0
            }
            /// Defines every member.
            pub fn define_all<I>(&self, members: I)
            where
                I: IntoIterator<Item = #elem>,
            {
                reactive::ensure_view(<Self as reactive::ViewSchema>::DESCRIPTOR);
                self.#accessor().define_all(members, #name_str);
            }
            /// Defines every member.
            pub fn from<I>(members: I) -> Self
            where
                I: IntoIterator<Item = #elem>,
            {
                let handle = Self;
                handle.define_all(members);
                handle
            }
        },
        FieldShape::Scalar(ty) => {
            if derived {
                quote! {
                    /// Reads the value.
                    #[must_use]
                    pub fn value(&self) -> #ty {
                        self.#accessor()
                    }
                    /// Defines the value.
                    pub fn define(&self, value: #ty) {
                        reactive::define_unit(
                            reactive::UnitKey::field(
                                <Self as reactive::ViewSchema>::DESCRIPTOR.id,
                                reactive::FieldId(0),
                                reactive::UnitKind::Value,
                            ),
                            value,
                            #name_str,
                        );
                    }
                }
            } else {
                quote!()
            }
        }
        FieldShape::Graph(_, _) | FieldShape::Tree(_) => quote!(),
    }
}
