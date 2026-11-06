//! Expression lowering.
//!
//! The macro turns a Rust-shaped component body into tracked primitives:
//!
//! * every value-producing expression becomes a `reactive::eval(site, ...)` node;
//! * every local binding remembers the node that produced its value;
//! * every use of such a local observes that node, so a primitive that consumes
//!   a local still depends on the computation that produced it.
//!
//! Constructs the lowering cannot preserve are rejected with a source-level
//! diagnostic rather than silently made coarse.

use proc_macro2::TokenStream;
use quote::{ToTokens, format_ident, quote};
use syn::{Block, Expr, ExprClosure, ExprMacro, Pat, Stmt};

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
    scopes: Vec<std::collections::HashMap<String, TokenStream>>,
    /// Inside a closure body no per-expression nodes are created: the closure
    /// may be called many times in one scope (for example by an iterator
    /// adapter), so per-call node identity cannot be assigned syntactically.
    /// Tracked reads still record on the enclosing computation.
    inline_depth: usize,
}

impl Lowerer {
    /// Builds a lowerer for one component function.
    #[must_use]
    pub fn new() -> Self {
        Self {
            counter: 0,
            strict: false,
            allowed: Vec::new(),
            scopes: vec![std::collections::HashMap::new()],
            inline_depth: 0,
        }
    }

    fn node_or_inline(&mut self, body: TokenStream) -> Lowered {
        if self.inline_depth > 0 {
            return Lowered::plain(body);
        }
        let site = self.site();
        Lowered::node(
            quote!(reactive::eval(#site, || #body)),
            quote!(reactive::node_of(#site)),
        )
    }

    fn site(&mut self) -> TokenStream {
        let counter = self.counter;
        self.counter += 1;
        // Sequential per component; the high half is reserved for the macro so
        // operator-defined site ids (1, 2, ...) can never collide with it.
        quote! {
            reactive::SiteId(0x8000_0000u32 | (#counter as u32))
        }
    }

    /// Sets the strict-mode policy for opaque calls.
    pub fn set_policy(&mut self, strict: bool, allowed: Vec<String>) {
        self.strict = strict;
        self.allowed = allowed;
    }

    fn is_declared_primitive(&self, name: &str) -> bool {
        if self
            .allowed
            .iter()
            .any(|allowed| name == allowed || name.ends_with(&format!("::{allowed}")))
        {
            return true;
        }
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
        PURE.contains(&name)
    }

    fn error<T: ToTokens>(&self, tokens: T, message: &str) -> Lowered {
        Lowered::plain(syn::Error::new_spanned(tokens, message).to_compile_error())
    }

    fn lookup(&self, name: &str) -> Option<TokenStream> {
        for scope in self.scopes.iter().rev() {
            if let Some(node) = scope.get(name) {
                return Some(node.clone());
            }
        }
        None
    }

    fn bind(&mut self, name: String, node: TokenStream) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name, node);
        }
    }

    /// Lowers a block.
    pub fn block(&mut self, block: &Block) -> TokenStream {
        self.scopes.push(std::collections::HashMap::new());
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
                        return self
                            .error(
                                &local.pat,
                                "`let mut` is not supported in a component body; build the value \
                                 functionally instead",
                            )
                            .tokens;
                    }
                }
                let pat = &local.pat;
                let Some(init) = &local.init else {
                    return quote!(let #pat;);
                };
                if init.diverge.is_some() {
                    return self
                        .error(
                            &init.expr,
                            "`let ... else` is not supported in a component body",
                        )
                        .tokens;
                }
                let lowered = self.expr_node(&init.expr);
                if self.inline_depth > 0 {
                    let tokens = lowered.tokens;
                    return quote!(let #pat = #tokens;);
                }
                if lowered.iterator {
                    let tokens = lowered.tokens;
                    return quote!(let #pat = #tokens;);
                }
                let (tokens, node) = match (&lowered.node, is_trivial(&init.expr)) {
                    (Some(node), _) => (lowered.tokens, Some(node.clone())),
                    (None, true) => (lowered.tokens, None),
                    (None, false) => {
                        let site = self.site();
                        let inner = lowered.tokens;
                        let tokens = quote!(reactive::eval(#site, || #inner));
                        (tokens, Some(quote!(reactive::node_of(#site))))
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
                let wrapped = lowered.node.is_none()
                    && self.inline_depth == 0
                    && matches!(expr, Expr::If(_) | Expr::Match(_) | Expr::Block(_));
                if wrapped {
                    let site = self.site();
                    tokens = quote!(reactive::eval(#site, || #tokens));
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
                self.error(item, "nested items are not supported in a component body")
                    .tokens
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
                    if let Some(node) = self.lookup(&name) {
                        let ident = &path.path.segments[0].ident;
                        let tokens = quote!(reactive::observe_clone(#node, &#ident));
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
                let iterator_chain =
                    is_collection_adapter(&method_name) && receiver_is_iterator(&call.receiver);
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
                    return self.error(
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
                let rest = structure.rest.as_ref().map(|rest| quote!(.. #rest));
                self.node_or_inline(quote!(#path { #(#fields,)* #rest }))
            }
            Expr::If(if_expr) => {
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
                    .map(|node| quote!(reactive::observe_local(#node);));
                let scrutinee_tokens = &scrutinee.tokens;
                let arms: Vec<TokenStream> = match_expr
                    .arms
                    .iter()
                    .map(|arm| {
                        let (pattern, guard) = match &arm.pat {
                            Pat::Guard(guard) => {
                                let condition = self.expr(&guard.guard);
                                ((*guard.pat).clone(), quote!(if #condition))
                            }
                            other => (other.clone(), quote!()),
                        };
                        let body = self.expr(&arm.body);
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
                    return self.error(
                        left,
                        "only local variables can be assigned inside a component",
                    );
                }
                if let Expr::Path(path) = &**left {
                    if path.qself.is_none() && path.path.segments.len() == 1 {
                        let name = path.path.segments[0].ident.to_string();
                        if self.lookup(&name).is_some() {
                            return self.error(
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
            Expr::Closure(closure) => self.error(
                closure,
                "closures are only supported as arguments to collection operators \
                 (`map`, `filter`, `flat_map`)",
            ),
            Expr::Await(await_expr) => {
                self.error(await_expr, "`.await` is not supported inside a component")
            }
            Expr::Async(async_expr) => self.error(
                async_expr,
                "async blocks are not supported inside a component",
            ),
            Expr::Unsafe(unsafe_expr) => {
                self.error(unsafe_expr, "`unsafe` is not supported inside a component")
            }
            Expr::Return(return_expr) => self.error(
                return_expr,
                "`return` is not supported inside a component body; \
                 use the final expression of a block",
            ),
            Expr::Break(break_expr) => self.error(break_expr, "`break` is not supported"),
            Expr::Continue(continue_expr) => {
                self.error(continue_expr, "`continue` is not supported")
            }
            Expr::While(while_expr) => self.error(
                while_expr,
                "`while` loops are not supported; use a collection operator",
            ),
            Expr::Loop(loop_expr) => self.error(
                loop_expr,
                "`loop` is not supported; use a collection operator",
            ),
            Expr::ForLoop(for_expr) => self.error(
                for_expr,
                "`for` loops are not supported; use `map`, `filter`, or `flat_map`",
            ),
            Expr::Let(let_expr) => self.error(let_expr, "`let` expressions are not supported"),
            Expr::Infer(infer) => {
                self.error(infer, "type inference placeholders are not supported")
            }
            Expr::Const(const_block) => self.error(
                const_block,
                "const blocks are not supported inside a component",
            ),
            other => self.error(other, "unsupported expression in a component body"),
        }
    }

    /// Lowers a receiver that is only borrowed, avoiding a temporary clone.
    fn borrowed_receiver(&mut self, receiver: &Expr) -> TokenStream {
        if let Expr::Path(path) = receiver {
            if path.qself.is_none() && path.path.segments.len() == 1 {
                let name = path.path.segments[0].ident.to_string();
                if let Some(node) = self.lookup(&name) {
                    let ident = &path.path.segments[0].ident;
                    return quote!(reactive::observe_ref(#node, &#ident));
                }
            }
        }
        self.expr(receiver)
    }

    fn argument(&mut self, arg: &Expr) -> TokenStream {
        match arg {
            Expr::Closure(closure) => self.closure(closure),
            other => self.expr(other),
        }
    }

    fn closure(&mut self, closure: &ExprClosure) -> TokenStream {
        self.inline_depth += 1;
        self.scopes.push(std::collections::HashMap::new());
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
        let allowed = matches!(
            name.as_str(),
            "vec" | "format" | "matches" | "concat" | "stringify" | "assert" | "debug_assert"
        );
        if !allowed {
            return self.error(
                mac,
                "this macro is not supported inside a component; \
                 effects belong in observers, not computations",
            );
        }
        let tokens = mac.to_token_stream();
        self.node_or_inline(tokens)
    }
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
        Pat::Ident(ident) => names.push(ident.ident.to_string()),
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

/// Returns `true` when the receiver expression is itself an iterator chain.
fn receiver_is_iterator(receiver: &Expr) -> bool {
    match receiver {
        Expr::MethodCall(call) => is_iterator_method(&call.method.to_string()),
        Expr::Call(call) => call
            .func
            .to_token_stream()
            .to_string()
            .contains("into_iter"),
        _ => false,
    }
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
