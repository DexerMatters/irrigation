//! Guard expressions and state actions for lexer rules.
//!
//! The language is deliberately total and side-effect free. It reads the
//! current column, value-stack depth, top value, lexeme, named captures, and
//! EOF; it cannot call Rust or inspect the rest of the document. That is what
//! makes the lexer state serializable and incremental.

use std::fmt;
use std::sync::Arc;

/// A value on the lexer's auxiliary stack.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum LexValue {
    /// No value.
    Unit,
    /// A signed integer, used by indentation-like grammars.
    Int(i64),
    /// A boolean.
    Bool(bool),
    /// Interned text, used by tag matching and similar state.
    Text(Arc<str>),
}

impl LexValue {
    /// Returns the text payload.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }

    /// Returns the integer payload.
    #[must_use]
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(value) => Some(*value),
            _ => None,
        }
    }

    /// Returns the boolean payload.
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }
}

impl From<&str> for LexValue {
    fn from(value: &str) -> Self {
        Self::Text(Arc::from(value))
    }
}

impl From<String> for LexValue {
    fn from(value: String) -> Self {
        Self::Text(Arc::from(value))
    }
}

impl From<i64> for LexValue {
    fn from(value: i64) -> Self {
        Self::Int(value)
    }
}

impl From<bool> for LexValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

/// Guard expression.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GuardExpr {
    /// Zero-based current column.
    Column,
    /// Value-stack depth.
    Depth,
    /// The matched lexeme.
    Lexeme,
    /// A named capture's text.
    Capture(String),
    /// Top of the value stack, or `Unit`.
    TopValue,
    /// Whether the lexer is at EOF.
    Eof,
    /// Integer literal.
    Int(i64),
    /// String literal.
    Text(Arc<str>),
    /// Boolean literal.
    Bool(bool),
    /// Logical negation.
    Not(Box<GuardExpr>),
    /// Logical conjunction.
    And(Box<GuardExpr>, Box<GuardExpr>),
    /// Logical disjunction.
    Or(Box<GuardExpr>, Box<GuardExpr>),
    /// Equality.
    Eq(Box<GuardExpr>, Box<GuardExpr>),
    /// Inequality.
    Ne(Box<GuardExpr>, Box<GuardExpr>),
    /// Less than.
    Lt(Box<GuardExpr>, Box<GuardExpr>),
    /// Less than or equal.
    Le(Box<GuardExpr>, Box<GuardExpr>),
    /// Greater than.
    Gt(Box<GuardExpr>, Box<GuardExpr>),
    /// Greater than or equal.
    Ge(Box<GuardExpr>, Box<GuardExpr>),
    /// Addition.
    Add(Box<GuardExpr>, Box<GuardExpr>),
    /// Subtraction.
    Sub(Box<GuardExpr>, Box<GuardExpr>),
    /// Multiplication.
    Mul(Box<GuardExpr>, Box<GuardExpr>),
    /// Division.
    Div(Box<GuardExpr>, Box<GuardExpr>),
}

impl GuardExpr {
    /// Parses a guard expression.
    ///
    /// # Errors
    ///
    /// Returns a structured parse error.
    pub fn parse(source: &str) -> Result<Self, ExprError> {
        let mut parser = ExprParser::new(source);
        let expr = parser.parse_or()?;
        parser.skip_ws();
        if !parser.at_end() {
            return Err(parser.error("unexpected trailing expression characters"));
        }
        Ok(expr)
    }

    /// Returns `true` when any subexpression observes the current column.
    #[must_use]
    pub fn uses_column(&self) -> bool {
        match self {
            Self::Column => true,
            Self::Capture(_)
            | Self::Depth
            | Self::Lexeme
            | Self::TopValue
            | Self::Eof
            | Self::Int(_)
            | Self::Text(_)
            | Self::Bool(_) => false,
            Self::Not(inner) => inner.uses_column(),
            Self::And(left, right)
            | Self::Or(left, right)
            | Self::Eq(left, right)
            | Self::Ne(left, right)
            | Self::Lt(left, right)
            | Self::Le(left, right)
            | Self::Gt(left, right)
            | Self::Ge(left, right)
            | Self::Add(left, right)
            | Self::Sub(left, right)
            | Self::Mul(left, right)
            | Self::Div(left, right) => left.uses_column() || right.uses_column(),
        }
    }

    /// Evaluates the expression in a lexer context.
    ///
    /// # Errors
    ///
    /// Returns an error for a type mismatch or a missing value.
    pub fn eval(&self, cx: &EvalContext<'_>) -> Result<LexValue, ExprError> {
        match self {
            Self::Column => Ok(LexValue::Int(i64::from(cx.column))),
            Self::Depth => Ok(LexValue::Int(cx.values.len() as i64)),
            Self::Lexeme => Ok(LexValue::Text(Arc::from(cx.lexeme))),
            Self::Capture(name) => {
                let index = cx
                    .capture_names
                    .iter()
                    .position(|item| item == name)
                    .ok_or_else(|| ExprError::new(format!("unknown capture `${name}`"), 0))?;
                let range = cx
                    .capture_ranges
                    .get(index)
                    .copied()
                    .flatten()
                    .ok_or_else(|| ExprError::new(format!("capture `${name}` did not match"), 0))?;
                let text = cx
                    .text
                    .get(range.0..range.1)
                    .ok_or_else(|| ExprError::new("capture range is outside the token", 0))?;
                Ok(LexValue::Text(Arc::from(text)))
            }
            Self::TopValue => Ok(cx.values.last().cloned().unwrap_or(LexValue::Unit)),
            Self::Eof => Ok(LexValue::Bool(cx.eof)),
            Self::Int(value) => Ok(LexValue::Int(*value)),
            Self::Text(value) => Ok(LexValue::Text(Arc::clone(value))),
            Self::Bool(value) => Ok(LexValue::Bool(*value)),
            Self::Not(inner) => Ok(LexValue::Bool(!as_bool(inner.eval(cx)?)?)),
            Self::And(left, right) => {
                let left = as_bool(left.eval(cx)?)?;
                if left {
                    Ok(LexValue::Bool(as_bool(right.eval(cx)?)?))
                } else {
                    Ok(LexValue::Bool(false))
                }
            }
            Self::Or(left, right) => {
                let left = as_bool(left.eval(cx)?)?;
                if left {
                    Ok(LexValue::Bool(true))
                } else {
                    Ok(LexValue::Bool(as_bool(right.eval(cx)?)?))
                }
            }
            Self::Eq(left, right) => Ok(LexValue::Bool(left.eval(cx)? == right.eval(cx)?)),
            Self::Ne(left, right) => Ok(LexValue::Bool(left.eval(cx)? != right.eval(cx)?)),
            Self::Lt(left, right) => compare(cx, left, right, |a, b| a < b),
            Self::Le(left, right) => compare(cx, left, right, |a, b| a <= b),
            Self::Gt(left, right) => compare(cx, left, right, |a, b| a > b),
            Self::Ge(left, right) => compare(cx, left, right, |a, b| a >= b),
            Self::Add(left, right) => arithmetic(cx, left, right, |a, b| a + b),
            Self::Sub(left, right) => arithmetic(cx, left, right, |a, b| a - b),
            Self::Mul(left, right) => arithmetic(cx, left, right, |a, b| a * b),
            Self::Div(left, right) => {
                let a = as_int(left.eval(cx)?)?;
                let b = as_int(right.eval(cx)?)?;
                if b == 0 {
                    return Err(ExprError::new("division by zero", 0));
                }
                Ok(LexValue::Int(a / b))
            }
        }
    }

    /// Evaluates the expression as a boolean guard.
    ///
    /// # Errors
    ///
    /// Returns an error for a type mismatch.
    pub fn eval_bool(&self, cx: &EvalContext<'_>) -> Result<bool, ExprError> {
        as_bool(self.eval(cx)?)
    }
}

fn as_bool(value: LexValue) -> Result<bool, ExprError> {
    value
        .as_bool()
        .ok_or_else(|| ExprError::new("guard expression is not boolean", 0))
}

fn as_int(value: LexValue) -> Result<i64, ExprError> {
    value
        .as_int()
        .ok_or_else(|| ExprError::new("expression is not an integer", 0))
}

fn compare(
    cx: &EvalContext<'_>,
    left: &GuardExpr,
    right: &GuardExpr,
    f: impl Fn(i64, i64) -> bool,
) -> Result<LexValue, ExprError> {
    let left = left.eval(cx)?;
    let right = right.eval(cx)?;
    match (left, right) {
        (LexValue::Int(a), LexValue::Int(b)) => Ok(LexValue::Bool(f(a, b))),
        (LexValue::Text(a), LexValue::Text(b)) => Ok(LexValue::Bool(f(
            i64::from(a.as_bytes().first().copied().unwrap_or(0)),
            i64::from(b.as_bytes().first().copied().unwrap_or(0)),
        ))),
        _ => Err(ExprError::new(
            "comparison requires two integers or two texts",
            0,
        )),
    }
}

fn arithmetic(
    cx: &EvalContext<'_>,
    left: &GuardExpr,
    right: &GuardExpr,
    f: impl Fn(i64, i64) -> i64,
) -> Result<LexValue, ExprError> {
    let a = as_int(left.eval(cx)?)?;
    let b = as_int(right.eval(cx)?)?;
    Ok(LexValue::Int(f(a, b)))
}

/// Context for guard evaluation.
#[derive(Clone, Copy, Debug)]
pub struct EvalContext<'a> {
    /// Current zero-based column.
    pub column: u32,
    /// Text of the matched lexeme.
    pub text: &'a str,
    /// Full matched lexeme.
    pub lexeme: &'a str,
    /// Capture names in index order.
    pub capture_names: &'a [String],
    /// Capture ranges in the matched lexeme.
    pub capture_ranges: &'a [Option<(usize, usize)>],
    /// Value stack.
    pub values: &'a [LexValue],
    /// Whether the lexer is at EOF.
    pub eof: bool,
}

/// Statement action after a token is emitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Keep the current mode and value stack.
    Stay,
    /// Switch to a named mode.
    Switch(String),
    /// Push the current mode and enter a named mode.
    Push(String),
    /// Restore the previous mode.
    Pop,
    /// Push a value.
    PushValue(GuardExpr),
    /// Replace the top value.
    SetValue(GuardExpr),
    /// Pop a value.
    PopValue,
    /// Emit an error token with a message.
    Error(String),
}

impl Action {
    /// Parses a comma-separated action list (the text after `->`).
    ///
    /// # Errors
    ///
    /// Returns a structured parse error.
    pub fn parse_list(source: &str) -> Result<Vec<Self>, ExprError> {
        let mut actions = Vec::new();
        for part in split_top_level(source, ',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            actions.push(Self::parse(part)?);
        }
        Ok(actions)
    }

    /// Parses one action.
    ///
    /// # Errors
    ///
    /// Returns a structured parse error.
    pub fn parse(source: &str) -> Result<Self, ExprError> {
        let source = source.trim();
        if source == "stay" {
            return Ok(Self::Stay);
        }
        if source == "pop" {
            return Ok(Self::Pop);
        }
        if source == "pop_value" {
            return Ok(Self::PopValue);
        }
        if let Some(rest) = source.strip_prefix("switch") {
            let name = parse_call_ident(rest)?;
            return Ok(Self::Switch(name));
        }
        if let Some(rest) = source.strip_prefix("push_value") {
            let inner = parse_call_expr(rest)?;
            return Ok(Self::PushValue(inner));
        }
        if let Some(rest) = source.strip_prefix("set_value") {
            let inner = parse_call_expr(rest)?;
            return Ok(Self::SetValue(inner));
        }
        if let Some(rest) = source.strip_prefix("push") {
            let name = parse_call_ident(rest)?;
            return Ok(Self::Push(name));
        }
        if let Some(rest) = source.strip_prefix("error") {
            let inner = parse_call_string(rest)?;
            return Ok(Self::Error(inner));
        }
        Err(ExprError::new(format!("unknown action `{source}`"), 0))
    }
}

fn parse_call_ident(source: &str) -> Result<String, ExprError> {
    let source = source.trim();
    let Some(inner) = source.strip_prefix('(').and_then(|s| s.strip_suffix(')')) else {
        return Err(ExprError::new("expected `(name)`", 0));
    };
    let name = inner.trim();
    if name.is_empty() || !name.chars().all(|c| c == '_' || c.is_alphanumeric()) {
        return Err(ExprError::new("expected a mode name", 0));
    }
    Ok(name.to_string())
}

fn parse_call_expr(source: &str) -> Result<GuardExpr, ExprError> {
    let source = source.trim();
    let Some(inner) = source.strip_prefix('(').and_then(|s| s.strip_suffix(')')) else {
        return Err(ExprError::new("expected `(expression)`", 0));
    };
    GuardExpr::parse(inner)
}

fn parse_call_string(source: &str) -> Result<String, ExprError> {
    let source = source.trim();
    let Some(inner) = source.strip_prefix('(').and_then(|s| s.strip_suffix(')')) else {
        return Err(ExprError::new("expected `(\"message\")`", 0));
    };
    let inner = inner.trim();
    let Some(text) = inner.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else {
        return Err(ExprError::new("error action requires a string", 0));
    };
    Ok(text.replace("\\\"", "\"").replace("\\\\", "\\"))
}

fn split_top_level(source: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for (index, c) in source.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '(' => depth += 1,
            ')' => depth -= 1,
            _ if c == separator && depth == 0 => {
                parts.push(&source[start..index]);
                start = index + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&source[start..]);
    parts
}

/// Expression parse or evaluation error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExprError {
    /// Message.
    pub message: String,
    /// Byte offset in the expression text.
    pub offset: usize,
}

impl ExprError {
    /// Builds an error.
    #[must_use]
    pub fn new(message: impl Into<String>, offset: usize) -> Self {
        Self {
            message: message.into(),
            offset,
        }
    }
}

impl fmt::Display for ExprError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at expression offset {}", self.message, self.offset)
    }
}

impl std::error::Error for ExprError {}

struct ExprParser<'a> {
    source: &'a str,
    index: usize,
}

impl<'a> ExprParser<'a> {
    fn new(source: &'a str) -> Self {
        Self { source, index: 0 }
    }

    fn parse_or(&mut self) -> Result<GuardExpr, ExprError> {
        let mut expr = self.parse_and()?;
        while self.eat_word("or") {
            let right = self.parse_and()?;
            expr = GuardExpr::Or(Box::new(expr), Box::new(right));
        }
        Ok(expr)
    }

    fn parse_and(&mut self) -> Result<GuardExpr, ExprError> {
        let mut expr = self.parse_cmp()?;
        while self.eat_word("and") {
            let right = self.parse_cmp()?;
            expr = GuardExpr::And(Box::new(expr), Box::new(right));
        }
        Ok(expr)
    }

    fn parse_cmp(&mut self) -> Result<GuardExpr, ExprError> {
        let left = self.parse_add()?;
        let Some(op) = self.eat_any(&["==", "!=", "<=", ">=", "<", ">"]) else {
            return Ok(left);
        };
        let right = self.parse_add()?;
        Ok(match op {
            "==" => GuardExpr::Eq(Box::new(left), Box::new(right)),
            "!=" => GuardExpr::Ne(Box::new(left), Box::new(right)),
            "<" => GuardExpr::Lt(Box::new(left), Box::new(right)),
            "<=" => GuardExpr::Le(Box::new(left), Box::new(right)),
            ">" => GuardExpr::Gt(Box::new(left), Box::new(right)),
            ">=" => GuardExpr::Ge(Box::new(left), Box::new(right)),
            _ => unreachable!("known comparator"),
        })
    }

    fn parse_add(&mut self) -> Result<GuardExpr, ExprError> {
        let mut expr = self.parse_mul()?;
        while let Some(op) = self.eat_any(&["+", "-"]) {
            let right = self.parse_mul()?;
            expr = if op == "+" {
                GuardExpr::Add(Box::new(expr), Box::new(right))
            } else {
                GuardExpr::Sub(Box::new(expr), Box::new(right))
            };
        }
        Ok(expr)
    }

    fn parse_mul(&mut self) -> Result<GuardExpr, ExprError> {
        let mut expr = self.parse_unary()?;
        while let Some(op) = self.eat_any(&["*", "/"]) {
            let right = self.parse_unary()?;
            expr = if op == "*" {
                GuardExpr::Mul(Box::new(expr), Box::new(right))
            } else {
                GuardExpr::Div(Box::new(expr), Box::new(right))
            };
        }
        Ok(expr)
    }

    fn parse_unary(&mut self) -> Result<GuardExpr, ExprError> {
        if self.eat_word("not") {
            return Ok(GuardExpr::Not(Box::new(self.parse_unary()?)));
        }
        if self.eat('-') {
            return Ok(GuardExpr::Sub(
                Box::new(GuardExpr::Int(0)),
                Box::new(self.parse_unary()?),
            ));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<GuardExpr, ExprError> {
        self.skip_ws();
        if self.eat('(') {
            let expr = self.parse_or()?;
            if !self.eat(')') {
                return Err(self.error("expected `)`"));
            }
            return Ok(expr);
        }
        if self.eat('$') {
            let name = self.parse_ident()?;
            return Ok(GuardExpr::Capture(name));
        }
        if self.peek() == Some('"') {
            return Ok(GuardExpr::Text(Arc::from(self.parse_string()?)));
        }
        if self.peek().is_some_and(|c| c.is_ascii_digit()) {
            let start = self.index;
            while self.peek().is_some_and(|c| c.is_ascii_digit() || c == '_') {
                self.bump();
            }
            let raw = self.source[start..self.index].replace('_', "");
            let value = raw
                .parse::<i64>()
                .map_err(|_| self.error("invalid integer literal"))?;
            return Ok(GuardExpr::Int(value));
        }
        let word = self.parse_ident()?;
        match word.as_str() {
            "column" => Ok(GuardExpr::Column),
            "depth" => Ok(GuardExpr::Depth),
            "lexeme" => Ok(GuardExpr::Lexeme),
            "top_value" => Ok(GuardExpr::TopValue),
            "eof" => Ok(GuardExpr::Eof),
            "true" => Ok(GuardExpr::Bool(true)),
            "false" => Ok(GuardExpr::Bool(false)),
            other => Err(self.error(format!("unknown expression name `{other}`"))),
        }
    }

    fn parse_ident(&mut self) -> Result<String, ExprError> {
        self.skip_ws();
        let start = self.index;
        while let Some(c) = self.peek() {
            if c == '_' || c.is_alphanumeric() {
                self.bump();
            } else {
                break;
            }
        }
        if start == self.index {
            return Err(self.error("expected an identifier"));
        }
        Ok(self.source[start..self.index].to_string())
    }

    fn parse_string(&mut self) -> Result<String, ExprError> {
        if !self.eat('"') {
            return Err(self.error("expected a string"));
        }
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if c == '"' {
                self.bump();
                return Ok(out);
            }
            if c == '\\' {
                self.bump();
                let Some(escaped) = self.peek() else {
                    return Err(self.error("trailing escape in string"));
                };
                self.bump();
                out.push(match escaped {
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    '\\' => '\\',
                    '"' => '"',
                    other => other,
                });
            } else {
                out.push(c);
                self.bump();
            }
        }
        Err(self.error("unterminated string"))
    }

    fn eat_word(&mut self, word: &str) -> bool {
        self.skip_ws();
        let rest = &self.source[self.index..];
        if !rest.starts_with(word) {
            return false;
        }
        let after = rest[word.len()..].chars().next();
        if after.is_some_and(|c| c == '_' || c.is_alphanumeric()) {
            return false;
        }
        self.index += word.len();
        true
    }

    fn eat_any(&mut self, options: &[&str]) -> Option<&'static str> {
        self.skip_ws();
        for option in options {
            if self.source[self.index..].starts_with(option) {
                self.index += option.len();
                return Some(match *option {
                    "==" => "==",
                    "!=" => "!=",
                    "<=" => "<=",
                    ">=" => ">=",
                    "<" => "<",
                    ">" => ">",
                    "+" => "+",
                    "-" => "-",
                    "*" => "*",
                    "/" => "/",
                    _ => unreachable!("known operator"),
                });
            }
        }
        None
    }

    fn eat(&mut self, expected: char) -> bool {
        self.skip_ws();
        if self.peek() == Some(expected) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn peek(&self) -> Option<char> {
        self.source[self.index..].chars().next()
    }

    fn bump(&mut self) {
        if let Some(c) = self.peek() {
            self.index += c.len_utf8();
        }
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.bump();
        }
    }

    fn at_end(&self) -> bool {
        self.index >= self.source.len()
    }

    fn error(&self, message: impl Into<String>) -> ExprError {
        ExprError::new(message, self.index)
    }
}

#[cfg(test)]
mod tests {
    use super::{Action, EvalContext, GuardExpr, LexValue};

    #[test]
    fn parses_and_evaluates_comparisons() {
        let expr = GuardExpr::parse("$name == top_value and column > 0").unwrap();
        let ranges = [Some((0, 3))];
        let values = [LexValue::from("abc")];
        let cx = EvalContext {
            column: 4,
            text: "abc",
            lexeme: "abc",
            capture_names: &["name".to_string()],
            capture_ranges: &ranges,
            values: &values,
            eof: false,
        };
        assert!(expr.eval_bool(&cx).unwrap());
    }

    #[test]
    fn top_value_is_unit_when_empty() {
        let expr = GuardExpr::parse("top_value == \"x\"").unwrap();
        let cx = EvalContext {
            column: 0,
            text: "",
            lexeme: "",
            capture_names: &[],
            capture_ranges: &[],
            values: &[],
            eof: false,
        };
        assert!(!expr.eval_bool(&cx).unwrap());
    }

    #[test]
    fn parses_actions() {
        let actions = Action::parse_list("push_value($name), switch(tag)").unwrap();
        assert_eq!(actions.len(), 2);
        assert!(matches!(actions[0], Action::PushValue(_)));
        assert_eq!(actions[1], Action::Switch("tag".to_string()));
    }

    #[test]
    fn rejects_unknown_names() {
        assert!(GuardExpr::parse("bogus").is_err());
        assert!(Action::parse("nope").is_err());
    }
}
