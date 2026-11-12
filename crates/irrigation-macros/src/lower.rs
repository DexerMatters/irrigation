//! Expression lowering.
//!
//! The macro turns a Rust-shaped component body into tracked primitives:
//!
//! * every value-producing expression becomes a `irrigation::eval(site, ...)` node;
//! * every local binding remembers the node that produced its value;
//! * every use of such a local observes that node, so a primitive that consumes
//!   a local still depends on the computation that produced it.
//!
//! A value that goes through an iterator adapter carries no node (`cloned`,
//! `rev`, `chain`, ... are transparent so a chain is not memoized at every
//! step). A local bound from one is therefore untracked, and a later `if let` or
//! `match` on that local would keep the answer it saw first. Match where the
//! read is — `match node.children().get(1) { ... }` — so the branch observes the
//! tree unit instead of a hoisted, node-less copy.
//!
//! Constructs the lowering cannot preserve are rejected with a source-level
//! diagnostic rather than silently made coarse.
//!
//! Macros are lowered argument by argument. A read that lives inside a format
//! literal cannot be rewritten — the capture is part of the string — so it is
//! recorded by name against the binding's node instead.

use std::collections::{HashMap, HashSet};

use proc_macro2::TokenStream;
use quote::{ToTokens, format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::{Block, Expr, ExprClosure, ExprMacro, Ident, LitStr, Pat, Stmt, Token};

/// One lexical scope: a name maps to the node that produced its value, or to
/// `None` when a binder shadows an outer tracked name without producing one.
///
/// Iterator values carry no node. `iterators` remembers the names bound to
/// them, so a later adapter on the same name is classified as a lazy chain
/// instead of collection work.
#[derive(Default)]
struct Scope {
    bindings: HashMap<String, Option<TokenStream>>,
    iterators: HashSet<String>,
}

impl Scope {
    fn new() -> Self {
        Self::default()
    }
}

/// A lowered expression and, when it is backed by one node, that node.
struct Lowered {
    tokens: TokenStream,
    node: Option<TokenStream>,
    /// Iterator values are not memoized: they borrow their source and are
    /// consumed within one execution of the enclosing computation.
    iterator: bool,
}

impl Lowered {
    fn plain(tokens: TokenStream) -> Self {
        Self {
            tokens,
            node: None,
            iterator: false,
        }
    }

    fn node(tokens: TokenStream, node: TokenStream) -> Self {
        Self {
            tokens,
            node: Some(node),
            iterator: false,
        }
    }

    fn iter(tokens: TokenStream) -> Self {
        Self {
            tokens,
            node: None,
            iterator: true,
        }
    }
}

/// Lowers one component body.
pub struct Lowerer {
    counter: u32,
    strict: bool,
    allowed: Vec<String>,
    scopes: Vec<Scope>,
    /// Inside a closure body no per-expression nodes are created: the closure
    /// may be called many times in one scope (for example by an iterator
    /// adapter), so per-call node identity cannot be assigned syntactically.
    /// Tracked reads still record on the enclosing computation.
    inline_depth: usize,
    /// Label of the enclosing component body, if `return` is being lowered to
    /// `break`. `None` keeps the old rejection for a helper body.
    return_label: Option<syn::Lifetime>,
}

impl Lowerer {
    /// Builds a lowerer for one component function.
    #[must_use]
    pub fn new() -> Self {
        Self {
            counter: 0,
            strict: false,
            allowed: Vec::new(),
            scopes: vec![Scope::new()],
            inline_depth: 0,
            return_label: None,
        }
    }

    /// Enables top-level `return` lowering to `break` on this label.
    pub fn set_return_label(&mut self, label: syn::Lifetime) {
        self.return_label = Some(label);
    }

    fn node_or_inline(&mut self, body: TokenStream) -> Lowered {
        if self.inline_depth > 0 {
            return Lowered::plain(body);
        }
        let site = self.site();
        Lowered::node(
            quote!(irrigation::eval(#site, || #body)),
            quote!(irrigation::node_of(#site)),
        )
    }

    fn site(&mut self) -> TokenStream {
        let counter = self.counter;
        self.counter += 1;
        // Sequential per component; the high half is reserved for the macro so
        // operator-defined site ids (1, 2, ...) can never collide with it.
        quote! {
            irrigation::SiteId(0x8000_0000u32 | (#counter as u32))
        }
    }

    /// Sets the strict-mode policy for opaque calls.
    pub fn set_policy(&mut self, strict: bool, allowed: Vec<String>) {
        self.strict = strict;
        self.allowed = allowed;
    }

    fn is_declared_primitive(&self, name: &str) -> bool {
        const PURE: &[&str] = &[
            "Some",
            "None",
            "Ok",
            "Err",
            "String::from",
            "Vec::new",
            "Vec::from",
            "Box::new",
            "u8::from",
            "u16::from",
            "u32::from",
            "u64::from",
            "usize::from",
            "i8::from",
            "i16::from",
            "i32::from",
            "i64::from",
            "isize::from",
            "f32::from",
            "f64::from",
            "Default::default",
            "std::mem::take",
            "std::mem::replace",
        ];
        if self
            .allowed
            .iter()
            .any(|allowed| name == allowed || name.ends_with(&format!("::{allowed}")))
        {
            return true;
        }
        PURE.contains(&name)
    }

    fn lookup(&self, name: &str) -> Option<Option<TokenStream>> {
        for scope in self.scopes.iter().rev() {
            if let Some(binding) = scope.bindings.get(name) {
                return Some(binding.clone());
            }
        }
        None
    }

    /// Whether the innermost binding of `name` holds a lazy iterator value.
    fn is_iterator_binding(&self, name: &str) -> bool {
        for scope in self.scopes.iter().rev() {
            if scope.bindings.contains_key(name) {
                return scope.iterators.contains(name);
            }
        }
        false
    }

    fn bind(&mut self, name: String, node: TokenStream) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.bindings.insert(name, Some(node));
        }
    }

    /// Records that `name` is bound to an iterator value, which has no node.
    fn bind_iterator(&mut self, name: String) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.iterators.insert(name);
        }
    }

    /// Records that `name` is now bound by a binder that has no node of its
    /// own, so an outer tracked local of the same name is out of scope.
    fn shadow(&mut self, name: String) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.bindings.insert(name.clone(), None);
            scope.iterators.remove(&name);
        }
    }

    fn shadow_pattern(&mut self, pat: &Pat) {
        for name in pattern_names(pat) {
            self.shadow(name);
        }
    }

    /// Binds every name a pattern introduces to the node whose value it projects.
    ///
    /// A pattern binding is a projection of the scrutinee, so a read of the name
    /// is a read of that value: the site that uses it must observe the scrutinee
    /// and re-execute when the scrutinee changes. Without this, a site built from
    /// a match-bound value has no dependency at all and freezes at its first
    /// answer.
    fn bind_pattern_to(&mut self, pat: &Pat, node: &Option<TokenStream>) {
        for name in pattern_names(pat) {
            match node {
                Some(node) => self.bind(name, node.clone()),
                None => self.shadow(name),
            }
        }
    }

    /// Lowers a block.
    pub fn block(&mut self, block: &Block) -> TokenStream {
        self.scopes.push(Scope::new());
        let last = block.stmts.len().saturating_sub(1);
        let statements: Vec<TokenStream> = block
            .stmts
            .iter()
            .enumerate()
            .map(|(index, stmt)| self.stmt(stmt, index == last))
            .collect();
        self.scopes.pop();
        quote!({ #(#statements)* })
    }

    fn stmt(&mut self, stmt: &Stmt, is_tail: bool) -> TokenStream {
        match stmt {
            Stmt::Local(local) => {
                if let syn::Pat::Ident(binding) = &local.pat {
                    if binding.mutability.is_some() {
                        return error(
                            &local.pat,
                            "`let mut` is not supported in a component body; build the value \
                                 functionally instead",
                        )
                        .tokens;
                    }
                }
                let pat = &local.pat;
                let Some(init) = &local.init else {
                    self.shadow_pattern(pat);
                    return quote!(let #pat;);
                };
                if init.diverge.is_some() {
                    return error(
                        &init.expr,
                        "`let ... else` is not supported in a component body",
                    )
                    .tokens;
                }
                // The initializer still sees the previous binding of the same
                // name; the shadow is installed afterwards.
                let lowered = self.expr_node(&init.expr);
                self.shadow_pattern(pat);
                // An iterator value has no node, but the name still has to
                // remember that it is one: the next adapter on it is a lazy
                // chain, exactly as if the chain had never been named.
                if lowered.iterator {
                    for name in pattern_names(pat) {
                        self.bind_iterator(name);
                    }
                    let tokens = lowered.tokens;
                    return quote!(let #pat = #tokens;);
                }
                if self.inline_depth > 0 {
                    let tokens = lowered.tokens;
                    return quote!(let #pat = #tokens;);
                }
                let (tokens, node) = match (&lowered.node, is_trivial(&init.expr)) {
                    (Some(node), _) => (lowered.tokens, Some(node.clone())),
                    (None, true) => (lowered.tokens, None),
                    (None, false) => {
                        let site = self.site();
                        let inner = lowered.tokens;
                        let tokens = quote!(irrigation::eval(#site, || #inner));
                        (tokens, Some(quote!(irrigation::node_of(#site))))
                    }
                };
                let names = pattern_names(pat);
                let Some(node) = node else {
                    return quote!(let #pat = #tokens;);
                };
                let binding = format_ident!(
                    "__node_{}",
                    names
                        .first()
                        .cloned()
                        .unwrap_or_else(|| String::from("value"))
                );
                for name in &names {
                    self.bind(name.clone(), quote!(#binding));
                }
                quote! {
                    let #pat = #tokens;
                    let #binding = #node;
                }
            }
            Stmt::Expr(expr, semi) => {
                let lowered = self.expr_node(expr);
                let mut tokens = lowered.tokens;
                // Control-flow statements are effect barriers: a branch can
                // stage definitions, and a switched-off branch must retire
                // them before the next statement reads the affected units.
                // Giving the statement its own node makes that retirement
                // cascade at the statement boundary instead of at the end of
                // the enclosing component.
                // A branch that contains a lowered `return` already diverges;
                // wrapping its tokens in an `eval` closure would put `break`
                // inside a closure, where the component label is unreachable.
                let has_return = tokens.to_string().contains("break '__irrigation_component");
                let wrapped = !has_return
                    && lowered.node.is_none()
                    && self.inline_depth == 0
                    && matches!(expr, Expr::If(_) | Expr::Match(_) | Expr::Block(_));
                if wrapped {
                    let site = self.site();
                    tokens = quote!(irrigation::eval(#site, || #tokens));
                }
                if semi.is_some() || (wrapped && !is_tail) {
                    quote!(#tokens;)
                } else {
                    quote!(#tokens)
                }
            }
            Stmt::Macro(mac) => {
                let macro_expr = ExprMacro {
                    attrs: mac.attrs.clone(),
                    mac: mac.mac.clone(),
                };
                let lowered = self.macro_expr(&macro_expr).tokens;
                quote!(#lowered;)
            }
            Stmt::Item(item) => {
                error(item, "nested items are not supported in a component body").tokens
            }
        }
    }

    /// Lowers an expression to tokens.
    pub fn expr(&mut self, expr: &Expr) -> TokenStream {
        self.expr_node(expr).tokens
    }

    fn expr_node(&mut self, expr: &Expr) -> Lowered {
        match expr {
            Expr::Lit(lit) => Lowered::plain(quote!(#lit)),
            Expr::Path(path) => {
                if path.qself.is_none() && path.path.segments.len() == 1 {
                    let name = path.path.segments[0].ident.to_string();
                    if let Some(Some(node)) = self.lookup(&name) {
                        let ident = &path.path.segments[0].ident;
                        let tokens = quote!(irrigation::observe_clone(#node, &#ident));
                        return Lowered::node(tokens, node);
                    }
                }
                Lowered::plain(quote!(#path))
            }
            Expr::Paren(paren) => self.expr_node(&paren.expr),
            Expr::Group(group) => self.expr_node(&group.expr),
            Expr::Reference(reference) => {
                let inner = self.expr_node(&reference.expr);
                let tokens = if reference.mutability.is_some() {
                    let inner = inner.tokens;
                    quote!(&mut #inner)
                } else {
                    let inner = inner.tokens;
                    quote!(&#inner)
                };
                Lowered::plain(tokens)
            }
            Expr::Binary(binary) => {
                let left = self.expr(&binary.left);
                let right = self.expr(&binary.right);
                let op = &binary.op;
                self.node_or_inline(quote!(#left #op #right))
            }
            Expr::Unary(unary) => {
                let inner = self.expr(&unary.expr);
                let op = &unary.op;
                self.node_or_inline(quote!(#op #inner))
            }
            Expr::Cast(cast) => {
                let inner = self.expr(&cast.expr);
                let ty = &cast.ty;
                self.node_or_inline(quote!(#inner as #ty))
            }
            Expr::MethodCall(call) => {
                let method = &call.method;
                let method_name = method.to_string();
                let receiver = if is_iterator_method(&method_name) {
                    self.borrowed_receiver(&call.receiver)
                } else {
                    self.expr(&call.receiver)
                };
                let turbofish = &call.turbofish;
                let args: Vec<TokenStream> =
                    call.args.iter().map(|arg| self.argument(arg)).collect();
                let iterator_chain = is_collection_adapter(&method_name)
                    && self.receiver_is_iterator(&call.receiver);
                if is_transparent_method(&method_name) || iterator_chain {
                    let tokens = quote!(#receiver.#method #turbofish (#(#args),*));
                    return if is_iterator_method(&method_name) || iterator_chain {
                        Lowered::iter(tokens)
                    } else {
                        Lowered::plain(tokens)
                    };
                }
                self.node_or_inline(quote!(#receiver.#method #turbofish (#(#args),*)))
            }
            Expr::Call(call) => {
                if self.strict
                    && !self.is_declared_primitive(&call.func.to_token_stream().to_string())
                {
                    return error(
                        &call.func,
                        "opaque call in a strict component; declare it with \
                         `#[component(strict, allows = [path])]`, or move the work into a \
                         `GeneralComponent`",
                    );
                }
                let function = self.expr(&call.func);
                let args: Vec<TokenStream> =
                    call.args.iter().map(|arg| self.argument(arg)).collect();
                self.node_or_inline(quote!(#function(#(#args),*)))
            }
            Expr::Field(field) => {
                let base = self.expr(&field.base);
                let member = &field.member;
                self.node_or_inline(quote!(#base.#member))
            }
            Expr::Index(index) => {
                let base = self.expr(&index.expr);
                let offset = self.expr(&index.index);
                self.node_or_inline(quote!(#base[#offset]))
            }
            Expr::Tuple(tuple) => {
                let elems: Vec<TokenStream> =
                    tuple.elems.iter().map(|elem| self.expr(elem)).collect();
                self.node_or_inline(quote!((#(#elems),*)))
            }
            Expr::Array(array) => {
                let elems: Vec<TokenStream> =
                    array.elems.iter().map(|elem| self.expr(elem)).collect();
                self.node_or_inline(quote!([#(#elems),*]))
            }
            Expr::Repeat(repeat) => {
                let value = self.expr(&repeat.expr);
                let len = self.expr(&repeat.len);
                self.node_or_inline(quote!([#value; #len]))
            }
            Expr::Struct(structure) => {
                let path = &structure.path;
                let fields: Vec<TokenStream> = structure
                    .fields
                    .iter()
                    .map(|field| {
                        let member = &field.member;
                        let value = self.expr(&field.expr);
                        match member {
                            syn::Member::Named(_) => quote!(#member: #value),
                            syn::Member::Unnamed(_) => quote!(#value),
                        }
                    })
                    .collect();
                let rest = structure.rest.as_ref().map(|rest| {
                    let value = self.expr(rest);
                    quote!(.. #value)
                });
                self.node_or_inline(quote!(#path { #(#fields,)* #rest }))
            }
            Expr::If(if_expr) => {
                // `if let PAT = EXPR` parses as an `Expr::Let` condition. It is
                // pure control flow and lowers exactly to a two-arm match, so a
                // component need not spell the same program as nested matches.
                if let Expr::Let(let_expr) = &*if_expr.cond {
                    let scrutinee = self.expr_node(&let_expr.expr);
                    let observe = scrutinee
                        .node
                        .as_ref()
                        .map(|node| quote!(irrigation::observe_local(#node);));
                    let scrutinee_tokens = &scrutinee.tokens;
                    let pattern = &let_expr.pat;
                    self.scopes.push(Scope::new());
                    self.bind_pattern_to(pattern, &scrutinee.node);
                    let then_block = self.block(&if_expr.then_branch);
                    self.scopes.pop();
                    // A missing `else` is an empty block, not a unit literal:
                    // the two have the same type, and only one of them teaches
                    // clippy's `unused_unit` to fire inside generated code.
                    let else_branch = if_expr
                        .else_branch
                        .as_ref()
                        .map_or_else(|| quote!(), |(_, branch)| self.expr(branch));
                    let observe = observe.unwrap_or_default();
                    return Lowered::plain(quote! {
                        match #scrutinee_tokens {
                            #pattern => { #observe #then_block },
                            _ => { #observe #else_branch },
                        }
                    });
                }
                let condition = self.expr(&if_expr.cond);
                let then_block = self.block(&if_expr.then_branch);
                let else_branch = if_expr
                    .else_branch
                    .as_ref()
                    .map(|(_, branch)| self.expr(branch));
                let tokens = match else_branch {
                    Some(branch) => quote!(if #condition #then_block else #branch),
                    None => quote!(if #condition #then_block),
                };
                Lowered::plain(tokens)
            }
            Expr::Match(match_expr) => {
                let scrutinee = self.expr_node(&match_expr.expr);
                let observe = scrutinee
                    .node
                    .as_ref()
                    .map(|node| quote!(irrigation::observe_local(#node);));
                let scrutinee_tokens = &scrutinee.tokens;
                let arms: Vec<TokenStream> = match_expr
                    .arms
                    .iter()
                    .map(|arm| {
                        self.scopes.push(Scope::new());
                        match &arm.pat {
                            Pat::Guard(guard) => self.bind_pattern_to(&guard.pat, &scrutinee.node),
                            other => self.bind_pattern_to(other, &scrutinee.node),
                        }
                        let (pattern, guard) = match &arm.pat {
                            Pat::Guard(guard) => {
                                let condition = self.expr(&guard.guard);
                                ((*guard.pat).clone(), quote!(if #condition))
                            }
                            other => (other.clone(), quote!()),
                        };
                        let body = self.expr(&arm.body);
                        self.scopes.pop();
                        let observe = observe.clone();
                        quote!(#pattern #guard => { #observe #body },)
                    })
                    .collect();
                let _ = &arms;
                Lowered::plain(quote!(match #scrutinee_tokens { #(#arms)* }))
            }
            Expr::Block(block) => Lowered::plain(self.block(&block.block)),
            Expr::Try(try_expr) => {
                let inner = self.expr_node(&try_expr.expr);
                let node = inner.node.clone();
                let tokens = inner.tokens;
                Lowered {
                    tokens: quote!(#tokens?),
                    node,
                    iterator: inner.iterator,
                }
            }
            Expr::Assign(assign) => {
                let left = &assign.left;
                if !matches!(**left, Expr::Path(_) | Expr::Field(_)) {
                    return error(
                        left,
                        "only local variables can be assigned inside a component",
                    );
                }
                if let Expr::Path(path) = &**left {
                    if path.qself.is_none() && path.path.segments.len() == 1 {
                        let name = path.path.segments[0].ident.to_string();
                        if matches!(self.lookup(&name), Some(Some(_))) {
                            return error(
                                left,
                                "assignment to a tracked local is not supported; \
                                 build a new value instead",
                            );
                        }
                    }
                }
                let right = self.expr(&assign.right);
                Lowered::plain(quote!(#left = #right))
            }
            Expr::Range(range) => {
                let start = range.start.as_ref().map(|expr| self.expr(expr));
                let end = range.end.as_ref().map(|expr| self.expr(expr));
                let limits = &range.limits;
                self.node_or_inline(quote!(#start #limits #end))
            }
            Expr::Macro(mac) => self.macro_expr(mac),
            Expr::Closure(closure) => error(
                closure,
                "closures are only supported as arguments to collection operators \
                 (`map`, `filter`, `flat_map`)",
            ),
            Expr::Await(await_expr) => {
                error(await_expr, "`.await` is not supported inside a component")
            }
            Expr::Async(async_expr) => error(
                async_expr,
                "async blocks are not supported inside a component",
            ),
            Expr::Unsafe(unsafe_expr) => {
                error(unsafe_expr, "`unsafe` is not supported inside a component")
            }
            Expr::Return(return_expr) => {
                if self.inline_depth > 0 {
                    // Inside a closure, `return` belongs to the closure and must
                    // keep its Rust meaning.
                    let value = return_expr
                        .expr
                        .as_ref()
                        .map_or_else(|| quote!(()), |expr| self.expr(expr));
                    Lowered::plain(quote!(return #value))
                } else if let Some(label) = self.return_label.clone() {
                    let value = return_expr
                        .expr
                        .as_ref()
                        .map_or_else(|| quote!(()), |expr| self.expr(expr));
                    Lowered::plain(quote!(break #label (#value)))
                } else {
                    error(
                        return_expr,
                        "`return` is only supported in a component body",
                    )
                }
            }
            Expr::Break(break_expr) => error(break_expr, "`break` is not supported"),
            Expr::Continue(continue_expr) => error(continue_expr, "`continue` is not supported"),
            Expr::While(while_expr) => error(
                while_expr,
                "`while` loops are not supported; use a collection operator",
            ),
            Expr::Loop(loop_expr) => error(
                loop_expr,
                "`loop` is not supported; use a collection operator",
            ),
            Expr::ForLoop(for_expr) => error(
                for_expr,
                "`for` loops are not supported; use `map`, `filter`, or `flat_map`",
            ),
            Expr::Let(let_expr) => error(let_expr, "`let` expressions are not supported"),
            Expr::Infer(infer) => error(infer, "type inference placeholders are not supported"),
            Expr::Const(const_block) => error(
                const_block,
                "const blocks are not supported inside a component",
            ),
            other => error(other, "unsupported expression in a component body"),
        }
    }

    /// Lowers a receiver that is only borrowed, avoiding a temporary clone.
    fn borrowed_receiver(&mut self, receiver: &Expr) -> TokenStream {
        if let Expr::Path(path) = receiver {
            if path.qself.is_none() && path.path.segments.len() == 1 {
                let name = path.path.segments[0].ident.to_string();
                if let Some(Some(node)) = self.lookup(&name) {
                    let ident = &path.path.segments[0].ident;
                    return quote!(irrigation::observe_ref(#node, &#ident));
                }
            }
        }
        self.expr(receiver)
    }

    /// Returns `true` when the receiver expression is itself an iterator chain.
    ///
    /// A collection adapter is lazy when its receiver is already lazy, so the
    /// classification recurses through any number of `map`/`filter`/`flat_map`
    /// applications and through a path naming an iterator-valued local.
    fn receiver_is_iterator(&self, receiver: &Expr) -> bool {
        match receiver {
            Expr::MethodCall(call) => {
                let name = call.method.to_string();
                is_iterator_method(&name)
                    || (is_collection_adapter(&name) && self.receiver_is_iterator(&call.receiver))
            }
            Expr::Call(call) => call
                .func
                .to_token_stream()
                .to_string()
                .contains("into_iter"),
            Expr::Path(path) if path.qself.is_none() && path.path.segments.len() == 1 => {
                self.is_iterator_binding(&path.path.segments[0].ident.to_string())
            }
            Expr::Paren(paren) => self.receiver_is_iterator(&paren.expr),
            Expr::Group(group) => self.receiver_is_iterator(&group.expr),
            Expr::Reference(reference) => self.receiver_is_iterator(&reference.expr),
            _ => false,
        }
    }

    fn argument(&mut self, arg: &Expr) -> TokenStream {
        match arg {
            Expr::Closure(closure) => self.closure(closure),
            other => self.expr(other),
        }
    }

    fn closure(&mut self, closure: &ExprClosure) -> TokenStream {
        self.inline_depth += 1;
        self.scopes.push(Scope::new());
        for input in &closure.inputs {
            let pattern = match input {
                Pat::Type(typed) => &*typed.pat,
                other => other,
            };
            self.shadow_pattern(pattern);
        }
        let capture = &closure.capture;
        let inputs = closure.inputs.iter().map(|input| match input {
            Pat::Type(_) => quote!(#input),
            other => quote!(#other),
        });
        let body = match &*closure.body {
            Expr::Block(block) => self.block(&block.block),
            other => {
                let expr = self.expr(other);
                quote!({ #expr })
            }
        };
        self.scopes.pop();
        self.inline_depth -= 1;
        quote!(#capture |#(#inputs),*| #body)
    }

    fn macro_expr(&mut self, mac: &ExprMacro) -> Lowered {
        let name = mac
            .mac
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string())
            .unwrap_or_default();
        let path = &mac.mac.path;
        let args = mac.mac.tokens.clone();
        match name.as_str() {
            "vec" => match syn::parse2::<VecArguments>(args) {
                Ok(arguments) => self.lower_vec(path, arguments),
                Err(_) => self.opaque_macro(mac),
            },
            "format" => match syn::parse2::<FormatArguments>(args) {
                Ok(arguments) => self.lower_format(path, arguments),
                Err(_) => self.opaque_macro(mac),
            },
            "matches" => match syn::parse2::<MatchesArguments>(args) {
                Ok(arguments) => self.lower_matches(path, arguments),
                Err(_) => self.opaque_macro(mac),
            },
            "assert" | "debug_assert" => match syn::parse2::<AssertArguments>(args) {
                Ok(arguments) => {
                    let condition = self.expr(&arguments.condition);
                    let message = &arguments.message;
                    self.node_or_inline(quote!(#path!(#condition #message)))
                }
                Err(_) => self.opaque_macro(mac),
            },
            // Their arguments are spelled, never evaluated: no binding can be
            // read through them.
            "concat" | "stringify" => self.node_or_inline(mac.to_token_stream()),
            _ => error(
                mac,
                "this macro is not supported inside a component; \
                 effects belong in observers, not computations",
            ),
        }
    }

    fn lower_vec(&mut self, path: &syn::Path, arguments: VecArguments) -> Lowered {
        if let Some(count) = arguments.count {
            let value = self.expr(&arguments.elements[0]);
            let count = self.expr(&count);
            return self.node_or_inline(quote!(#path!(#value; #count)));
        }
        let elements: Vec<TokenStream> = arguments
            .elements
            .iter()
            .map(|element| self.expr(element))
            .collect();
        self.node_or_inline(quote!(#path!(#(#elements),*)))
    }

    fn lower_format(&mut self, path: &syn::Path, arguments: FormatArguments) -> Lowered {
        let mut named = std::collections::HashSet::new();
        let mut lowered = Vec::new();
        for argument in &arguments.args {
            match argument {
                FormatArgument::Positional(expr) => lowered.push(self.expr(expr)),
                FormatArgument::Named(ident, expr) => {
                    named.insert(ident.to_string());
                    let value = self.expr(expr);
                    lowered.push(quote!(#ident = #value));
                }
            }
        }
        // An explicit named argument wins over an implicit capture of the same
        // name, so it must not be recorded as a capture read.
        let mut observes = Vec::new();
        for name in format_capture_names(&arguments.literal.value()) {
            if named.contains(&name) {
                continue;
            }
            if let Some(Some(node)) = self.lookup(&name) {
                observes.push(quote!(irrigation::observe_local(#node);));
            }
        }
        let literal = &arguments.literal;
        self.observed(observes, quote!(#path!(#literal #(, #lowered)*)))
    }

    fn lower_matches(&mut self, path: &syn::Path, arguments: MatchesArguments) -> Lowered {
        // The scrutinee is evaluated inside the node this macro becomes, so its
        // own `eval` records the observation; observing it beforehand would
        // demand a value the first execution has not produced yet.
        let scrutinee = self.expr(&arguments.scrutinee);
        self.scopes.push(Scope::new());
        self.shadow_pattern(&arguments.pattern);
        let guard = arguments.guard.as_ref().map(|condition| {
            let condition = self.expr(condition);
            quote!(if #condition)
        });
        self.scopes.pop();
        let pattern = &arguments.pattern;
        let guard = guard.unwrap_or_default();
        self.node_or_inline(quote!(#path!(#scrutinee, #pattern #guard)))
    }

    /// Wraps a rebuilt macro in the observations its tokens could not carry.
    fn observed(&mut self, observes: Vec<TokenStream>, tokens: TokenStream) -> Lowered {
        if observes.is_empty() {
            return self.node_or_inline(tokens);
        }
        self.node_or_inline(quote!({ #(#observes)* #tokens }))
    }

    /// Keeps a macro whose arguments did not parse, but observes every tracked
    /// binding in scope: the read cannot be located, so the whole visible set
    /// keeps the node correct at the cost of a coarser trace.
    fn opaque_macro(&mut self, mac: &ExprMacro) -> Lowered {
        let mut seen = std::collections::HashSet::new();
        let mut observes = Vec::new();
        for scope in self.scopes.iter().rev() {
            for node in scope.bindings.values().flatten() {
                if seen.insert(node.to_string()) {
                    observes.push(quote!(irrigation::observe_local(#node);));
                }
            }
        }
        self.observed(observes, mac.to_token_stream())
    }
}

fn error<T: ToTokens>(tokens: T, message: &str) -> Lowered {
    Lowered::plain(syn::Error::new_spanned(tokens, message).to_compile_error())
}

/// `vec!` arguments: the element list, or the `[value; count]` form.
struct VecArguments {
    elements: Vec<Expr>,
    count: Option<Expr>,
}

impl Parse for VecArguments {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut elements = Vec::new();
        if input.is_empty() {
            return Ok(Self {
                elements,
                count: None,
            });
        }
        let first: Expr = input.parse()?;
        if input.peek(Token![;]) {
            let _: Token![;] = input.parse()?;
            let count: Expr = input.parse()?;
            if input.peek(Token![,]) {
                let _: Token![,] = input.parse()?;
            }
            if !input.is_empty() {
                return Err(input.error("unexpected tokens after `vec![value; count]`"));
            }
            return Ok(Self {
                elements: vec![first],
                count: Some(count),
            });
        }
        elements.push(first);
        while input.peek(Token![,]) {
            let _: Token![,] = input.parse()?;
            if input.is_empty() {
                break;
            }
            elements.push(input.parse()?);
        }
        if !input.is_empty() {
            return Err(input.error("unexpected tokens in `vec![...]`"));
        }
        Ok(Self {
            elements,
            count: None,
        })
    }
}

/// One `format!` argument: a positional expression or `name = expression`.
enum FormatArgument {
    Positional(Expr),
    Named(Ident, Expr),
}

impl Parse for FormatArgument {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        if input.peek(Ident) && input.peek2(Token![=]) {
            let name: Ident = input.parse()?;
            let _: Token![=] = input.parse()?;
            return Ok(Self::Named(name, input.parse()?));
        }
        Ok(Self::Positional(input.parse()?))
    }
}

struct FormatArguments {
    literal: LitStr,
    args: Vec<FormatArgument>,
}

impl Parse for FormatArguments {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let literal: LitStr = input.parse()?;
        let mut args = Vec::new();
        if !input.is_empty() {
            let _: Token![,] = input.parse()?;
            while !input.is_empty() {
                args.push(input.parse()?);
                if input.is_empty() {
                    break;
                }
                let _: Token![,] = input.parse()?;
            }
        }
        Ok(Self { literal, args })
    }
}

/// `matches!` arguments: the scrutinee, the pattern, and an optional guard.
struct MatchesArguments {
    scrutinee: Expr,
    pattern: Pat,
    guard: Option<Expr>,
}

impl Parse for MatchesArguments {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let scrutinee: Expr = input.parse()?;
        let _: Token![,] = input.parse()?;
        let pattern = Pat::parse_multi(input)?;
        let guard = if input.peek(Token![if]) {
            let _: Token![if] = input.parse()?;
            Some(input.parse()?)
        } else {
            None
        };
        if input.peek(Token![,]) {
            let _: Token![,] = input.parse()?;
        }
        if !input.is_empty() {
            return Err(input.error("unexpected tokens after the `matches!` pattern"));
        }
        Ok(Self {
            scrutinee,
            pattern,
            guard,
        })
    }
}

/// `assert!` arguments: the condition, then the untouched message tokens.
struct AssertArguments {
    condition: Expr,
    message: TokenStream,
}

impl Parse for AssertArguments {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let condition: Expr = input.parse()?;
        let message: TokenStream = input.parse()?;
        Ok(Self { condition, message })
    }
}

/// Identifier captures spelled inside a format literal: `{name}` and the
/// `name$` width or precision of a spec. Positional indices and literal text
/// are not captures.
fn format_capture_names(literal: &str) -> Vec<String> {
    let mut names = Vec::new();
    let bytes = literal.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'{' => {
                if bytes.get(index + 1) == Some(&b'{') {
                    index += 2;
                    continue;
                }
                index += 1;
                let start = index;
                while index < bytes.len() && bytes[index] != b'}' && bytes[index] != b':' {
                    index += 1;
                }
                let name = &literal[start..index];
                if is_identifier(name) {
                    names.push(name.to_string());
                }
                while index < bytes.len() && bytes[index] != b'}' {
                    if bytes[index] == b'$' {
                        let end = index;
                        let mut begin = index;
                        while begin > 0
                            && (bytes[begin - 1].is_ascii_alphanumeric()
                                || bytes[begin - 1] == b'_')
                        {
                            begin -= 1;
                        }
                        let name = &literal[begin..end];
                        if is_identifier(name) {
                            names.push(name.to_string());
                        }
                    }
                    index += 1;
                }
                index += 1;
            }
            b'}' => {
                index += 1;
                if bytes.get(index) == Some(&b'}') {
                    index += 1;
                }
            }
            _ => index += 1,
        }
    }
    names.sort();
    names.dedup();
    names
}

fn is_identifier(name: &str) -> bool {
    syn::parse_str::<Ident>(name).is_ok()
}

fn is_trivial(expr: &Expr) -> bool {
    matches!(expr, Expr::Lit(_) | Expr::Path(_) | Expr::Closure(_))
}

fn pattern_names(pat: &Pat) -> Vec<String> {
    let mut names = Vec::new();
    collect_pattern_names(pat, &mut names);
    names
}

fn collect_pattern_names(pat: &Pat, names: &mut Vec<String>) {
    match pat {
        Pat::Ident(ident) => {
            names.push(ident.ident.to_string());
            if let Some((_, subpat)) = &ident.subpat {
                collect_pattern_names(subpat, names);
            }
        }
        Pat::Tuple(tuple) => {
            for elem in &tuple.elems {
                collect_pattern_names(elem, names);
            }
        }
        Pat::TupleStruct(structured) => {
            for elem in &structured.elems {
                collect_pattern_names(elem, names);
            }
        }
        Pat::Struct(structured) => {
            for field in &structured.fields {
                collect_pattern_names(&field.pat, names);
            }
        }
        Pat::Or(or) => {
            for case in &or.cases {
                collect_pattern_names(case, names);
            }
        }
        Pat::Slice(slice) => {
            for elem in &slice.elems {
                collect_pattern_names(elem, names);
            }
        }
        Pat::Type(typed) => collect_pattern_names(&typed.pat, names),
        Pat::Reference(reference) => collect_pattern_names(&reference.pat, names),
        Pat::Paren(paren) => collect_pattern_names(&paren.pat, names),
        _ => {}
    }
}

/// Adapters that are lazy on a standard iterator but incremental on a
/// collection handle. The receiver decides which meaning applies.
fn is_collection_adapter(name: &str) -> bool {
    matches!(name, "map" | "filter" | "flat_map")
}

/// Methods that produce a lazy iterator value.
fn is_iterator_method(name: &str) -> bool {
    matches!(
        name,
        "iter"
            | "iter_mut"
            | "into_iter"
            | "cloned"
            | "copied"
            | "rev"
            | "enumerate"
            | "zip"
            | "chain"
            | "take"
            | "skip"
            | "step_by"
            | "filter_map"
    )
}

/// Iterator and borrow adapters that must not become memoized nodes.
fn is_transparent_method(name: &str) -> bool {
    matches!(
        name,
        "iter"
            | "iter_mut"
            | "into_iter"
            | "cloned"
            | "copied"
            | "rev"
            | "enumerate"
            | "zip"
            | "chain"
            | "take"
            | "skip"
            | "step_by"
            | "as_ref"
            | "as_str"
            | "as_slice"
            | "as_mut"
            | "as_deref"
            | "borrow"
            | "filter_map"
            | "fold"
            | "sum"
            | "product"
            | "count"
            | "collect"
            | "find"
            | "any"
            | "all"
            | "min"
            | "max"
            | "last"
            | "nth"
            | "position"
            | "for_each"
            | "first"
            | "get"
            | "values"
            | "keys"
            | "next"
            | "front"
            | "back"
            | "chars"
            | "as_bytes"
            | "trim"
            | "trim_start"
            | "trim_end"
            | "split"
            | "lines"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use quote::quote;

    fn lowered(body: TokenStream) -> String {
        let block: Block = syn::parse2(body).expect("block");
        Lowerer::new().block(&block).to_string()
    }

    #[test]
    fn a_format_capture_records_the_binding() {
        let tokens = lowered(quote!({
            let name = input.name();
            format!("{name}")
        }));
        assert!(
            tokens.contains("observe_local (__node_name"),
            "a format capture must observe its binding: {tokens}"
        );
    }

    #[test]
    fn a_vec_element_is_lowered() {
        let tokens = lowered(quote!({
            let seed = input.seed();
            vec![seed]
        }));
        assert!(
            tokens.contains("observe_clone (__node_seed"),
            "a vec element must be lowered: {tokens}"
        );
    }

    #[test]
    fn a_struct_update_base_is_lowered() {
        let tokens = lowered(quote!({
            let base = Point {
                x: 1,
                y: input.wide(),
            };
            let point = Point { x: 2, ..base };
            point.y
        }));
        assert!(
            tokens.contains(".. irrigation :: observe_clone (__node_base"),
            "a struct-update base must be lowered: {tokens}"
        );
    }

    #[test]
    fn a_matches_scrutinee_is_lowered() {
        let tokens = lowered(quote!({
            let flag = input.flag();
            matches!(flag, true)
        }));
        assert!(
            tokens.contains("observe_clone (__node_flag"),
            "a matches scrutinee must be lowered: {tokens}"
        );
    }

    #[test]
    fn a_matches_guard_reads_its_pattern_binding() {
        let tokens = lowered(quote!({
            let n = input.wide();
            matches!(input.option(), Some(n) if n > 1)
        }));
        assert!(
            !tokens.contains("observe_clone (__node_n"),
            "a matches guard must see the pattern binding: {tokens}"
        );
    }

    #[test]
    fn a_matches_guard_still_observes_a_visible_local() {
        let tokens = lowered(quote!({
            let limit = input.limit();
            matches!(input.option(), Some(n) if n > limit)
        }));
        assert!(
            tokens.contains("observe_clone (__node_limit"),
            "a matches guard must observe an outer local: {tokens}"
        );
    }

    #[test]
    fn a_match_arm_binding_shadows_a_tracked_local() {
        let tokens = lowered(quote!({
            let n = input.wide();
            match input.narrow() {
                n => n + 1,
            }
        }));
        assert!(
            !tokens.contains("observe_clone (__node_n"),
            "an arm binding must shadow the tracked local: {tokens}"
        );
    }

    #[test]
    fn a_closure_parameter_shadows_a_tracked_local() {
        let tokens = lowered(quote!({
            let n = input.wide();
            let values = vec![1, 2, 3];
            values.iter().map(|n| *n + 1).sum::<u64>()
        }));
        assert!(
            !tokens.contains("observe_clone (__node_n"),
            "a closure parameter must shadow the tracked local: {tokens}"
        );
    }

    #[test]
    fn a_trivial_let_binding_shadows_a_tracked_local() {
        let tokens = lowered(quote!({
            let n = input.wide();
            let n = 5u64;
            n
        }));
        assert!(
            !tokens.contains("observe_clone (__node_n"),
            "a trivial let must shadow the tracked local: {tokens}"
        );
    }

    #[test]
    fn a_visible_tracked_local_is_still_observed() {
        let tokens = lowered(quote!({
            let n = input.wide();
            n + 1
        }));
        assert!(
            tokens.contains("observe_clone (__node_n"),
            "a read outside a binder must observe: {tokens}"
        );
    }

    #[test]
    fn a_bound_iterator_chain_stays_lazy() {
        let tokens = lowered(quote!({
            let values = input.values().to_vec();
            let chain = values.iter();
            chain.map(|value| *value * 2).sum::<u64>()
        }));
        assert_eq!(
            tokens.matches("eval").count(),
            1,
            "only the collection read is memoized; a bound iterator adapter must not \
             become an `eval` node: {tokens}"
        );
    }

    #[test]
    fn a_collection_handle_binding_stays_eager() {
        let tokens = lowered(quote!({
            let handle = input.values();
            handle.map(|value| *value * 2).sum::<u64>()
        }));
        assert_eq!(
            tokens.matches("eval").count(),
            2,
            "a collection adapter on a bound handle is eager work and must stay \
             memoized: {tokens}"
        );
    }
}
