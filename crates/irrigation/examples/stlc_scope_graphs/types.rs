//! Types and the simply typed lambda calculus checking rules.
//!
//! Checking is syntax-directed. A variable reads the binding node the resolver
//! returned: a lambda binding's type is its annotation, a let binding's type is
//! the type of its value term. There is no `TypeEnv`; each `check_term` writes
//! only its own term's `types[term]` and `type_errors[term]` map entries.

use crate::terms::Term;

/// A simply typed lambda calculus type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Type {
    /// The base type of integers.
    Int,
    /// The base type of booleans.
    Bool,
    /// A function type, written `A -> B`.
    Fun(Box<Type>, Box<Type>),
    /// A failed annotation; it poisons its context without cascading.
    Error,
}

impl Type {
    /// Parses a type annotation, e.g. `Int -> Bool -> Int`.
    ///
    /// # Errors
    ///
    /// Returns a message naming the offending byte offset.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut lexer = Lexer {
            bytes: text.as_bytes(),
            at: 0,
        };
        let ty = lexer.ty()?;
        lexer.end()?;
        Ok(ty)
    }

    /// Renders the type with the parentheses a reparse needs.
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Self::Int => String::from("Int"),
            Self::Bool => String::from("Bool"),
            Self::Fun(domain, codomain) => match domain.as_ref() {
                Self::Fun(_, _) => format!("({}) -> {}", domain.render(), codomain.render()),
                _ => format!("{} -> {}", domain.render(), codomain.render()),
            },
            Self::Error => String::from("?"),
        }
    }
}

struct Lexer<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Lexer<'_> {
    fn skip(&mut self) {
        while self.bytes.get(self.at).is_some_and(u8::is_ascii_whitespace) {
            self.at += 1;
        }
    }

    fn word(&mut self) -> Option<&str> {
        self.skip();
        let start = self.at;
        while self.bytes.get(self.at).is_some_and(u8::is_ascii_alphabetic) {
            self.at += 1;
        }
        (self.at > start)
            .then(|| std::str::from_utf8(&self.bytes[start..self.at]).expect("ascii annotation"))
    }

    fn ty(&mut self) -> Result<Type, String> {
        let domain = self.atom()?;
        self.skip();
        if self.bytes.get(self.at) == Some(&b'-') && self.bytes.get(self.at + 1) == Some(&b'>') {
            self.at += 2;
            let codomain = self.ty()?;
            return Ok(Type::Fun(Box::new(domain), Box::new(codomain)));
        }
        Ok(domain)
    }

    fn atom(&mut self) -> Result<Type, String> {
        match self.word() {
            Some("Int") => Ok(Type::Int),
            Some("Bool") => Ok(Type::Bool),
            Some(other) => Err(format!("`{other}` is not a type")),
            None => {
                self.skip();
                if self.bytes.get(self.at) == Some(&b'(') {
                    self.at += 1;
                    let ty = self.ty()?;
                    self.skip();
                    if self.bytes.get(self.at) != Some(&b')') {
                        return Err(String::from("missing `)` in a type"));
                    }
                    self.at += 1;
                    return Ok(ty);
                }
                Err(String::from("expected a type"))
            }
        }
    }

    fn end(&mut self) -> Result<(), String> {
        self.skip();
        if self.at == self.bytes.len() {
            Ok(())
        } else {
            Err(format!(
                "unexpected `{}` after the type",
                self.bytes[self.at..]
                    .iter()
                    .map(|byte| char::from(*byte))
                    .collect::<String>()
            ))
        }
    }
}

use crate::graph::{publish_type_edge, publish_type_scope};
use crate::report::Line;
use crate::{ScopeGraph, Terms};

/// Checks every term once with one memo table, then publishes the type map,
/// canonical type scopes, and `T` edges.
pub fn check_all() {
    let mut memo = std::collections::BTreeMap::new();
    for term in Terms.nodes().keys() {
        let _ = check(term, &mut memo);
    }
    let mut labels = std::collections::BTreeSet::new();
    for (term, ty) in &memo {
        ScopeGraph.define_types(*term, ty.clone());
        labels.insert(ty.render());
        if matches!(ty, Type::Error) {
            ScopeGraph.define_type_errors(*term, Line(format!("term #{term:x} is ill-typed")));
        } else {
            ScopeGraph
                .type_errors()
                .remove_if_present(*term, "typing_edge");
        }
    }
    for label in &labels {
        publish_type_scope(ScopeGraph.graph(), label);
    }
    for (term, ty) in &memo {
        publish_type_edge(ScopeGraph.graph(), *term, &ty.render());
    }
}

/// Checks one term, deriving a variable's type from its binding.
fn check(term: u64, memo: &mut std::collections::BTreeMap<u64, Type>) -> Type {
    if let Some(ty) = memo.get(&term) {
        return ty.clone();
    }
    let ty = match Terms.nodes().get(&term) {
        Some(Term::Var) => match ScopeGraph.targets().get(&term) {
            Some(binding) => match Terms.nodes().get(&binding) {
                Some(Term::Lam { .. }) => {
                    let text = Terms.annotations().get(&binding).unwrap_or_default();
                    Type::parse(&text).unwrap_or(Type::Error)
                }
                Some(Term::Let { binding: value, .. }) => check(value, memo),
                _ => Type::Error,
            },
            None => Type::Error,
        },
        Some(Term::Lam { body }) => {
            let text = Terms.annotations().get(&term).unwrap_or_default();
            let domain = Type::parse(&text).unwrap_or(Type::Error);
            let codomain = check(body, memo);
            if matches!(domain, Type::Error) || matches!(codomain, Type::Error) {
                Type::Error
            } else {
                Type::Fun(Box::new(domain), Box::new(codomain))
            }
        }
        Some(Term::Let { binding, body }) => {
            let _ = check(binding, memo);
            check(body, memo)
        }
        Some(Term::App { func, arg }) => {
            let function = check(func, memo);
            let argument = check(arg, memo);
            match (function, argument) {
                (Type::Fun(domain, codomain), argument) => {
                    if domain.as_ref() == &argument {
                        *codomain
                    } else {
                        Type::Error
                    }
                }
                _ => Type::Error,
            }
        }
        Some(Term::Int) => Type::Int,
        Some(Term::Bool) => Type::Bool,
        Some(Term::If { cond, yes, no }) => {
            let condition = check(cond, memo);
            let yes = check(yes, memo);
            let no = check(no, memo);
            if condition == Type::Bool && yes == no && !matches!(yes, Type::Error) {
                yes
            } else {
                Type::Error
            }
        }
        None => Type::Error,
    };
    memo.insert(term, ty.clone());
    ty
}

#[cfg(test)]
mod tests {
    use super::Type;

    #[test]
    fn types_parse_and_render_without_losing_grouping() {
        for text in ["Int", "Bool", "Int -> Bool", "(Int -> Bool) -> Int"] {
            let ty = Type::parse(text).expect("type");
            assert_eq!(ty.render(), text);
        }
        assert_eq!(
            Type::parse("Int -> Bool -> Int").expect("type"),
            Type::Fun(
                Box::new(Type::Int),
                Box::new(Type::Fun(Box::new(Type::Bool), Box::new(Type::Int)))
            )
        );
    }

    #[test]
    fn a_bad_annotation_is_an_error() {
        assert!(Type::parse("Intty").is_err());
        assert!(Type::parse("(Int ->").is_err());
    }
}
