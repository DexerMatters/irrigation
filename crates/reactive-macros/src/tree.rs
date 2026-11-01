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

use std::collections::HashSet;

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{Fields, GenericArgument, Ident, Item, ItemMod, PathArguments, Result, Type, Visibility};

/// A field of one enum variant.
enum FieldShape {
    Leaf(Box<Type>),
    Child(String),
    OptionalChild(String),
    VecChild(String),
}

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

    let view_ident = parsed.view.clone();
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
            #[derive(Clone, PartialEq, Eq, Hash, Debug)]
            pub struct #ref_ident {
                inner: reactive::TreeRef,
            }

            impl #ref_ident {
                /// Wraps an untyped tree ref.
                #[must_use]
                pub fn from_tree(inner: reactive::TreeRef) -> Self {
                    Self { inner }
                }

                /// The node identity.
                #[must_use]
                pub fn raw(&self) -> reactive::TreeNodeId {
                    self.inner.id()
                }

                /// The underlying ref.
                #[must_use]
                pub fn tree(&self) -> &reactive::TreeRef {
                    &self.inner
                }

                /// Reads the discriminant, observing only the tag unit.
                #[must_use]
                pub fn kind(&self) -> Option<#kind_ident> {
                    #kind_ident::from_index(self.inner.tag()?.1)
                }
            }
        });

        let mut accessors = Vec::new();
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
                            reactive::BuildSlot::One(Box::new(#build(#field_ident)))
                        });
                    }
                    FieldShape::OptionalChild(enum_name) => {
                        patterns.push(quote!(#field_ident));
                        let build = build_fn_for(enum_name);
                        children.push(quote! {
                            reactive::BuildSlot::Optional(#field_ident.as_ref().map(|inner| Box::new(#build(inner))))
                        });
                    }
                    FieldShape::VecChild(enum_name) => {
                        patterns.push(quote!(#field_ident));
                        let build = build_fn_for(enum_name);
                        children.push(quote! {
                            reactive::BuildSlot::Many(#field_ident.iter().map(|inner| #build(inner)).collect())
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
                #name::#variant_ident #pattern => reactive::TreeBuild {
                    type_index: #type_index,
                    variant: #variant_index as u16,
                    fields: vec![#(#field_exprs),*],
                    children: vec![#(#children),*],
                },
            });

            for field in &variant.fields {
                let method = field.method.clone();
                let leaf_slot = field.leaf_slot;
                let child_slot = field.child_slot;
                let accessor = match &field.shape {
                    FieldShape::Leaf(ty) => {
                        let extract = leaf_extract(ty)?;
                        quote! {
                            /// Reads this projection, observing only its unit.
                            #[must_use]
                            pub fn #method(&self) -> Option<#ty> {
                                if self.kind() != Some(#kind_ident::#variant_ident) {
                                    return None;
                                }
                                self.inner.field(#leaf_slot).and_then(|value| #extract)
                            }
                        }
                    }
                    FieldShape::Child(enum_name) => {
                        let child_ref = format_ident!("{}Ref", enum_name);
                        quote! {
                            /// Reads this child projection.
                            #[must_use]
                            pub fn #method(&self) -> Option<#child_ref> {
                                if self.kind() != Some(#kind_ident::#variant_ident) {
                                    return None;
                                }
                                match self.inner.child_slot(#child_slot) {
                                    reactive::ChildSlot::One(id) => {
                                        Some(#child_ref::from_tree(self.inner.child(id)))
                                    }
                                    _ => None,
                                }
                            }
                        }
                    }
                    FieldShape::OptionalChild(enum_name) => {
                        let child_ref = format_ident!("{}Ref", enum_name);
                        quote! {
                            /// Reads this optional child projection.
                            #[must_use]
                            pub fn #method(&self) -> Option<#child_ref> {
                                if self.kind() != Some(#kind_ident::#variant_ident) {
                                    return None;
                                }
                                match self.inner.child_slot(#child_slot) {
                                    reactive::ChildSlot::Optional(Some(id)) => {
                                        Some(#child_ref::from_tree(self.inner.child(id)))
                                    }
                                    _ => None,
                                }
                            }
                        }
                    }
                    FieldShape::VecChild(enum_name) => {
                        let child_ref = format_ident!("{}Ref", enum_name);
                        quote! {
                            /// Reads this child list projection.
                            #[must_use]
                            pub fn #method(&self) -> Vec<#child_ref> {
                                if self.kind() != Some(#kind_ident::#variant_ident) {
                                    return Vec::new();
                                }
                                self.inner
                                    .child_slot(#child_slot)
                                    .ids()
                                    .into_iter()
                                    .map(|id| #child_ref::from_tree(self.inner.child(id)))
                                    .collect()
                            }
                        }
                    }
                };
                accessors.push(accessor);
            }
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
            pub fn #build_ident(value: &#name) -> reactive::TreeBuild {
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
                        reactive::TreeEditor::new(&mut *self.edit, #view_const, storage);
                    tree.replace(node.raw(), &#build_ident(value))
                };
                let storage = self.storage.as_ref().expect("editor storage");
                #ref_ident::from_tree(reactive::TreeRef::snapshot(
                    ::std::sync::Arc::new(storage.clone()),
                    id,
                ))
            }

            /// Removes a subtree rooted at this node.
            pub fn #remove_name(&mut self, node: #ref_ident) {
                let storage = self.storage.as_mut().expect("editor storage");
                let mut tree = reactive::TreeEditor::new(&mut *self.edit, #view_const, storage);
                tree.remove_subtree(node.raw());
            }
        });
    }

    // ------------------------------------------------------------- the View
    let root_spec = &specs[0];
    let root_ident = root_spec.ident.clone();
    let root_ref = format_ident!("{}Ref", root_spec.ident);
    let root_build = format_ident!("build_{}", snake_case(&root_spec.ident.to_string()));

    let content_impl = quote! {
        fn #content_fn(content: &dyn ::std::any::Any) -> reactive::TreeStorage {
            content
                .downcast_ref::<reactive::TreeStorage>()
                .expect(#view_name)
                .clone()
        }

        const #view_const: reactive::ViewId = reactive::ViewId(
            reactive::const_hash(concat!(module_path!(), "::", #view_name)),
        );

        static #descriptor: reactive::ViewDescriptor = reactive::ViewDescriptor {
            id: #view_const,
            name: concat!(module_path!(), "::", #view_name),
            kind: reactive::ViewKind::Source,
            fields: reactive::tree_fields(),
            content: reactive::tree_content_vtable(),
        };
    };

    let view_def = quote! {
        /// The tree View handle.
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        pub struct #view_ident;

        impl reactive::ViewSchema for #view_ident {
            const DESCRIPTOR: &'static reactive::ViewDescriptor = &#descriptor;
            type Content = reactive::TreeStorage;
            type Snapshot<'a> = #snapshot_ident;

            fn snapshot<'a>(
                content: &'a ::std::sync::Arc<dyn ::std::any::Any + Send + Sync>,
            ) -> Self::Snapshot<'a> {
                #snapshot_ident {
                    storage: ::std::sync::Arc::new(
                        content
                            .downcast_ref::<reactive::TreeStorage>()
                            .expect(#view_name)
                            .clone(),
                    ),
                }
            }
        }

        impl reactive::SourceView for #view_ident {}

        impl #view_ident {
            /// Reads the root identity, observing only the root unit.
            #[must_use]
            pub fn root(&self) -> Option<#root_ref> {
                let node: Option<reactive::TreeNodeId> = reactive::try_read_unit_value(
                    reactive::UnitKey::field(
                        #view_const,
                        reactive::FieldId(reactive::tree_slots::ROOT),
                        reactive::UnitKind::Value,
                    ),
                    |content| Some(#content_fn(content).root),
                )
                .flatten();
                node.map(|id| {
                    #root_ref::from_tree(reactive::TreeRef::live(
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
            storage: ::std::sync::Arc<reactive::TreeStorage>,
        }

        impl #snapshot_ident {
            /// Reads the root identity.
            #[must_use]
            pub fn root(&self) -> Option<#root_ref> {
                self.storage.root.map(|id| {
                    #root_ref::from_tree(reactive::TreeRef::snapshot(
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

    let editor_def = quote! {
        /// Transactional editor for the tree View.
        pub struct #editor_ident<'a> {
            edit: &'a mut reactive::Edit,
            storage: Option<reactive::TreeStorage>,
        }

        impl<'a> #editor_ident<'a> {
            /// Replaces the whole tree, returning its new root handle.
            pub fn set_root(&mut self, document: &#root_ident) -> #root_ref {
                let id = {
                    let storage = self.storage.as_mut().expect("editor storage");
                    let mut tree =
                        reactive::TreeEditor::new(&mut *self.edit, #view_const, storage);
                    tree.set_root(&#root_build(document))
                };
                let storage = self.storage.as_ref().expect("editor storage");
                #root_ref::from_tree(reactive::TreeRef::snapshot(
                    ::std::sync::Arc::new(storage.clone()),
                    id,
                ))
            }

            /// Replaces one node's payload and children, preserving its identity.
            pub fn replace_root(&mut self, node: #root_ref, document: &#root_ident) -> #root_ref {
                let id = {
                    let storage = self.storage.as_mut().expect("editor storage");
                    let mut tree =
                        reactive::TreeEditor::new(&mut *self.edit, #view_const, storage);
                    tree.replace(node.raw(), &#root_build(document))
                };
                let storage = self.storage.as_ref().expect("editor storage");
                #root_ref::from_tree(reactive::TreeRef::snapshot(
                    ::std::sync::Arc::new(storage.clone()),
                    id,
                ))
            }

            /// Removes a subtree.
            pub fn remove_subtree(&mut self, node: #root_ref) {
                let storage = self.storage.as_mut().expect("editor storage");
                let mut tree = reactive::TreeEditor::new(&mut *self.edit, #view_const, storage);
                tree.remove_subtree(node.raw());
            }

            #( #editor_methods )*

            /// Reads the current root as a snapshot ref over the transaction's
            /// staged tree, so navigation does not require a computation frame.
            pub fn root(&self) -> #root_ref {
                let storage = self.storage.as_ref().expect("editor storage");
                let id = storage.root.expect("tree has no root");
                #root_ref::from_tree(reactive::TreeRef::snapshot(
                    ::std::sync::Arc::new(storage.clone()),
                    id,
                ))
            }

            /// Moves a subtree under a new parent and child slot.
            pub fn move_subtree(
                &mut self,
                node: reactive::TreeNodeId,
                new_parent: reactive::TreeNodeId,
                slot: u16,
                index: usize,
            ) -> bool {
                let storage = self.storage.as_mut().expect("editor storage");
                let mut tree = reactive::TreeEditor::new(&mut *self.edit, #view_const, storage);
                tree.move_subtree(node, new_parent, slot, index)
            }
        }

        impl reactive::EditableView for #view_ident {
            type Editor<'a> = #editor_ident<'a>;

            fn edit<'a>(edit: &'a mut reactive::Edit) -> Self::Editor<'a> {
                edit.ensure_view(<#view_ident as reactive::ViewSchema>::DESCRIPTOR);
                let storage = (*edit.content::<reactive::TreeStorage>(#view_const)).clone();
                #editor_ident {
                    edit,
                    storage: Some(storage),
                }
            }
        }

        impl Drop for #editor_ident<'_> {
            fn drop(&mut self) {
                if let Some(storage) = self.storage.take() {
                    self.edit.set_content(
                        #view_const,
                        ::std::sync::Arc::new(storage),
                        reactive::tree_eq,
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
            #( #build_fns )*
            #view_def
            #editor_def
        }
    })
}

struct TreeArgs {
    root: Ident,
    view: Ident,
}

impl syn::parse::Parse for TreeArgs {
    fn parse(input: syn::parse::ParseStream) -> Result<Self> {
        let mut root = None;
        let mut view = None;
        while !input.is_empty() {
            let key: Ident = input.parse()?;
            input.parse::<syn::Token![=]>()?;
            let value: Ident = input.parse()?;
            match key.to_string().as_str() {
                "root" => root = Some(value),
                "view" => view = Some(value),
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!("unknown `#[tree_view]` argument `{other}`"),
                    ))
                }
            }
            if input.peek(syn::Token![,]) {
                input.parse::<syn::Token![,]>()?;
            }
        }
        Ok(Self {
            root: root.ok_or_else(|| syn::Error::new(input.span(), "missing `root = ...`"))?,
            view: view.ok_or_else(|| syn::Error::new(input.span(), "missing `view = ...`"))?,
        })
    }
}

type ItemEnum = syn::ItemEnum;

fn enum_spec(item: &ItemEnum, names: &HashSet<String>, type_index: u16) -> Result<EnumSpec> {
    let tuple_methods: HashSet<String> = item
        .variants
        .iter()
        .filter(|variant| {
            matches!(variant.fields, Fields::Unnamed(_))
                && variant.fields.iter().count() == 1
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
                    fields.push(FieldSpec {
                        ident,
                        method,
                        shape,
                        leaf_slot: leaf_slot_for(&fields),
                        child_slot: child_slot_for(&fields),
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
                    fields.push(FieldSpec {
                        ident,
                        method,
                        shape,
                        leaf_slot: leaf_slot_for(&fields),
                        child_slot: child_slot_for(&fields),
                    });
                }
            }
        }
        // Recompute slots deterministically after the list is complete.
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

fn leaf_slot_for(fields: &[FieldSpec]) -> u16 {
    fields
        .iter()
        .filter(|field| matches!(field.shape, FieldShape::Leaf(_)))
        .count() as u16
}

fn child_slot_for(fields: &[FieldSpec]) -> u16 {
    fields
        .iter()
        .filter(|field| !matches!(field.shape, FieldShape::Leaf(_)))
        .count() as u16
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
                            generic_types(
                                inner_path.path.segments.last().expect("box segment"),
                            )
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

fn leaf_build(ty: &Type, value: &TokenStream) -> Result<TokenStream> {
    let name = leaf_name(ty)?;
    Ok(match name.as_str() {
        "String" => quote!(reactive::TreeValue::Text(#value.clone())),
        "i64" => quote!(reactive::TreeValue::Int(*#value)),
        "u32" | "u64" | "usize" => quote!(reactive::TreeValue::UInt(*#value as u64)),
        "f64" => quote!(reactive::TreeValue::Float(*#value)),
        "bool" => quote!(reactive::TreeValue::Bool(*#value)),
        other => {
            return Err(syn::Error::new_spanned(
                ty,
                format!("unsupported leaf type `{other}` in a tree View"),
            ))
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
            ))
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

#[allow(dead_code)]
fn unused(_: Visibility) {}

