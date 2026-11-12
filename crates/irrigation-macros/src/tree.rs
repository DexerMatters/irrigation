//! `#[tree_view]` expansion.
//!
//! The tree group macro must see every mutually recursive type at once, which
//! is why it is a module-level attribute rather than a derive. It generates:
//!
//! * typed node handles (`DocumentRef`, `ExprRef`, ...);
//! * discriminant enums (`ExprKind`) that read only the tag unit;
//! * per-variant field accessors that read only the requested projection;
//! * conversions from the owned construction values into build descriptions;
//! * a View and transaction editor with identity-preserving moves and replaces.

use std::collections::{HashMap, HashSet};

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::spanned::Spanned;
use syn::{Fields, GenericArgument, Ident, Item, ItemMod, PathArguments, Result, Type};

/// A field of one enum variant.
#[derive(Clone)]
enum FieldShape {
    Leaf(Box<Type>),
    Child(String),
    OptionalChild(String),
    VecChild(String),
}

#[derive(Clone)]
struct FieldSpec {
    ident: Ident,
    method: Ident,
    shape: FieldShape,
    leaf_slot: u16,
    child_slot: u16,
}

struct VariantSpec {
    ident: Ident,
    fields: Vec<FieldSpec>,
}

struct EnumSpec {
    ident: Ident,
    type_index: u16,
    variants: Vec<VariantSpec>,
}

/// Expands a tree View group.
pub fn expand(args: TokenStream, input: TokenStream) -> TokenStream {
    match expand_inner(args, input) {
        Ok(tokens) => tokens,
        Err(error) => error.to_compile_error(),
    }
}

fn expand_inner(args: TokenStream, input: TokenStream) -> Result<TokenStream> {
    let parsed: TreeArgs = syn::parse2(args)?;
    let kind = parsed.kind.as_ref().map(ToString::to_string);
    let derived = kind.as_deref() == Some("derived");
    let view_kind = match kind.as_deref() {
        Some("source") | None => quote!(irrigation::ViewKind::Source),
        Some("derived") => quote!(irrigation::ViewKind::Derived),
        Some(other) => {
            return Err(syn::Error::new(
                parsed.kind.as_ref().expect("checked").span(),
                format!("unknown tree view kind `{other}`"),
            ));
        }
    };
    let module: ItemMod = syn::parse2(input)?;
    let Some((_, items)) = module.content.clone() else {
        return Err(syn::Error::new_spanned(
            &module,
            "`#[tree_view]` requires an inline module",
        ));
    };

    let mut enum_items = Vec::new();
    let mut other_items = Vec::new();
    for item in items {
        match item {
            Item::Enum(item) => enum_items.push(item),
            other => other_items.push(other),
        }
    }
    if enum_items.is_empty() {
        return Err(syn::Error::new_spanned(
            &module,
            "`#[tree_view]` requires at least one enum",
        ));
    }

    let names: HashSet<String> = enum_items
        .iter()
        .map(|item| item.ident.to_string())
        .collect();
    let root_name = parsed.root.to_string();
    if !names.contains(&root_name) {
        return Err(syn::Error::new_spanned(
            &parsed.root,
            "the root type must be declared in the tree module",
        ));
    }

    let mut specs = Vec::new();
    let mut type_index = 0u16;
    // The root type is index 0 so a root node's tag is stable.
    let mut ordered: Vec<&ItemEnum> = Vec::new();
    for item in &enum_items {
        if item.ident == parsed.root {
            ordered.push(item);
        }
    }
    for item in &enum_items {
        if item.ident != parsed.root {
            ordered.push(item);
        }
    }
    for item in ordered {
        let spec = enum_spec(item, &names, type_index)?;
        type_index += 1;
        specs.push(spec);
    }

    // A generic drawing has no renderer to name a node, so the descriptor
    // carries the declared names: `Rule` instead of `variant 3`.
    let mut label_arms = Vec::new();
    for spec in &specs {
        let type_index = spec.type_index;
        for (variant_index, variant) in spec.variants.iter().enumerate() {
            let name = variant.ident.to_string();
            let type_index = u32::from(type_index);
            let variant_index = variant_index as u16;
            label_arms.push(quote! {
                (#type_index, #variant_index) => Some(#name),
            });
            let _ = type_index;
        }
    }
    let label_fn = format_ident!("__{}_variant_label", parsed.view.to_string().to_lowercase());

    let view_ident = parsed.view;
    let view_name = view_ident.to_string();
    let prefix = view_name.to_lowercase();
    let view_const = format_ident!("__{}_view_id", prefix);
    let descriptor = format_ident!("__{}_descriptor", prefix);
    let content_fn = format_ident!("__{}_content", prefix);
    let editor_ident = format_ident!("{}Editor", view_ident);
    let snapshot_ident = format_ident!("{}Snapshot", view_ident);

    let enum_defs: Vec<TokenStream> = enum_items.iter().map(|item| quote!(#item)).collect();

    // ------------------------------------------------------------- kind enums
    let mut kind_defs = Vec::new();
    let mut ref_defs = Vec::new();
    let mut ref_impls = Vec::new();
    let mut build_fns = Vec::new();
    let mut editor_methods = Vec::new();

    for spec in &specs {
        let name = &spec.ident;
        let kind_ident = format_ident!("{}Kind", name);
        let ref_ident = format_ident!("{}Ref", name);
        let build_ident = format_ident!("build_{}", snake_case(&name.to_string()));
        let type_index = spec.type_index;
        let variant_idents: Vec<&Ident> = spec.variants.iter().map(|v| &v.ident).collect();
        let from_index_arms: Vec<TokenStream> = spec
            .variants
            .iter()
            .enumerate()
            .map(|(index, variant)| {
                let ident = &variant.ident;
                let index = index as u16;
                quote!(#index => Some(Self::#ident),)
            })
            .collect();

        kind_defs.push(quote! {
            /// Discriminant of one declared enum type.
            #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
            pub enum #kind_ident {
                #( #variant_idents, )*
            }

            impl #kind_ident {
                fn from_index(index: u16) -> Option<Self> {
                    match index {
                        #( #from_index_arms )*
                        _ => None,
                    }
                }
            }
        });

        ref_defs.push(quote! {
            /// Typed handle to one node of this enum type.
            ///
            /// A reference, not an owned child: it can be used as a key in other
            /// Views and never participates in this tree's ownership.
            #[derive(Clone, PartialEq, Eq, Hash, Debug)]
            pub struct #ref_ident {
                inner: irrigation::TreeRef,
            }

            impl PartialOrd for #ref_ident {
                fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
                    Some(self.cmp(other))
                }
            }

            impl Ord for #ref_ident {
                fn cmp(&self, other: &Self) -> std::cmp::Ordering {
                    (self.inner.view(), self.inner.id())
                        .cmp(&(other.inner.view(), other.inner.id()))
                }
            }

            impl #ref_ident {
                /// Wraps an untyped tree ref.
                #[must_use]
                pub fn from_tree(inner: irrigation::TreeRef) -> Self {
                    Self { inner }
                }

                /// The node identity.
                #[must_use]
                pub fn raw(&self) -> irrigation::TreeNodeId {
                    self.inner.id()
                }

                /// The underlying ref.
                #[must_use]
                pub fn tree(&self) -> &irrigation::TreeRef {
                    &self.inner
                }

                /// Reads the discriminant, observing only the tag unit.
                ///
                /// A node of another declared type answers `None`: the tag's
                /// type half is part of the discriminant, not a free slot.
                #[must_use]
                pub fn kind(&self) -> Option<#kind_ident> {
                    match self.inner.tag()? {
                        (#type_index, index) => #kind_ident::from_index(index),
                        _ => None,
                    }
                }
            }
        });

        let mut accessors = Vec::new();
        let mut projections: Vec<(Ident, Vec<Projection>)> = Vec::new();
        let mut predicates = Vec::new();
        let mut build_arms = Vec::new();
        for (variant_index, variant) in spec.variants.iter().enumerate() {
            let variant_ident = &variant.ident;
            let predicate = format_ident!("is_{}", snake_case(&variant_ident.to_string()));
            predicates.push(quote! {
                /// Returns `true` when this node is this variant.
                #[must_use]
                pub fn #predicate(&self) -> bool {
                    self.kind() == Some(#kind_ident::#variant_ident)
                }
            });
            let mut patterns = Vec::new();
            let mut field_exprs = Vec::new();
            let mut children = Vec::new();
            for field in &variant.fields {
                let field_ident = &field.ident;
                match &field.shape {
                    FieldShape::Leaf(ty) => {
                        patterns.push(quote!(#field_ident));
                        field_exprs.push(leaf_build(ty, &quote!(#field_ident))?);
                    }
                    FieldShape::Child(enum_name) => {
                        patterns.push(quote!(#field_ident));
                        let build = build_fn_for(enum_name);
                        children.push(quote! {
                            irrigation::BuildSlot::One(Box::new(#build(#field_ident)))
                        });
                    }
                    FieldShape::OptionalChild(enum_name) => {
                        patterns.push(quote!(#field_ident));
                        let build = build_fn_for(enum_name);
                        children.push(quote! {
                            irrigation::BuildSlot::Optional(#field_ident.as_ref().map(|inner| Box::new(#build(inner))))
                        });
                    }
                    FieldShape::VecChild(enum_name) => {
                        patterns.push(quote!(#field_ident));
                        let build = build_fn_for(enum_name);
                        children.push(quote! {
                            irrigation::BuildSlot::Many(#field_ident.iter().map(|inner| #build(inner)).collect())
                        });
                    }
                }
            }
            // Tuple variants use positional patterns; named variants use braces.
            let is_tuple = matches!(variant.fields.first().map(|f| f.ident.to_string()), Some(name) if name.starts_with("__tuple"));
            let pattern = if patterns.is_empty() {
                quote!()
            } else if is_tuple {
                quote!((#(#patterns),*))
            } else {
                quote!({ #(#patterns),* })
            };
            build_arms.push(quote! {
                #name::#variant_ident #pattern => irrigation::TreeBuild {
                    id: None,
                    type_index: #type_index,
                    variant: #variant_index as u16,
                    fields: vec![#(#field_exprs),*],
                    children: vec![#(#children),*],
                },
            });

            for field in &variant.fields {
                push_projection(&mut projections, field, variant_ident);
            }
        }

        // One accessor per field name per enum. Two variants may name the same
        // field when they mean the same thing -- `body` in every binder, `text`
        // in every literal -- and the accessor dispatches on the discriminant.
        // A name is reused only when the type and the read are identical, so a
        // `body: Expr` and a `body: String` stay two questions and two errors.
        for (method, specs) in &projections {
            let shape = specs[0].shape.clone();
            let accessor = match shape {
                FieldShape::Leaf(ty) => {
                    let extract = leaf_extract(&ty)?;
                    let arms = specs.iter().map(|spec| {
                        let variant = &spec.kind_variant;
                        let slot = spec.leaf_slot;
                        quote! {
                            #kind_ident::#variant => {
                                self.inner.field(#slot).and_then(|value| #extract)
                            }
                        }
                    });
                    quote! {
                        /// Reads this projection, observing only its unit.
                        #[must_use]
                        pub fn #method(&self) -> Option<#ty> {
                            match self.kind()? {
                                #( #arms, )*
                                _ => None,
                            }
                        }
                    }
                }
                FieldShape::Child(enum_name) => {
                    let child_ref = format_ident!("{}Ref", enum_name);
                    let arms = specs.iter().map(|spec| {
                        let variant = &spec.kind_variant;
                        let slot = spec.child_slot;
                        quote! {
                            #kind_ident::#variant => {
                                match self.inner.child_slot(#slot) {
                                    irrigation::ChildSlot::One(id) => {
                                        Some(#child_ref::from_tree(self.inner.child(id)))
                                    }
                                    _ => None,
                                }
                            }
                        }
                    });
                    quote! {
                        /// Reads this child projection.
                        #[must_use]
                        pub fn #method(&self) -> Option<#child_ref> {
                            match self.kind()? {
                                #( #arms, )*
                                _ => None,
                            }
                        }
                    }
                }
                FieldShape::OptionalChild(enum_name) => {
                    let child_ref = format_ident!("{}Ref", enum_name);
                    let arms = specs.iter().map(|spec| {
                        let variant = &spec.kind_variant;
                        let slot = spec.child_slot;
                        quote! {
                            #kind_ident::#variant => {
                                match self.inner.child_slot(#slot) {
                                    irrigation::ChildSlot::Optional(Some(id)) => {
                                        Some(#child_ref::from_tree(self.inner.child(id)))
                                    }
                                    _ => None,
                                }
                            }
                        }
                    });
                    quote! {
                        /// Reads this optional child projection.
                        #[must_use]
                        pub fn #method(&self) -> Option<#child_ref> {
                            match self.kind()? {
                                #( #arms, )*
                                _ => None,
                            }
                        }
                    }
                }
                FieldShape::VecChild(enum_name) => {
                    let child_ref = format_ident!("{}Ref", enum_name);
                    let arms = specs.iter().map(|spec| {
                        let variant = &spec.kind_variant;
                        let slot = spec.child_slot;
                        quote! {
                            #kind_ident::#variant => self
                                .inner
                                .child_slot(#slot)
                                .ids()
                                .into_iter()
                                .map(|id| #child_ref::from_tree(self.inner.child(id)))
                                .collect(),
                        }
                    });
                    quote! {
                        /// Reads this child list projection.
                        #[must_use]
                        pub fn #method(&self) -> Vec<#child_ref> {
                            let Some(kind) = self.kind() else {
                                return Vec::new();
                            };
                            match kind {
                                #( #arms )*
                                _ => Vec::new(),
                            }
                        }
                    }
                }
            };
            accessors.push(accessor);
        }

        ref_impls.push(quote! {
            impl #ref_ident {
                #( #predicates )*
                #( #accessors )*
            }
        });

        build_fns.push(quote! {
            /// Converts an owned construction value into a build description.
            #[must_use]
            pub fn #build_ident(value: &#name) -> irrigation::TreeBuild {
                match value {
                    #( #build_arms )*
                }
            }
        });

        // Editor helpers for this type.
        let replace_name = format_ident!("replace_{}", snake_case(&name.to_string()));
        let remove_name = format_ident!("remove_{}", snake_case(&name.to_string()));
        let _ = type_index;
        editor_methods.push(quote! {
            /// Replaces one node's payload and children, preserving its identity.
            pub fn #replace_name(&mut self, node: #ref_ident, value: &#name) -> #ref_ident {
                let id = {
                    let storage = self.storage.as_mut().expect("editor storage");
                    let mut tree =
                        irrigation::TreeEditor::new(&mut *self.edit, #view_const, storage);
                    tree.replace(node.raw(), &#build_ident(value))
                };
                let storage = self.storage.as_ref().expect("editor storage");
                #ref_ident::from_tree(irrigation::TreeRef::snapshot(
                    ::std::sync::Arc::new(storage.clone()),
                    id,
                ))
            }

            /// Removes a subtree rooted at this node.
            pub fn #remove_name(&mut self, node: #ref_ident) {
                let storage = self.storage.as_mut().expect("editor storage");
                let mut tree = irrigation::TreeEditor::new(&mut *self.edit, #view_const, storage);
                tree.remove_subtree(node.raw());
            }
        });
    }

    // ------------------------------------------------------------- the View
    let root_spec = &specs[0];
    let root_ident = root_spec.ident.clone();
    let root_ref = format_ident!("{}Ref", root_spec.ident);
    let root_build = format_ident!("build_{}", snake_case(&root_spec.ident.to_string()));

    // ---------------------------------------------------------------- layout
    //
    // The declaration as data, for code that builds or walks the tree without
    // naming its accessors. It is generated from the same specs the accessors
    // are, so the two cannot disagree; a test pins that.
    let index_by_name: HashMap<String, u16> = specs
        .iter()
        .map(|spec| (spec.ident.to_string(), spec.type_index))
        .collect();
    let mut type_layouts = Vec::new();
    for spec in &specs {
        let type_name = spec.ident.to_string();
        let mut variant_layouts = Vec::new();
        for variant in &spec.variants {
            let variant_name = variant.ident.to_string();
            let mut leaves = Vec::new();
            let mut children = Vec::new();
            for field in &variant.fields {
                let name = field.method.to_string();
                let leaf_slot = field.leaf_slot;
                let child_slot = field.child_slot;
                match &field.shape {
                    FieldShape::Leaf(ty) => {
                        let kind = leaf_kind(ty)?;
                        leaves.push(quote! {
                            irrigation::LeafLayout { name: #name, slot: #leaf_slot, kind: #kind }
                        });
                    }
                    FieldShape::Child(target) => {
                        let target = layout_target(&index_by_name, target, &variant.ident)?;
                        children.push(quote! {
                            irrigation::ChildLayout {
                                name: #name,
                                slot: #child_slot,
                                cardinality: irrigation::Cardinality::One,
                                target: #target,
                            }
                        });
                    }
                    FieldShape::OptionalChild(target) => {
                        let target = layout_target(&index_by_name, target, &variant.ident)?;
                        children.push(quote! {
                            irrigation::ChildLayout {
                                name: #name,
                                slot: #child_slot,
                                cardinality: irrigation::Cardinality::Optional,
                                target: #target,
                            }
                        });
                    }
                    FieldShape::VecChild(target) => {
                        let target = layout_target(&index_by_name, target, &variant.ident)?;
                        children.push(quote! {
                            irrigation::ChildLayout {
                                name: #name,
                                slot: #child_slot,
                                cardinality: irrigation::Cardinality::Many,
                                target: #target,
                            }
                        });
                    }
                }
            }
            variant_layouts.push(quote! {
                irrigation::VariantLayout {
                    name: #variant_name,
                    leaves: &[#(#leaves),*],
                    children: &[#(#children),*],
                }
            });
        }
        type_layouts.push(quote! {
            irrigation::TypeLayout { name: #type_name, variants: &[#(#variant_layouts),*] }
        });
    }
    let layout_impl = quote! {
        impl irrigation::TreeSchema for #view_ident {
            const LAYOUT: &'static irrigation::TreeLayout = &irrigation::TreeLayout {
                types: &[#(#type_layouts),*],
                root: 0,
            };
        }
    };

    // ---------------------------------------------------- cross-type traversal
    let node_ident = format_ident!("{}Node", view_ident);
    let mut node_variants: Vec<TokenStream> = Vec::new();
    let mut id_arms: Vec<TokenStream> = Vec::new();
    let mut child_arms: Vec<TokenStream> = Vec::new();
    let mut flatten_impls: Vec<TokenStream> = Vec::new();
    for spec in &specs {
        let name = &spec.ident;
        let ref_ident = format_ident!("{}Ref", name);
        node_variants.push(quote!(#name(#ref_ident)));
        id_arms.push(quote!(#node_ident::#name(node) => node.raw(),));
        let mut pushes = Vec::new();
        for variant in &spec.variants {
            for field in &variant.fields {
                let method = &field.method;
                match &field.shape {
                    FieldShape::Child(enum_name) | FieldShape::OptionalChild(enum_name) => {
                        let child = Ident::new(enum_name, name.span());
                        pushes.push(quote! {
                            if let Some(child) = node.#method() {
                                out.push(#node_ident::#child(child));
                            }
                        });
                    }
                    FieldShape::VecChild(enum_name) => {
                        let child = Ident::new(enum_name, name.span());
                        pushes.push(quote! {
                            out.extend(node.#method().into_iter().map(#node_ident::#child));
                        });
                    }
                    FieldShape::Leaf(_) => {}
                }
            }
        }
        child_arms.push(quote! {
            #node_ident::#name(node) => {
                let mut out = Vec::new();
                #(#pushes)*
                out
            }
        });
        flatten_impls.push(quote! {
            impl #ref_ident {
                /// Defines one target entry per node in this subtree, using `f`
                /// to choose the value (`None` skips the node).
                pub fn flatten_map<V, F>(&self, target: &irrigation::MapRef<u64, V>, mut f: F)
                where
                    V: Clone + PartialEq + Send + Sync + 'static,
                    F: FnMut(&#node_ident) -> Option<V>,
                {
                    let mut stack = vec![#node_ident::#name(self.clone())];
                    while let Some(node) = stack.pop() {
                        if let Some(value) = f(&node) {
                            target.define(node.id(), value, "tree_flatten_map");
                        }
                        stack.extend(node.children());
                    }
                }
            }
        });
    }
    let node_def = quote! {
        /// One node of the tree group, for cross-family flattening.
        #[derive(Clone, Debug)]
        pub enum #node_ident {
            #( #node_variants, )*
        }

        impl #node_ident {
            /// The node identity.
            #[must_use]
            pub fn id(&self) -> u64 {
                match self {
                    #( #id_arms )*
                }
            }

            fn children(&self) -> Vec<Self> {
                match self {
                    #( #child_arms )*
                }
            }
        }
    };

    let content_impl = quote! {
        fn #content_fn(content: &dyn ::std::any::Any) -> irrigation::TreeStorage {
            content
                .downcast_ref::<irrigation::TreeStorage>()
                .expect(#view_name)
                .clone()
        }

        const #view_const: irrigation::ViewId = irrigation::ViewId(
            irrigation::const_hash(concat!(module_path!(), "::", #view_name)),
        );

        static #descriptor: irrigation::ViewDescriptor = irrigation::ViewDescriptor {
            id: #view_const,
            name: concat!(module_path!(), "::", #view_name),
            kind: #view_kind,
            fields: irrigation::tree_fields(),
            content: irrigation::ContentVtable {
                variant_label: #label_fn,
                ..irrigation::tree_content_vtable()
            },
        };
    };

    let label_def = quote! {
        fn #label_fn(type_index: u32, variant: u16) -> Option<&'static str> {
            match (type_index, variant) {
                #( #label_arms )*
                _ => None,
            }
        }
    };

    let view_marker = if derived {
        quote!(impl irrigation::DerivedView for #view_ident {})
    } else {
        quote!(impl irrigation::SourceView for #view_ident {})
    };
    let view_def = quote! {
        /// The tree View handle.
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
        pub struct #view_ident;

        impl irrigation::ViewSchema for #view_ident {
            const DESCRIPTOR: &'static irrigation::ViewDescriptor = &#descriptor;
            type Content = irrigation::TreeStorage;
            type Snapshot<'a> = #snapshot_ident;

            fn snapshot<'a>(
                content: &'a ::std::sync::Arc<dyn ::std::any::Any + Send + Sync>,
            ) -> Self::Snapshot<'a> {
                #snapshot_ident {
                    storage: ::std::sync::Arc::new(
                        content
                            .downcast_ref::<irrigation::TreeStorage>()
                            .expect(#view_name)
                            .clone(),
                    ),
                }
            }
        }

        // A derived tree's content comes from its producer, so the marker says so;
        // `SourceView` would claim the host populates it.
        #view_marker

        impl #view_ident {
            /// Reads the root identity, observing only the root unit.
            #[must_use]
            pub fn root(&self) -> Option<#root_ref> {
                let node: Option<irrigation::TreeNodeId> = irrigation::try_read_unit_value(
                    irrigation::UnitKey::field(
                        #view_const,
                        irrigation::FieldId(irrigation::tree_slots::ROOT),
                        irrigation::UnitKind::Value,
                    ),
                    |content| Some(#content_fn(content).root),
                )
                .flatten();
                node.map(|id| {
                    #root_ref::from_tree(irrigation::TreeRef::live(
                        #view_const,
                        id,
                        #content_fn,
                    ))
                })
            }
        }

        /// Immutable snapshot of the tree.
        #[derive(Clone, Debug)]
        pub struct #snapshot_ident {
            storage: ::std::sync::Arc<irrigation::TreeStorage>,
        }

        impl #snapshot_ident {
            /// Reads the root identity.
            #[must_use]
            pub fn root(&self) -> Option<#root_ref> {
                self.storage.root.map(|id| {
                    #root_ref::from_tree(irrigation::TreeRef::snapshot(
                        ::std::sync::Arc::clone(&self.storage),
                        id,
                    ))
                })
            }

            /// Number of nodes in the snapshot.
            #[must_use]
            pub fn node_count(&self) -> usize {
                self.storage.len()
            }
        }
    };

    // A derived tree View's content is published by its producer. Removing
    // `impl EditableView` would be a public-surface break, so the generated
    // editor type stays and its only constructor rejects the call instead of
    // staging a whole-tree content replacement behind the producer's back.
    let editable_impl = if derived {
        quote! {
            impl irrigation::EditableView for #view_ident {
                type Editor<'a> = #editor_ident<'a>;

                fn edit<'a>(_edit: &'a mut irrigation::Edit) -> Self::Editor<'a> {
                    panic!(
                        "tree view `{}` is derived; its content is published by a producer",
                        #view_name
                    )
                }
            }
        }
    } else {
        quote! {
            impl irrigation::EditableView for #view_ident {
                type Editor<'a> = #editor_ident<'a>;

                fn edit<'a>(edit: &'a mut irrigation::Edit) -> Self::Editor<'a> {
                    edit.ensure_view(<#view_ident as irrigation::ViewSchema>::DESCRIPTOR);
                    let storage = (*edit.staged_content::<irrigation::TreeStorage>(#view_const)).clone();
                    #editor_ident {
                        edit,
                        storage: Some(storage),
                    }
                }
            }
        }
    };

    let editor_def = quote! {
        /// Transactional editor for the tree View.
        pub struct #editor_ident<'a> {
            edit: &'a mut irrigation::Edit,
            storage: Option<irrigation::TreeStorage>,
        }

        impl<'a> #editor_ident<'a> {
            /// Replaces the whole tree, returning its new root handle.
            pub fn set_root(&mut self, document: &#root_ident) -> #root_ref {
                let id = {
                    let storage = self.storage.as_mut().expect("editor storage");
                    let mut tree =
                        irrigation::TreeEditor::new(&mut *self.edit, #view_const, storage);
                    tree.set_root(&#root_build(document))
                };
                let storage = self.storage.as_ref().expect("editor storage");
                #root_ref::from_tree(irrigation::TreeRef::snapshot(
                    ::std::sync::Arc::new(storage.clone()),
                    id,
                ))
            }

            /// Imports a whole tree, returning its root and the import report.
            pub fn import_root(
                &mut self,
                document: &#root_ident,
            ) -> (#root_ref, irrigation::TreeImportReport) {
                let (id, report) = {
                    let storage = self.storage.as_mut().expect("editor storage");
                    let mut tree =
                        irrigation::TreeEditor::new(&mut *self.edit, #view_const, storage);
                    tree.set_root_report(&#root_build(document))
                };
                let storage = self.storage.as_ref().expect("editor storage");
                (
                    #root_ref::from_tree(irrigation::TreeRef::snapshot(
                        ::std::sync::Arc::new(storage.clone()),
                        id,
                    )),
                    report,
                )
            }

            /// Reconciles one subtree, returning its root and the report.
            pub fn reconcile_root(
                &mut self,
                node: #root_ref,
                document: &#root_ident,
            ) -> (#root_ref, irrigation::TreeImportReport) {
                let (id, report) = {
                    let storage = self.storage.as_mut().expect("editor storage");
                    let mut tree =
                        irrigation::TreeEditor::new(&mut *self.edit, #view_const, storage);
                    tree.replace_report(node.raw(), &#root_build(document))
                };
                let storage = self.storage.as_ref().expect("editor storage");
                (
                    #root_ref::from_tree(irrigation::TreeRef::snapshot(
                        ::std::sync::Arc::new(storage.clone()),
                        id,
                    )),
                    report,
                )
            }

            /// Replaces one node's payload and children, preserving its identity.
            pub fn replace_root(&mut self, node: #root_ref, document: &#root_ident) -> #root_ref {
                let id = {
                    let storage = self.storage.as_mut().expect("editor storage");
                    let mut tree =
                        irrigation::TreeEditor::new(&mut *self.edit, #view_const, storage);
                    tree.replace(node.raw(), &#root_build(document))
                };
                let storage = self.storage.as_ref().expect("editor storage");
                #root_ref::from_tree(irrigation::TreeRef::snapshot(
                    ::std::sync::Arc::new(storage.clone()),
                    id,
                ))
            }

            /// Removes a subtree.
            pub fn remove_subtree(&mut self, node: #root_ref) {
                let storage = self.storage.as_mut().expect("editor storage");
                let mut tree = irrigation::TreeEditor::new(&mut *self.edit, #view_const, storage);
                tree.remove_subtree(node.raw());
            }

            /// Removes a subtree, returning a structured error when it is absent.
            pub fn remove_subtree_checked(
                &mut self,
                node: #root_ref,
            ) -> Result<(), irrigation::ComputeError> {
                let storage = self.storage.as_mut().expect("editor storage");
                let mut tree = irrigation::TreeEditor::new(&mut *self.edit, #view_const, storage);
                tree.remove_subtree_checked(node.raw())
            }

            #( #editor_methods )*

            /// Reads the current root as a snapshot ref over the transaction's
            /// staged tree, so navigation does not require a computation frame.
            pub fn root(&self) -> #root_ref {
                let storage = self.storage.as_ref().expect("editor storage");
                let id = storage.root.expect("tree has no root");
                #root_ref::from_tree(irrigation::TreeRef::snapshot(
                    ::std::sync::Arc::new(storage.clone()),
                    id,
                ))
            }

            /// Moves a subtree under a new parent and child slot.
            pub fn move_subtree(
                &mut self,
                node: irrigation::TreeNodeId,
                new_parent: irrigation::TreeNodeId,
                slot: u16,
                index: usize,
            ) -> bool {
                let storage = self.storage.as_mut().expect("editor storage");
                let mut tree = irrigation::TreeEditor::new(&mut *self.edit, #view_const, storage);
                tree.move_subtree(node, new_parent, slot, index)
            }

            /// Moves a subtree, returning a structured error instead of `false`.
            pub fn move_subtree_checked(
                &mut self,
                node: irrigation::TreeNodeId,
                new_parent: irrigation::TreeNodeId,
                slot: u16,
                index: usize,
            ) -> Result<(), irrigation::ComputeError> {
                let storage = self.storage.as_mut().expect("editor storage");
                let mut tree = irrigation::TreeEditor::new(&mut *self.edit, #view_const, storage);
                tree.move_subtree_checked(node, new_parent, slot, index)
            }
        }

        #editable_impl

        impl Drop for #editor_ident<'_> {
            fn drop(&mut self) {
                if let Some(storage) = self.storage.take() {
                    self.edit.set_content(
                        #view_const,
                        ::std::sync::Arc::new(storage),
                        irrigation::tree_eq,
                    );
                }
            }
        }
    };

    let attrs = &module.attrs;
    let module_vis = &module.vis;
    let module_ident = &module.ident;
    Ok(quote! {
        #(#attrs)*
        #module_vis mod #module_ident {
            #( #enum_defs )*
            #( #other_items )*

            #content_impl
            #( #kind_defs )*
            #( #ref_defs )*
            #( #ref_impls )*
            #node_def
            #( #flatten_impls )*
            #( #build_fns )*
            #label_def
            #view_def
            #layout_impl
            #editor_def
        }
    })
}

struct TreeArgs {
    root: Ident,
    view: Ident,
    kind: Option<Ident>,
}

impl syn::parse::Parse for TreeArgs {
    fn parse(input: syn::parse::ParseStream) -> Result<Self> {
        let mut root = None;
        let mut view = None;
        let mut kind = None;
        while !input.is_empty() {
            let key: Ident = input.parse()?;
            input.parse::<syn::Token![=]>()?;
            let value: Ident = input.parse()?;
            match key.to_string().as_str() {
                "root" => root = Some(value),
                "view" => view = Some(value),
                "kind" => kind = Some(value),
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!("unknown `#[tree_view]` argument `{other}`"),
                    ));
                }
            }
            if input.peek(syn::Token![,]) {
                input.parse::<syn::Token![,]>()?;
            }
        }
        Ok(Self {
            root: root.ok_or_else(|| syn::Error::new(input.span(), "missing `root = ...`"))?,
            view: view.ok_or_else(|| syn::Error::new(input.span(), "missing `view = ...`"))?,
            kind,
        })
    }
}

type ItemEnum = syn::ItemEnum;

fn enum_spec(item: &ItemEnum, names: &HashSet<String>, type_index: u16) -> Result<EnumSpec> {
    // Every variant's field accessors are pushed into one inherent impl, so a
    // name may be claimed once per enum. The tuple-variant renaming below is
    // the one deliberate disambiguation; anything else is reported here.
    let mut accessors = HashMap::new();
    let tuple_methods: HashSet<String> = item
        .variants
        .iter()
        .filter(|variant| {
            matches!(variant.fields, Fields::Unnamed(_)) && variant.fields.iter().count() == 1
        })
        .map(|variant| snake_case(&variant.ident.to_string()))
        .collect();
    let mut variants = Vec::new();
    for variant in &item.variants {
        let mut fields = Vec::new();
        match &variant.fields {
            Fields::Unit => {}
            Fields::Named(named) => {
                for field in &named.named {
                    let ident = field.ident.clone().expect("named field");
                    let field_name = ident.to_string();
                    let method = if tuple_methods.contains(&field_name) {
                        format_ident!("{}_{}", snake_case(&variant.ident.to_string()), field_name)
                    } else {
                        ident.clone()
                    };
                    let shape = classify(&field.ty, names);
                    claim_accessor(&mut accessors, &method, &variant.ident, field, &shape)?;
                    fields.push(FieldSpec {
                        ident,
                        method,
                        shape,
                        // Assigned by the one pass below, which owns the rule.
                        leaf_slot: 0,
                        child_slot: 0,
                    });
                }
            }
            Fields::Unnamed(unnamed) => {
                for (index, field) in unnamed.unnamed.iter().enumerate() {
                    let ident = format_ident!("__tuple{}", index);
                    let count = unnamed.unnamed.len();
                    let method = if count == 1 {
                        format_ident!("{}", snake_case(&variant.ident.to_string()))
                    } else {
                        format_ident!("field{}", index)
                    };
                    let shape = classify(&field.ty, names);
                    claim_accessor(&mut accessors, &method, &variant.ident, field, &shape)?;
                    fields.push(FieldSpec {
                        ident,
                        method,
                        shape,
                        // Assigned by the one pass below, which owns the rule.
                        leaf_slot: 0,
                        child_slot: 0,
                    });
                }
            }
        }
        // The single owner of a field's slot: a leaf counts leaves, anything else
        // counts children, and the counter it does not use stays zero. Computing
        // them here rather than while pushing was never needed for determinism --
        // a field's slot is a function of the declaration order before it -- and
        // having both meant two copies of one rule.
        let mut leaf = 0u16;
        let mut child = 0u16;
        for field in &mut fields {
            match field.shape {
                FieldShape::Leaf(_) => {
                    field.leaf_slot = leaf;
                    field.child_slot = 0;
                    leaf += 1;
                }
                _ => {
                    field.leaf_slot = 0;
                    field.child_slot = child;
                    child += 1;
                }
            }
        }
        variants.push(VariantSpec {
            ident: variant.ident.clone(),
            fields,
        });
    }
    Ok(EnumSpec {
        ident: item.ident.clone(),
        type_index,
        variants,
    })
}

/// Records one projection for one enum, merging a compatible repeat.
///
/// Every variant's accessors share one inherent impl, so two fields may carry
/// one accessor name only when they ask the same question: `body` in two binders
/// is one accessor that dispatches on the discriminant. A different type under
/// the same name is a real ambiguity and is reported at the source field.
fn claim_accessor(
    accessors: &mut HashMap<String, (String, FieldShape)>,
    method: &Ident,
    variant: &Ident,
    field: &syn::Field,
    shape: &FieldShape,
) -> Result<()> {
    let name = method.to_string();
    let Some((previous, previous_shape)) = accessors.get(&name) else {
        accessors.insert(name, (variant.to_string(), shape.clone()));
        return Ok(());
    };
    if same_shape(previous_shape, shape) {
        return Ok(());
    }
    let span = match &field.ident {
        Some(ident) => ident.span(),
        None => field.ty.span(),
    };
    Err(syn::Error::new(
        span,
        format!(
            "field of variant `{variant}` would generate accessor `{name}`, which \
             variant `{previous}` already declares with a different type: one \
             accessor name may serve several variants only when it reads the same \
             thing in each. Rename one of the fields"
        ),
    ))
}

/// Whether two fields answer the same question.
fn same_shape(left: &FieldShape, right: &FieldShape) -> bool {
    match (left, right) {
        (FieldShape::Leaf(left), FieldShape::Leaf(right)) => {
            leaf_name(left).ok() == leaf_name(right).ok()
        }
        (FieldShape::Child(left), FieldShape::Child(right)) => left == right,
        (FieldShape::OptionalChild(left), FieldShape::OptionalChild(right)) => left == right,
        (FieldShape::VecChild(left), FieldShape::VecChild(right)) => left == right,
        _ => false,
    }
}

/// One field's contribution to a merged accessor.
struct Projection {
    kind_variant: Ident,
    leaf_slot: u16,
    child_slot: u16,
    shape: FieldShape,
}

/// Adds one field to the accessor table.
fn push_projection(
    projections: &mut Vec<(Ident, Vec<Projection>)>,
    field: &FieldSpec,
    variant: &Ident,
) {
    let spec = Projection {
        kind_variant: variant.clone(),
        leaf_slot: field.leaf_slot,
        child_slot: field.child_slot,
        shape: field.shape.clone(),
    };
    if let Some((_, specs)) = projections
        .iter_mut()
        .find(|(name, _)| *name == field.method)
    {
        specs.push(spec);
        return;
    }
    projections.push((field.method.clone(), vec![spec]));
}

fn classify(ty: &Type, names: &HashSet<String>) -> FieldShape {
    if let Some(name) = path_enum(ty, names) {
        return FieldShape::Child(name);
    }
    if let Type::Path(path) = ty {
        if let Some(segment) = path.path.segments.last() {
            let ident = segment.ident.to_string();
            let args = generic_types(segment);
            match ident.as_str() {
                "Box" if args.len() == 1 => {
                    if let Some(name) = path_enum(&args[0], names) {
                        return FieldShape::Child(name);
                    }
                }
                "Vec" if args.len() == 1 => {
                    if let Some(name) = path_enum(&args[0], names) {
                        return FieldShape::VecChild(name);
                    }
                }
                "Option" if args.len() == 1 => {
                    let inner = if let Type::Path(inner_path) = &args[0] {
                        if inner_path
                            .path
                            .segments
                            .last()
                            .is_some_and(|segment| segment.ident == "Box")
                        {
                            generic_types(inner_path.path.segments.last().expect("box segment"))
                                .first()
                                .cloned()
                                .unwrap_or_else(|| args[0].clone())
                        } else {
                            args[0].clone()
                        }
                    } else {
                        args[0].clone()
                    };
                    if let Some(name) = path_enum(&inner, names) {
                        return FieldShape::OptionalChild(name);
                    }
                }
                _ => {}
            }
        }
    }
    FieldShape::Leaf(Box::new(ty.clone()))
}

fn path_enum(ty: &Type, names: &HashSet<String>) -> Option<String> {
    let Type::Path(path) = ty else {
        return None;
    };
    let segment = path.path.segments.last()?;
    let name = segment.ident.to_string();
    names.contains(&name).then_some(name)
}

fn generic_types(segment: &syn::PathSegment) -> Vec<Type> {
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return Vec::new();
    };
    args.args
        .iter()
        .filter_map(|arg| match arg {
            GenericArgument::Type(ty) => Some(ty.clone()),
            _ => None,
        })
        .collect()
}

fn build_fn_for(enum_name: &str) -> Ident {
    format_ident!("build_{}", snake_case(enum_name))
}

fn snake_case(name: &str) -> String {
    let mut out = String::new();
    for (index, ch) in name.chars().enumerate() {
        if ch.is_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// The layout entry for one enum-typed field: the index of the declared type.
fn layout_target(index_by_name: &HashMap<String, u16>, name: &str, variant: &Ident) -> Result<u16> {
    index_by_name.get(name).copied().ok_or_else(|| {
        syn::Error::new(
            variant.span(),
            format!("field of variant `{variant}` names `{name}`, which is not declared in this tree module"),
        )
    })
}

/// The storage kind a leaf type is written as.
fn leaf_kind(ty: &Type) -> Result<TokenStream> {
    let name = leaf_name(ty)?;
    Ok(match name.as_str() {
        "String" => quote!(irrigation::LeafKind::Text),
        "i64" => quote!(irrigation::LeafKind::Int),
        "u32" | "u64" | "usize" => quote!(irrigation::LeafKind::UInt),
        "f64" => quote!(irrigation::LeafKind::Float),
        "bool" => quote!(irrigation::LeafKind::Bool),
        other => {
            return Err(syn::Error::new_spanned(
                ty,
                format!("unsupported leaf type `{other}` in a tree View"),
            ));
        }
    })
}

fn leaf_build(ty: &Type, value: &TokenStream) -> Result<TokenStream> {
    let name = leaf_name(ty)?;
    Ok(match name.as_str() {
        "String" => quote!(irrigation::TreeValue::Text(#value.clone())),
        "i64" => quote!(irrigation::TreeValue::Int(*#value)),
        "u32" | "u64" | "usize" => quote!(irrigation::TreeValue::UInt(*#value as u64)),
        "f64" => quote!(irrigation::TreeValue::Float(*#value)),
        "bool" => quote!(irrigation::TreeValue::Bool(*#value)),
        other => {
            return Err(syn::Error::new_spanned(
                ty,
                format!("unsupported leaf type `{other}` in a tree View"),
            ));
        }
    })
}

fn leaf_extract(ty: &Type) -> Result<TokenStream> {
    let name = leaf_name(ty)?;
    Ok(match name.as_str() {
        "String" => quote!(value.as_text().map(str::to_string)),
        "i64" => quote!(value.as_int()),
        "u32" => quote!(value.as_uint().map(|raw| raw as u32)),
        "u64" => quote!(value.as_uint()),
        "usize" => quote!(value.as_uint().map(|raw| raw as usize)),
        "f64" => quote!(value.as_float()),
        "bool" => quote!(value.as_bool()),
        other => {
            return Err(syn::Error::new_spanned(
                ty,
                format!("unsupported leaf type `{other}` in a tree View"),
            ));
        }
    })
}

fn leaf_name(ty: &Type) -> Result<String> {
    match ty {
        Type::Path(path) => Ok(path
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string())
            .unwrap_or_default()),
        other => Err(syn::Error::new_spanned(
            other,
            "tree leaf fields must be named scalar types",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quote::quote;

    #[test]
    fn two_variants_share_an_accessor_when_they_agree() {
        let tokens = expand(
            quote!(root = Item, view = Tree),
            quote! {
                mod tree {
                    #[derive(Clone, Debug, PartialEq)]
                    pub enum Item {
                        Leaf { value: u64 },
                        Pair { value: u64, other: u64 },
                    }
                }
            },
        )
        .to_string();
        assert!(!tokens.contains("compile_error"), "{tokens}");
        assert_eq!(
            tokens.matches("pub fn value").count(),
            1,
            "one accessor serves both variants: {tokens}"
        );
        assert!(
            tokens.contains("Some (ItemKind :: Leaf)")
                && tokens.contains("Some (ItemKind :: Pair)"),
            "the accessor dispatches on the discriminant: {tokens}"
        );
    }

    #[test]
    fn two_variants_cannot_share_a_name_with_different_types() {
        let tokens = expand(
            quote!(root = Item, view = Tree),
            quote! {
                mod tree {
                    #[derive(Clone, Debug, PartialEq)]
                    pub enum Item {
                        Leaf { value: u64 },
                        Named { value: String },
                    }
                }
            },
        )
        .to_string();
        assert!(
            tokens.contains("compile_error"),
            "a shared name with two types must be rejected at the source: {tokens}"
        );
        assert!(
            tokens.contains("`Leaf`") && tokens.contains("`Named`"),
            "the diagnostic must name both variants: {tokens}"
        );
    }
}
