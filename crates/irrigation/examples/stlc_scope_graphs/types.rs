//! Types.
//!
//! A type is ordinary data: equality is structural, and its rendered form is the
//! key the checker uses to share one type scope between every term of that type.

/// A simply typed lambda calculus type.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
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
    /// Renders the type with the parentheses a reader needs.
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

#[cfg(test)]
mod tests {
    use super::Type;

    #[test]
    fn renders_without_losing_grouping() {
        let function = Type::Fun(
            Box::new(Type::Fun(Box::new(Type::Int), Box::new(Type::Bool))),
            Box::new(Type::Int),
        );
        assert_eq!(function.render(), "(Int -> Bool) -> Int");
        assert_eq!(
            Type::Fun(Box::new(Type::Int), Box::new(Type::Bool)).render(),
            "Int -> Bool"
        );
    }
}
