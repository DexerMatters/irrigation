//! `#[view(source)]` and `#[view(derived)]` expansion.
//!
//! A View declaration generates:
//!
//! * a zero-sized handle whose accessors are tracked reads;
//! * an immutable content type and its vtable operations;
//! * a snapshot handle whose reads are pure;
//! * for source Views, a transaction editor;
//! * for derived Views, definition methods.

use std::collections::HashSet;

use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::{
    Data, DeriveInput, Expr, ExprLit, Field, Fields, GenericArgument, Ident, Lit, PathArguments,
    Result, Type, Visibility,
};

use crate::attr::{ViewKindArg, is_lint_policy};

/// The shape of one View field.
#[derive(Clone)]
enum FieldShape {
    Scalar(Type),
    Map(Type, Type),
    List(Type),
    Set(Type),
    Reduced(Type),
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
    let crate::attr::ViewArgs { kind, hide } = crate::attr::parse_view_args(&attr)?;
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
        if let FieldShape::Tree(_) = &field.shape {
            return Err(syn::Error::new_spanned(
                &field.ty,
                "tree fields are declared with `#[tree_view]`; \
                 a `#[view]` struct cannot own an enum-group tree",
            ));
        }
    }

    // The declared struct is replaced by generated items, so its attributes
    // follow them. `cfg` and lint policy gate every item as one unit; the rest
    // describes the handle users name. Derives and layout hints describe the
    // input fields, which the handle does not have. A deprecation is dropped:
    // the generated impls name the handle, so forwarding it would warn from
    // code the user did not write.
    let mut gate_attrs = Vec::new();
    let mut handle_attrs = Vec::new();
    for attr in &item.attrs {
        let path = attr.path();
        if path.is_ident("cfg") || path.is_ident("cfg_attr") || is_lint_policy(attr) {
            gate_attrs.push(attr);
        } else if path.is_ident("doc")
            || path.is_ident("must_use")
            || path.is_ident("non_exhaustive")
        {
            handle_attrs.push(attr);
        }
    }
    let gate = quote!(#(#gate_attrs)*);

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
    let define_many_fn = format_ident!("__{}_define_many", prefix);
    let retract_fn = format_ident!("__{}_retract", prefix);
    let contains_fn = format_ident!("__{}_contains", prefix);
    let get_fn = format_ident!("__{}_get", prefix);
    let storage_fn = format_ident!("__{}_storage", prefix);
    let structure_fn = format_ident!("__{}_structure", prefix);
    let len_fn = format_ident!("__{}_len", prefix);
    let moves_fn = format_ident!("__{}_moves_content", prefix);
    let changed_eq_fn = format_ident!("__{}_changed_units_equal", prefix);
    let summary_fn = format_ident!("__{}_summary", prefix);
    let eq_fn = format_ident!("__{}_eq", prefix);
    let describe_fn = format_ident!("__{}_describe", prefix);

    // A View spells its own values unless it declined: one downcast per
    // declared element type, so a projection can draw it with no host renderer.
    // Each entry pairs the type with how a found value becomes a label, because
    // a graph edge is the one value the content stores inside a cell's `Option`.
    let mut described: Vec<(Type, TokenStream)> = Vec::new();
    for field in &fields {
        match &field.shape {
            FieldShape::Scalar(ty) | FieldShape::List(ty) | FieldShape::Set(ty) => {
                described.push((ty.clone(), quote!(format!("{found:?}"))));
            }
            FieldShape::Map(key, value) => {
                described.push((key.clone(), quote!(format!("{found:?}"))));
                described.push((value.clone(), quote!(format!("{found:?}"))));
            }
            FieldShape::Graph(key, value) => {
                described.push((key.clone(), quote!(format!("{found:?}"))));
                described.push((value.clone(), quote!(format!("{found:?}"))));
                // An edge cell holds `Option<EdgePayload<E>>`, whose outer
                // none is absence; the label is the record inside it, not the
                // wrapper.
                described.push((
                    syn::parse_quote!(Option<irrigation::EdgePayload<#value>>),
                    quote!(
                        found
                            .as_ref()
                            .map(|edge| format!("{edge:?}"))
                            .unwrap_or_default()
                    ),
                ));
            }
            FieldShape::Reduced(reduction) => {
                described.push((
                    syn::parse_quote!(<#reduction as irrigation::Reduction>::Item),
                    quote!(format!("{found:?}")),
                ));
                described.push((
                    syn::parse_quote!(<#reduction as irrigation::Reduction>::Aggregate),
                    quote!(format!("{found:?}")),
                ));
            }
            // Tree values are the framework's own; the tree drawing reads them.
            FieldShape::Tree(_) => {}
        }
    }
    let describe_arms: Vec<TokenStream> = if hide {
        Vec::new()
    } else {
        described
            .iter()
            .map(|(ty, spell)| {
                quote! {
                    if let Some(found) = value.downcast_ref::<#ty>() {
                        return Some(#spell);
                    }
                }
            })
            .collect()
    };
    let describe_entry = if hide {
        quote!(|_| None)
    } else {
        quote!(#describe_fn)
    };

    let markers: Vec<TokenStream> = fields
        .iter()
        .map(|field| marker_for(&field.shape, derived))
        .collect();
    let field_idents: Vec<&Ident> = fields.iter().map(|field| &field.ident).collect();
    let field_names: Vec<String> = fields.iter().map(|field| field.ident.to_string()).collect();
    let field_vis: Vec<&Visibility> = fields.iter().map(|field| &field.vis).collect();
    let _indices: Vec<u16> = fields.iter().map(|field| field.index).collect();

    // The generated field constants and the hand-written unit keys agree only
    // if every public constant has a unique name. `foo` and `FOO` collide after
    // upper-casing; report that at the declaration instead of emitting a
    // duplicate definition inside a generated impl.
    let mut constant_names = HashSet::new();
    for field in &fields {
        let constant = field.ident.to_string().to_uppercase();
        if !constant_names.insert(constant.clone()) {
            return Err(syn::Error::new_spanned(
                &field.ident,
                format!("field constant `{constant}` collides with another field"),
            ));
        }
    }

    // ----------------------------------------------------------- field handles
    let mut field_content_fns = Vec::new();
    let mut field_consts = Vec::new();
    for field in &fields {
        if matches!(field.shape, FieldShape::Tree(_)) {
            continue;
        }
        let ident = &field.ident;
        let index = field.index;
        let marker = marker_for(&field.shape, derived);
        let alias = field_alias(&field.shape, derived);
        let constructor = field_constructor(&field.shape, derived);
        let const_ident = format_ident!("{}", ident.to_string().to_uppercase());
        let content_fn = format_ident!("__{}_field_{}", prefix, ident);
        let field_visibility = &field.vis;
        field_content_fns.push(quote! {
            #gate
            #[doc(hidden)]
            fn #content_fn(
                content: &dyn ::std::any::Any,
            ) -> <#marker as irrigation::ViewField>::Storage {
                content
                    .downcast_ref::<#content_ident>()
                    .expect(#name_str)
                    .#ident
                    .clone()
            }
        });
        field_consts.push(quote! {
            #[doc = concat!("Typed handle for the `", stringify!(#ident), "` field.")]
            #field_visibility const #const_ident: #alias =
                irrigation::Field::new(
                    <Self as irrigation::ViewSchema>::DESCRIPTOR,
                    irrigation::FieldId(#index),
                    #constructor(#view_const, irrigation::FieldId(#index), #content_fn),
                );
        });
    }

    // ---------------------------------------------------------------- content
    let content = quote! {
        #gate
        #[derive(Clone, PartialEq)]
        #vis struct #content_ident {
            #( #field_vis #field_idents: <#markers as irrigation::ViewField>::Storage, )*
        }

        #gate
        impl #content_ident {
            #vis fn empty() -> Self {
                Self {
                    #( #field_idents: <#markers as irrigation::ViewField>::empty(), )*
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
            // A membership definition states presence, never a payload, and
            // this is where the unit kind is still known: `false` takes the
            // entity out for every family, exactly as retraction does. `true`
            // installs an entity only in a set, whose content spells a member
            // as the entity itself; every other family spells the entity
            // through its payload draft, so a bare presence statement changes
            // no content. Routing here keeps a `Map<K, bool>` value definition
            // unambiguous and stops a presence draft overwriting a payload a
            // family stores in the same shape (a graph's `bool` node).
            let present = match field.shape {
                FieldShape::Set(_) => quote! {
                    else {
                        next.#ident =
                            <#marker as irrigation::ViewField>::define(&next.#ident, entity, value);
                    }
                },
                _ => quote! {},
            };
            quote! {
                #index => {
                    let entity = key.entity.as_deref().unwrap_or(&());
                    match key.kind {
                        irrigation::UnitKind::Membership => {
                            if value.downcast_ref::<bool>() == Some(&false) {
                                next.#ident = <#marker as irrigation::ViewField>::retract(
                                    &next.#ident,
                                    entity,
                                );
                            }
                            #present
                        }
                        _ => {
                            next.#ident = <#marker as irrigation::ViewField>::define(
                                &next.#ident,
                                entity,
                                value,
                            );
                        }
                    }
                },
            }
        })
        .collect();

    let mut batch_groups: Vec<TokenStream> = Vec::new();
    let mut batch_push_arms: Vec<TokenStream> = Vec::new();
    let mut batch_apply: Vec<TokenStream> = Vec::new();
    for field in &fields {
        let ident = &field.ident;
        let index = field.index;
        let marker = marker_for(&field.shape, derived);
        let group = format_ident!("__batch_{}", ident);
        batch_groups.push(quote! {
            let mut #group: ::std::vec::Vec<(irrigation::UnitKey, irrigation::Value)> =
                ::std::vec::Vec::new();
        });
        batch_push_arms.push(quote! {
            #index => #group.push((key.clone(), value.clone())),
        });
        batch_apply.push(quote! {
            next.#ident =
                <#marker as irrigation::ViewField>::define_many(&next.#ident, &#group);
        });
    }

    let retract_arms: Vec<TokenStream> = fields
        .iter()
        .map(|field| {
            let ident = &field.ident;
            let index = field.index;
            let marker = marker_for(&field.shape, derived);
            quote! {
                #index => {
                    match key.entity.as_deref() {
                        Some(entity) => {
                            next.#ident =
                                <#marker as irrigation::ViewField>::retract(&next.#ident, entity);
                        }
                        // Only a whole-field value definition owns the field's
                        // storage. The other entity-less units are the field's
                        // protocol: a structure projection, the keyed/whole
                        // certificate, an index, the content. Retracting one
                        // leaves the entities the field holds — installation
                        // of the same unit writes none of them either.
                        None if key.kind == irrigation::UnitKind::Value => {
                            next.#ident = <#marker as irrigation::ViewField>::empty();
                        }
                        None => {}
                    }
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
                        <#marker as irrigation::ViewField>::contains(&next.#ident, entity)
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
                    #index => Some(irrigation::value_of(next.#ident.clone())),
                },
                _ => quote! {
                    #index => match key.kind {
                        // A field-level structure unit is the family's
                        // projection of the content, the payload every writer
                        // installs. The whole field storage is `field_storage`:
                        // it is the value a whole-field definition installs,
                        // not the projection of the same field.
                        irrigation::UnitKind::Structure if key.entity.is_none() => {
                            <#marker as irrigation::ViewField>::structure(&next.#ident)
                                .map(|structure| structure.value)
                        }
                        // This family has no entity-scoped structure slot; the
                        // tree vtable spells those.
                        irrigation::UnitKind::Structure => None,
                        irrigation::UnitKind::Membership => key.entity.as_deref().map(|entity| {
                            irrigation::value_of(<#marker as irrigation::ViewField>::contains(&next.#ident, entity))
                        }),
                        irrigation::UnitKind::Value => key.entity.as_deref().and_then(|entity| {
                            <#marker as irrigation::ViewField>::get(&next.#ident, entity)
                        }),
                        _ => None,
                    },
                },
            }
        })
        .collect();

    let storage_arms: Vec<TokenStream> = fields
        .iter()
        .map(|field| {
            let ident = &field.ident;
            let index = field.index;
            quote! {
                #index => Some(irrigation::value_of(next.#ident.clone())),
            }
        })
        .collect();

    let structure_arms: Vec<TokenStream> = fields
        .iter()
        .map(|field| {
            let ident = &field.ident;
            let index = field.index;
            let marker = marker_for(&field.shape, derived);
            quote! {
                #index => <#marker as irrigation::ViewField>::structure(&next.#ident),
            }
        })
        .collect();

    let len_arms: Vec<TokenStream> = fields
        .iter()
        .map(|field| {
            let ident = &field.ident;
            let index = field.index;
            let marker = marker_for(&field.shape, derived);
            quote! {
                #index => <#marker as irrigation::ViewField>::len(&next.#ident),
            }
        })
        .collect();

    let moves_arms: Vec<TokenStream> = fields
        .iter()
        .map(|field| {
            let index = field.index;
            let marker = marker_for(&field.shape, derived);
            quote! {
                #index => <#marker as irrigation::ViewField>::unit_moves_content(unit),
            }
        })
        .collect();

    let changed_eq_arms: Vec<TokenStream> = fields
        .iter()
        .map(|field| {
            let ident = &field.ident;
            let index = field.index;
            let marker = marker_for(&field.shape, derived);
            quote! {
                #index => {
                    match <#marker as irrigation::ViewField>::unit_eq(
                        &old.#ident,
                        &new.#ident,
                        unit,
                    ) {
                        Some(value) => equal &= value,
                        None => return None,
                    }
                }
            }
        })
        .collect();

    let summary_arms: Vec<TokenStream> = fields
        .iter()
        .map(|field| {
            let ident = &field.ident;
            let index = field.index;
            let marker = marker_for(&field.shape, derived);
            quote! {
                #index => <#marker as irrigation::ViewField>::summary(&next.#ident),
            }
        })
        .collect();

    let vtable = quote! {
        #gate
        fn #empty_fn() -> ::std::sync::Arc<dyn ::std::any::Any + Send + Sync> {
            ::std::sync::Arc::new(#content_ident::empty())
        }

        #gate
        fn #define_fn(
            content: &::std::sync::Arc<dyn ::std::any::Any + Send + Sync>,
            key: &irrigation::UnitKey,
            value: irrigation::Value,
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

        #gate
        fn #define_many_fn(
            content: &::std::sync::Arc<dyn ::std::any::Any + Send + Sync>,
            drafts: &[(irrigation::UnitKey, irrigation::Value)],
        ) -> ::std::sync::Arc<dyn ::std::any::Any + Send + Sync> {
            let mut next = content
                .downcast_ref::<#content_ident>()
                .cloned()
                .unwrap_or_else(#content_ident::empty);
            #( #batch_groups )*
            for (key, value) in drafts {
                match key.field.0 {
                    #( #batch_push_arms )*
                    _ => {}
                }
            }
            #( #batch_apply )*
            ::std::sync::Arc::new(next)
        }

        #gate
        fn #retract_fn(
            content: &::std::sync::Arc<dyn ::std::any::Any + Send + Sync>,
            key: &irrigation::UnitKey,
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

        #gate
        fn #contains_fn(
            content: &::std::sync::Arc<dyn ::std::any::Any + Send + Sync>,
            key: &irrigation::UnitKey,
        ) -> bool {
            let Some(next) = content.downcast_ref::<#content_ident>() else {
                return false;
            };
            match key.field.0 {
                #( #contains_arms )*
                _ => false,
            }
        }

        #gate
        fn #get_fn(
            content: &::std::sync::Arc<dyn ::std::any::Any + Send + Sync>,
            key: &irrigation::UnitKey,
        ) -> Option<irrigation::Value> {
            let next = content.downcast_ref::<#content_ident>()?;
            match key.field.0 {
                #( #get_arms )*
                _ => None,
            }
        }

        #gate
        fn #storage_fn(
            content: &::std::sync::Arc<dyn ::std::any::Any + Send + Sync>,
            field: irrigation::FieldId,
        ) -> Option<irrigation::Value> {
            let next = content.downcast_ref::<#content_ident>()?;
            match field.0 {
                #( #storage_arms )*
                _ => None,
            }
        }

        #gate
        fn #structure_fn(
            content: &::std::sync::Arc<dyn ::std::any::Any + Send + Sync>,
            field: irrigation::FieldId,
        ) -> Option<irrigation::Structure> {
            let next = content.downcast_ref::<#content_ident>()?;
            match field.0 {
                #( #structure_arms )*
                _ => None,
            }
        }

        #gate
        fn #len_fn(
            content: &::std::sync::Arc<dyn ::std::any::Any + Send + Sync>,
            field: irrigation::FieldId,
        ) -> Option<usize> {
            let next = content.downcast_ref::<#content_ident>()?;
            match field.0 {
                #( #len_arms )*
                _ => None,
            }
        }

        #gate
        fn #moves_fn(unit: &irrigation::UnitKey) -> bool {
            match unit.field.0 {
                #( #moves_arms )*
                _ => true,
            }
        }

        #gate
        fn #changed_eq_fn(
            old: &::std::sync::Arc<dyn ::std::any::Any + Send + Sync>,
            new: &::std::sync::Arc<dyn ::std::any::Any + Send + Sync>,
            units: &[irrigation::UnitKey],
        ) -> Option<bool> {
            let old = old.downcast_ref::<#content_ident>()?;
            let new = new.downcast_ref::<#content_ident>()?;
            if units.is_empty() {
                return None;
            }
            let mut equal = true;
            for unit in units {
                match unit.field.0 {
                    #( #changed_eq_arms )*
                    _ => return None,
                }
            }
            Some(equal)
        }

        #gate
        fn #summary_fn(
            content: &::std::sync::Arc<dyn ::std::any::Any + Send + Sync>,
            field: irrigation::FieldId,
        ) -> Option<irrigation::Value> {
            let next = content.downcast_ref::<#content_ident>()?;
            match field.0 {
                #( #summary_arms )*
                _ => None,
            }
        }

        #gate
        fn #eq_fn(
            left: &::std::sync::Arc<dyn ::std::any::Any + Send + Sync>,
            right: &::std::sync::Arc<dyn ::std::any::Any + Send + Sync>,
        ) -> bool {
            left.downcast_ref::<#content_ident>() == right.downcast_ref::<#content_ident>()
        }

        #gate
        fn #describe_fn(
            value: &::std::sync::Arc<dyn ::std::any::Any + Send + Sync>,
        ) -> Option<String> {
            #( #describe_arms )*
            None
        }

        #gate
        const #view_const: irrigation::ViewId = irrigation::ViewId(
            irrigation::const_hash(concat!(module_path!(), "::", #name_str)),
        );

        #gate
        static #descriptor_ident: irrigation::ViewDescriptor = irrigation::ViewDescriptor {
            id: #view_const,
            name: concat!(module_path!(), "::", #name_str),
            kind: irrigation::ViewKind::#kind_ident,
            fields: &[
                #( irrigation::FieldDescriptor {
                    name: #field_names,
                    kind: <#markers as irrigation::ViewField>::KIND,
                }, )*
            ],
            content: irrigation::ContentVtable {
                empty: #empty_fn,
                define: #define_fn,
                define_many: #define_many_fn,
                retract: #retract_fn,
                contains: #contains_fn,
                get: #get_fn,
                field_storage: #storage_fn,
                structure: #structure_fn,
                len: #len_fn,
                moves_content: #moves_fn,
                changed_units_equal: #changed_eq_fn,
                summary: #summary_fn,
                eq: #eq_fn,
                describe: #describe_entry,
                variant_label: |_, _| None,
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
                        <#marker as irrigation::ViewFieldHandle>::handle(
                            #view_const,
                            irrigation::FieldId(#index),
                            |content| content
                                .downcast_ref::<#content_ident>()
                                .expect(#name_str)
                                .#ident
                                .clone(),
                        ).read()
                    }
                },
                _ => quote! {
                    #vis fn #ident(&self) -> <#marker as irrigation::ViewFieldHandle>::Handle {
                        <#marker as irrigation::ViewFieldHandle>::handle(
                            #view_const,
                            irrigation::FieldId(#index),
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
                                self.content.#ident.to_vec()
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
                FieldShape::Set(elem) => {
                    if derived {
                        quote! {
                            #vis fn #ident(&self) -> Vec<#elem> {
                                self.content.#ident.to_vec()
                            }
                        }
                    } else {
                        quote! {
                            #vis fn #ident(&self) -> Vec<#elem> {
                                self.content.#ident.iter().cloned().collect()
                            }
                        }
                    }
                }
                FieldShape::Reduced(reduction) => {
                    let aggregate = format_ident!("{}_aggregate", ident);
                    quote! {
                        #vis fn #ident(&self) -> Vec<<#reduction as irrigation::Reduction>::Item> {
                            self.content.#ident.to_vec()
                        }
                        #vis fn #aggregate(&self) -> <#reduction as irrigation::Reduction>::Aggregate {
                            self.content.#ident.aggregate()
                        }
                    }
                }
                FieldShape::Graph(node_ty, edge_ty) => {
                    let nodes = format_ident!("{}_nodes", ident);
                    let edges = format_ident!("{}_edges", ident);
                    let node = format_ident!("{}_node", ident);
                    let edge = format_ident!("{}_edge", ident);
                    quote! {
                        /// Node identities in order.
                        #vis fn #nodes(&self) -> Vec<u64> {
                            self.content.#ident.nodes().map(|(id, _)| id).collect()
                        }
                        /// Edge identities in order.
                        #vis fn #edges(&self) -> Vec<u64> {
                            self.content.#ident.edges().map(|(id, _)| id).collect()
                        }
                        /// Reads one node payload from this snapshot.
                        #vis fn #node(&self, id: u64) -> Option<#node_ty> {
                            self.content.#ident.node(id).cloned()
                        }
                        /// Reads one edge, endpoints included, from this snapshot.
                        #vis fn #edge(&self, id: u64) -> Option<irrigation::EdgePayload<#edge_ty>> {
                            self.content.#ident.edge(id).map(|edge| irrigation::EdgePayload {
                                from: edge.from,
                                to: edge.to,
                                value: edge.value.clone(),
                            })
                        }
                    }
                },
                FieldShape::Tree(_) => quote! {},
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
                                irrigation::UnitKey::field(
                                    #view_const,
                                    irrigation::FieldId(#index),
                                    irrigation::UnitKind::Value,
                                ),
                                self.content.#ident.clone(),
                            );
                        }
                    },
                    _ => quote! {
                        #vis fn #ident(&mut self) -> <#marker as irrigation::ViewFieldHandle>::Editor<'_> {
                            <#marker as irrigation::ViewFieldHandle>::editor(
                                &mut *self.edit,
                                #view_const,
                                irrigation::FieldId(#index),
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
                        #[track_caller]
                        #vis fn #define_name(&self, value: #ty) {
                            irrigation::ensure_view(<#name as irrigation::ViewSchema>::DESCRIPTOR);
                            irrigation::define_unit(
                                irrigation::UnitKey::field(
                                    #view_const,
                                    irrigation::FieldId(#index),
                                    irrigation::UnitKind::Value,
                                ),
                                value,
                                #name_str,
                            );
                        }
                    },
                    FieldShape::List(elem) => quote! {
                        #[track_caller]
                        #vis fn #define_name<I>(&self, items: I)
                        where I: IntoIterator<Item = #elem> {
                            irrigation::ensure_view(<#name as irrigation::ViewSchema>::DESCRIPTOR);
                            self.#ident().define_all(items, #name_str);
                        }
                    },
                    FieldShape::Set(elem) => quote! {
                        #[track_caller]
                        #vis fn #define_name<I>(&self, members: I)
                        where I: IntoIterator<Item = #elem> {
                            irrigation::ensure_view(<#name as irrigation::ViewSchema>::DESCRIPTOR);
                            self.#ident().define_all(members, #name_str);
                        }
                    },
                    FieldShape::Map(key, value) => quote! {
                        #[track_caller]
                        #vis fn #define_name(&self, key: #key, value: #value) {
                            irrigation::ensure_view(<#name as irrigation::ViewSchema>::DESCRIPTOR);
                            self.#ident().define(key, value, #name_str);
                        }
                    },
                    FieldShape::Reduced(reduction) => quote! {
                        #[track_caller]
                        #vis fn #define_name<I>(&self, items: I)
                        where I: IntoIterator<Item = <#reduction as irrigation::Reduction>::Item> {
                            irrigation::ensure_view(<#name as irrigation::ViewSchema>::DESCRIPTOR);
                            self.#ident().define_all(items, #name_str);
                        }
                    },
                    FieldShape::Graph(node_ty, edge_ty) => quote! {
                        #[track_caller]
                            #vis fn #define_name(
                            &self,
                            nodes: &[(u64, #node_ty)],
                            edges: &[(u64, u64, u64, #edge_ty)],
                        ) {
                            irrigation::ensure_view(<#name as irrigation::ViewSchema>::DESCRIPTOR);
                            self.#ident().define_graph(nodes, edges, #name_str);
                        }
                    },
                    FieldShape::Tree(_) => quote! {},
                }
            })
            .collect()
    } else {
        Vec::new()
    };

    // ------------------------------------------------ single-field forwarding
    let forwarding = single_field_forwarding(&fields, &name, &name_str, derived);

    // A one-field View may be read through `value()` without remembering the
    // generated accessor's name (`field0` for a tuple declaration). The alias
    // is omitted when the field is itself called `value`, and the live and
    // snapshot forms differ exactly as the underlying accessors do.
    let (value_live, value_snapshot) = if fields.len() == 1 && fields[0].ident != "value" {
        let ident = &fields[0].ident;
        let vis = &fields[0].vis;
        let marker = marker_for(&fields[0].shape, derived);
        match &fields[0].shape {
            FieldShape::Scalar(ty) => {
                if derived {
                    // `single_field_forwarding` already defines `value` for a
                    // derived scalar View; a second definition would be a
                    // duplicate inherent method.
                    (Vec::new(), Vec::new())
                } else {
                    (
                        vec![quote! {
                            /// The single declared field.
                            #vis fn value(&self) -> #ty { self.#ident() }
                        }],
                        vec![quote! {
                            /// The single declared field.
                            #vis fn value(&self) -> #ty { self.#ident() }
                        }],
                    )
                }
            }
            FieldShape::Map(key, value) => (
                vec![quote! {
                    /// The single declared field.
                    #vis fn value(&self) -> <#marker as irrigation::ViewFieldHandle>::Handle {
                        self.#ident()
                    }
                }],
                vec![quote! {
                    /// The single declared field.
                    #vis fn value(&self) -> Vec<(#key, #value)> { self.#ident() }
                }],
            ),
            FieldShape::List(elem) | FieldShape::Set(elem) => (
                vec![quote! {
                    /// The single declared field.
                    #vis fn value(&self) -> <#marker as irrigation::ViewFieldHandle>::Handle {
                        self.#ident()
                    }
                }],
                vec![quote! {
                    /// The single declared field.
                    #vis fn value(&self) -> Vec<#elem> { self.#ident() }
                }],
            ),
            FieldShape::Reduced(reduction) => (
                vec![quote! {
                    /// The single declared field.
                    #vis fn value(&self) -> <#marker as irrigation::ViewFieldHandle>::Handle {
                        self.#ident()
                    }
                }],
                vec![quote! {
                    /// The single declared field.
                    #vis fn value(&self) -> Vec<<#reduction as irrigation::Reduction>::Item> {
                        self.#ident()
                    }
                }],
            ),
            FieldShape::Graph(_, _) | FieldShape::Tree(_) => (Vec::new(), Vec::new()),
        }
    } else {
        (Vec::new(), Vec::new())
    };

    let schema_impl = quote! {
        #gate
        impl irrigation::ViewSchema for #name {
            const DESCRIPTOR: &'static irrigation::ViewDescriptor = &#descriptor_ident;
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

        #gate
        #vis struct #snapshot_ident<'a> {
            content: &'a #content_ident,
        }

        #gate
        impl #snapshot_ident<'_> {
            #( #snapshot_accessors )*
            #( #value_snapshot )*
        }
    };

    let source_impl = if derived {
        quote! {
            #gate
            impl irrigation::DerivedView for #name {}

            #gate
            impl #name {
                #( #define_accessors )*
                #forwarding
            }
        }
    } else {
        quote! {
            #gate
            impl irrigation::SourceView for #name {}

            #gate
            impl irrigation::EditableView for #name {
                type Editor<'a> = #editor_ident<'a>;

                fn edit<'a>(edit: &'a mut irrigation::Edit) -> Self::Editor<'a> {
                    edit.ensure_view(<#name as irrigation::ViewSchema>::DESCRIPTOR);
                    let content = (*edit.staged_content::<#content_ident>(#view_const)).clone();
                    #editor_ident { edit, content }
                }
            }

            #gate
            #vis struct #editor_ident<'a> {
                edit: &'a mut irrigation::Edit,
                content: #content_ident,
            }

            #gate
            impl #editor_ident<'_> {
                #( #edit_accessors )*
            }

            #gate
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
        #(#handle_attrs)*
        #gate
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
        #vis struct #name;

        #content
        #( #field_content_fns )*
        #vtable
        #schema_impl
        #source_impl

        #gate
        impl #name {
            #( #live_accessors )*
            #( #value_live )*
            #( #field_consts )*
        }
    })
}

fn parse_field(field: &Field, index: usize, named: bool) -> Result<FieldSpec> {
    // A field attribute reaches this macro unexpanded: the compiler strips a
    // `cfg` on the item the attribute applies to, never on a field inside a
    // struct it hands to a macro. The generated content, descriptor and
    // snapshot share one field shape, so no per-field gate can be represented;
    // a dropped gate would either let a gated-out type break the build or add
    // a field the source compiled out.
    //
    // `#[field(name = "...")]` is the one supported attribute: a tuple field
    // has no name of its own, and `field0` is a poor public accessor. It is
    // rejected on a named field, whose name already exists.
    let mut renamed: Option<String> = None;
    for attr in &field.attrs {
        let path = attr.path();
        if path.is_ident("doc") {
            continue;
        }
        if path.is_ident("field") {
            if named {
                return Err(syn::Error::new_spanned(
                    attr,
                    "`#[field(name = ...)]` is only meaningful on a tuple field",
                ));
            }
            let parser = Punctuated::<syn::MetaNameValue, syn::Token![,]>::parse_terminated;
            for meta in parser.parse2(attr.meta.require_list()?.tokens.clone())? {
                if !meta.path.is_ident("name") {
                    return Err(syn::Error::new_spanned(
                        meta.path,
                        "expected `name = \"...\"`",
                    ));
                }
                let Expr::Lit(ExprLit {
                    lit: Lit::Str(literal),
                    ..
                }) = meta.value
                else {
                    return Err(syn::Error::new_spanned(
                        meta.value,
                        "expected a string literal",
                    ));
                };
                if renamed.is_some() {
                    return Err(syn::Error::new_spanned(attr, "duplicate `name`"));
                }
                renamed = Some(literal.value());
            }
            continue;
        }
        let attribute = if path.is_ident("cfg") {
            "`#[cfg]`"
        } else if path.is_ident("cfg_attr") {
            "`#[cfg_attr]`"
        } else {
            "an attribute"
        };
        return Err(syn::Error::new_spanned(
            field,
            format!(
                "{attribute} on a View field is not supported: the generated content, \
                 descriptor and snapshot share one field shape. Gate the whole View item \
                 instead"
            ),
        ));
    }
    let ident = match (&field.ident, renamed) {
        (Some(ident), None) => ident.clone(),
        (None, Some(name)) => syn::parse_str::<Ident>(&name)
            .map_err(|_| syn::Error::new_spanned(field, "`name` must be an identifier"))?,
        (None, None) => format_ident!("field{}", index, span = Span::call_site()),
        (Some(_), Some(_)) => unreachable!("named fields reject the attribute above"),
    };
    let shape = shape_of(&field.ty);
    Ok(FieldSpec {
        ident,
        ty: field.ty.clone(),
        shape,
        index: index as u16,
        named,
        vis: field.vis.clone(),
    })
}

fn shape_of(ty: &Type) -> FieldShape {
    let Type::Path(path) = ty else {
        return FieldShape::Scalar(ty.clone());
    };
    let Some(segment) = path.path.segments.last() else {
        return FieldShape::Scalar(ty.clone());
    };
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return FieldShape::Scalar(ty.clone());
    };
    let types: Vec<Type> = args
        .args
        .iter()
        .filter_map(|arg| match arg {
            GenericArgument::Type(ty) => Some(ty.clone()),
            _ => None,
        })
        .collect();
    match (segment.ident.to_string().as_str(), types.len()) {
        ("Map", 2) => FieldShape::Map(types[0].clone(), types[1].clone()),
        ("List", 1) => FieldShape::List(types[0].clone()),
        ("Set", 1) => FieldShape::Set(types[0].clone()),
        ("Reduced", 1) => FieldShape::Reduced(types[0].clone()),
        ("Graph", 2) => FieldShape::Graph(types[0].clone(), types[1].clone()),
        ("Tree", 1) => FieldShape::Tree(types[0].clone()),
        _ => FieldShape::Scalar(ty.clone()),
    }
}

fn marker_for(shape: &FieldShape, derived: bool) -> TokenStream {
    match shape {
        FieldShape::Scalar(ty) => quote!(irrigation::Scalar<#ty>),
        FieldShape::Map(key, value) => quote!(irrigation::Map<#key, #value>),
        FieldShape::List(elem) => {
            if derived {
                quote!(irrigation::DerivedList<#elem>)
            } else {
                quote!(irrigation::SourceList<#elem>)
            }
        }
        FieldShape::Set(elem) => {
            if derived {
                quote!(irrigation::DerivedSet<#elem>)
            } else {
                quote!(irrigation::SourceSet<#elem>)
            }
        }
        FieldShape::Reduced(reduction) => quote!(irrigation::Reduced<#reduction>),
        FieldShape::Graph(node, edge) => quote!(irrigation::Graph<#node, #edge>),
        FieldShape::Tree(node) => quote!(irrigation::Tree<#node>),
    }
}

fn field_alias(shape: &FieldShape, derived: bool) -> TokenStream {
    match shape {
        FieldShape::Scalar(ty) => quote!(irrigation::ScalarField<#ty>),
        FieldShape::Map(key, value) => quote!(irrigation::MapField<#key, #value>),
        FieldShape::List(elem) => {
            if derived {
                quote!(irrigation::DerivedListField<#elem>)
            } else {
                quote!(irrigation::SourceListField<#elem>)
            }
        }
        FieldShape::Set(elem) => {
            if derived {
                quote!(irrigation::DerivedSetField<#elem>)
            } else {
                quote!(irrigation::SourceSetField<#elem>)
            }
        }
        FieldShape::Reduced(reduction) => quote!(irrigation::ReducedField<#reduction>),
        FieldShape::Graph(node, edge) => quote!(irrigation::GraphField<#node, #edge>),
        FieldShape::Tree(node) => quote!(irrigation::TreeField<#node>),
    }
}

fn field_constructor(shape: &FieldShape, derived: bool) -> TokenStream {
    match shape {
        FieldShape::Scalar(_) => quote!(irrigation::ScalarRef::new),
        FieldShape::Map(_, _) => quote!(irrigation::MapRef::new),
        FieldShape::List(_) => {
            if derived {
                quote!(irrigation::DerivedListRef::new)
            } else {
                quote!(irrigation::SourceListRef::new)
            }
        }
        FieldShape::Set(_) => {
            if derived {
                quote!(irrigation::DerivedSetRef::new)
            } else {
                quote!(irrigation::SourceSetRef::new)
            }
        }
        FieldShape::Reduced(_) => quote!(irrigation::ReducedRef::new),
        FieldShape::Graph(_, _) => quote!(irrigation::GraphRef::new),
        FieldShape::Tree(_) => quote!(irrigation::TreeRef::new),
    }
}

fn single_field_forwarding(
    fields: &[FieldSpec],
    name: &Ident,
    name_str: &str,
    derived: bool,
) -> TokenStream {
    let _ = name;
    if fields.len() != 1 {
        return quote!();
    }
    let field = &fields[0];
    let accessor = &field.ident;
    // A named field is already an accessor with that exact name. Forwarding a
    // helper with the same name would define an inherent method twice. Tuple
    // fields are named `fieldN`, which cannot collide with a helper; named
    // fields skip the whole forwarding set when one name overlaps.
    if field.named {
        let accessor_name = accessor.to_string();
        let generated: &[&str] = match &field.shape {
            FieldShape::Scalar(_) => &["value"],
            FieldShape::Map(_, _) => &[
                "get",
                "contains_key",
                "len",
                "is_empty",
                "keys",
                "define",
                "from",
            ],
            FieldShape::List(_) => &[
                "to_vec",
                "ids",
                "map",
                "filter",
                "flat_map",
                "len",
                "is_empty",
                "get",
                "define_all",
                "from",
                "define_keyed",
                "get_key",
            ],
            FieldShape::Set(_) => &[
                "to_vec",
                "members",
                "contains",
                "len",
                "is_empty",
                "define_all",
                "from",
                "define_keyed",
            ],
            FieldShape::Reduced(_) => &[
                "aggregate",
                "len",
                "is_empty",
                "to_vec",
                "define_all",
                "from",
            ],
            FieldShape::Graph(_, _) => &[
                "nodes",
                "edges",
                "node_count",
                "edge_count",
                "contains_node",
                "node",
                "edge",
                "outgoing",
                "neighbors",
                "define_graph",
                "from",
            ],
            FieldShape::Tree(_) => &[],
        };
        let define_name = format!("define_{accessor_name}");
        if generated.contains(&accessor_name.as_str()) || generated.contains(&define_name.as_str())
        {
            return quote!();
        }
    }
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
            #[track_caller]
            pub fn define(&self, key: #key, value: #value) {
                irrigation::ensure_view(<Self as irrigation::ViewSchema>::DESCRIPTOR);
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
        FieldShape::List(elem) => {
            let keyed = if derived {
                quote! {
                    /// Defines a keyed list: stable identities plus one value per entity.
                    pub fn define_keyed<I>(&self, items: I)
                    where
                        I: IntoIterator<Item = (u64, #elem)>,
                    {
                        irrigation::ensure_view(<Self as irrigation::ViewSchema>::DESCRIPTOR);
                        self.#accessor().define_keyed(items, #name_str);
                    }
                    /// Reads one element by identity.
                    #[must_use]
                    pub fn get_key(&self, id: u64) -> Option<#elem> {
                        self.#accessor().get_key(id)
                    }
                }
            } else {
                quote! {}
            };
            quote! {
            /// Reads every element.
            #[must_use]
            pub fn to_vec(&self) -> Vec<#elem> {
                self.#accessor().to_vec()
            }
            /// Element identity sequence.
            #[must_use]
            pub fn ids(&self) -> irrigation::PSeq<u64> {
                self.#accessor().ids()
            }
            /// Maps every element, memoizing per identity.
            #[must_use]
            pub fn map<U, F>(&self, f: F) -> Vec<U>
            where
                U: Clone + PartialEq + Send + Sync + 'static,
                F: Fn(irrigation::Param<#elem>) -> U + Send + Sync,
            {
                self.#accessor().map(f)
            }
            /// Keeps elements whose predicate holds, memoizing per identity.
            #[must_use]
            pub fn filter<F>(&self, predicate: F) -> Vec<#elem>
            where
                F: Fn(irrigation::Param<#elem>) -> bool,
            {
                self.#accessor().filter(predicate)
            }
            /// Maps each element to zero or more values, memoizing per identity.
            #[must_use]
            pub fn flat_map<U, F>(&self, f: F) -> Vec<U>
            where
                U: Clone + PartialEq + Send + Sync + 'static,
                F: Fn(irrigation::Param<#elem>) -> Vec<U>,
            {
                self.#accessor().flat_map(f)
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
                irrigation::ensure_view(<Self as irrigation::ViewSchema>::DESCRIPTOR);
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
            #keyed
            }
        }
        FieldShape::Set(elem) => quote! {
            /// Reads every member.
            #[must_use]
            pub fn to_vec(&self) -> Vec<#elem> {
                self.#accessor().to_vec()
            }
            /// The structure projection (the whole member set).
            #[must_use]
            pub fn members(&self) -> irrigation::PSet<#elem> {
                self.#accessor().members()
            }
            /// Whether a member is present.
            #[must_use]
            pub fn contains(&self, member: &#elem) -> bool {
                self.#accessor().contains(member)
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
                irrigation::ensure_view(<Self as irrigation::ViewSchema>::DESCRIPTOR);
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
            /// Defines a keyed set: one membership unit per member.
            pub fn define_keyed<I>(&self, members: I)
            where
                I: IntoIterator<Item = #elem>,
            {
                irrigation::ensure_view(<Self as irrigation::ViewSchema>::DESCRIPTOR);
                self.#accessor().define_keyed(members, #name_str);
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
                    #[track_caller]
                    pub fn define(&self, value: #ty) {
                        irrigation::ensure_view(<Self as irrigation::ViewSchema>::DESCRIPTOR);
                        irrigation::define_unit(
                            irrigation::UnitKey::field(
                                <Self as irrigation::ViewSchema>::DESCRIPTOR.id,
                                irrigation::FieldId(0),
                                irrigation::UnitKind::Value,
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
        FieldShape::Reduced(reduction) => quote! {
            /// The cached aggregate.
            #[must_use]
            pub fn aggregate(&self) -> <#reduction as irrigation::Reduction>::Aggregate {
                self.#accessor().aggregate()
            }
            /// Number of elements.
            #[must_use]
            pub fn len(&self) -> usize {
                self.#accessor().len()
            }
            /// Returns `true` when empty.
            #[must_use]
            pub fn is_empty(&self) -> bool {
                self.len() == 0
            }
            /// Reads every element.
            #[must_use]
            pub fn to_vec(&self) -> Vec<<#reduction as irrigation::Reduction>::Item> {
                self.#accessor().to_vec()
            }
            /// Defines the whole sequence.
            pub fn define_all<I>(&self, items: I)
            where
                I: IntoIterator<Item = <#reduction as irrigation::Reduction>::Item>,
            {
                irrigation::ensure_view(<Self as irrigation::ViewSchema>::DESCRIPTOR);
                self.#accessor().define_all(items, #name_str);
            }
            /// Defines the whole sequence.
            pub fn from<I>(items: I) -> Self
            where
                I: IntoIterator<Item = <#reduction as irrigation::Reduction>::Item>,
            {
                let handle = Self;
                handle.define_all(items);
                handle
            }
        },
        FieldShape::Graph(node_ty, edge_ty) => quote! {
            /// Node identities in order.
            #[must_use]
            pub fn nodes(&self) -> Vec<u64> {
                self.#accessor().nodes()
            }
            /// Edge identities in order.
            #[must_use]
            pub fn edges(&self) -> Vec<u64> {
                self.#accessor().edges()
            }
            /// Number of nodes.
            #[must_use]
            pub fn node_count(&self) -> usize {
                self.#accessor().node_count()
            }
            /// Number of edges.
            #[must_use]
            pub fn edge_count(&self) -> usize {
                self.#accessor().edge_count()
            }
            /// Reports node presence.
            #[must_use]
            pub fn contains_node(&self, node: u64) -> bool {
                self.#accessor().contains_node(node)
            }
            /// Reads one node payload.
            #[must_use]
            pub fn node(&self, node: u64) -> Option<#node_ty> {
                self.#accessor().node(node)
            }
            /// Reads one edge with its endpoints.
            #[must_use]
            pub fn edge(&self, edge: u64) -> Option<irrigation::EdgePayload<#edge_ty>> {
                self.#accessor().edge(edge)
            }
            /// Reads the outgoing adjacency of one node.
            #[must_use]
            pub fn outgoing(&self, node: u64) -> Vec<(u64, u64)> {
                self.#accessor().outgoing(node)
            }
            /// Reads the neighbours of one node.
            #[must_use]
            pub fn neighbors(&self, node: u64) -> Vec<u64> {
                self.#accessor().neighbors(node)
            }
            /// Defines a whole graph: node payloads, edge payloads, topology.
            pub fn define_graph(
                &self,
                nodes: &[(u64, #node_ty)],
                edges: &[(u64, u64, u64, #edge_ty)],
            ) {
                irrigation::ensure_view(<Self as irrigation::ViewSchema>::DESCRIPTOR);
                self.#accessor().define_graph(nodes, edges, #name_str);
            }
            /// Defines a whole graph.
            pub fn from(
                nodes: &[(u64, #node_ty)],
                edges: &[(u64, u64, u64, #edge_ty)],
            ) -> Self {
                let handle = Self;
                handle.define_graph(nodes, edges);
                handle
            }
        },
        FieldShape::Tree(_) => quote!(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use quote::quote;

    #[test]
    fn attributes_gate_every_generated_item() {
        let tokens = expand(
            quote!(source),
            quote!(
                /// Docs for the view.
                #[cfg(feature = "demo")]
                struct Numbers {
                    values: u64,
                }
            ),
        )
        .to_string();
        assert!(
            tokens.contains("Docs for the view."),
            "the docs must reach the handle: {tokens}"
        );
        // The intent is "every generated item carries the gate", stated as such:
        // a literal count breaks whenever an item is added or removed for an
        // unrelated reason, which says nothing about gating.
        let gate = "cfg (feature = \"demo\")";
        let mut checked = 0;
        for (index, _) in tokens.match_indices("fn __") {
            let window = &tokens[index.saturating_sub(160)..index];
            assert!(
                window.contains(gate),
                "a generated item is not gated: {window}"
            );
            checked += 1;
        }
        assert!(
            checked >= 10,
            "the expansion generated almost nothing: {tokens}"
        );
    }

    #[test]
    fn a_cfg_gated_field_is_rejected_at_the_field() {
        let tokens = expand(
            quote!(source),
            quote!(
                struct Numbers {
                    /// Shown.
                    shown: u64,
                    #[cfg(any())]
                    hidden: MissingType,
                }
            ),
        )
        .to_string();
        assert!(
            tokens.contains("compile_error"),
            "a gated field must not reach the generated content: {tokens}"
        );
        assert!(
            tokens.contains("Gate the whole View item"),
            "the diagnostic must name the supported alternative: {tokens}"
        );
    }

    #[test]
    fn derives_are_not_forwarded_to_the_handle() {
        let tokens = expand(
            quote!(source),
            quote!(
                #[derive(Default)]
                struct Numbers {
                    values: u64,
                }
            ),
        )
        .to_string();
        assert!(
            !tokens.contains("derive (Default)"),
            "an input derive must not be replayed on the handle: {tokens}"
        );
    }
}
