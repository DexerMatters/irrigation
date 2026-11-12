//! Runtime values and the evaluator's outcome.
//!
//! Values are dynamic: a lambda parameter's value comes from the application and
//! a closure captures its environment, so the environment is an ordinary
//! argument of the evaluator, never a View.

use irrigation::TreeNodeId;

use crate::report::Line;
use crate::syntax::TermRef;

/// How deep a run may recurse before it stops.
pub const DEPTH_LIMIT: u32 = 512;

/// A value environment, keyed by the binding term that introduced each value.
pub type Env = Vec<(TreeNodeId, Value)>;

/// A runtime value.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Value {
    /// An integer.
    Int(i64),
    /// A boolean.
    Bool(bool),
    /// A closure over its parameter binding, body, and environment.
    Closure {
        /// The binding term's parser identity.
        binder: TreeNodeId,
        /// The body term.
        body: TermRef,
        /// The captured environment.
        env: Env,
    },
}

impl Value {
    fn text(&self) -> String {
        match self {
            Self::Int(value) => value.to_string(),
            Self::Bool(value) => value.to_string(),
            Self::Closure { .. } => String::from("λ"),
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Int(_) => "Int",
            Self::Bool(_) => "Bool",
            Self::Closure { .. } => "function",
        }
    }
}

/// What evaluating one term produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    /// The value, when evaluation produced one.
    pub value: Option<Value>,
    /// Beta and `let` steps performed.
    pub steps: u64,
    /// Why evaluation did not produce a value, if it did not.
    pub diagnostics: Vec<Line>,
}

impl Outcome {
    /// An outcome holding `value`.
    #[must_use]
    pub fn value(value: Value) -> Self {
        Self {
            value: Some(value),
            steps: 0,
            diagnostics: Vec::new(),
        }
    }

    /// An outcome that produced no value, for `message`.
    #[must_use]
    pub fn stuck(message: String) -> Self {
        Self {
            value: None,
            steps: 0,
            diagnostics: vec![Line(message)],
        }
    }

    /// Adds the steps a subterm performed.
    #[must_use]
    pub fn plus_steps(mut self, steps: u64) -> Self {
        self.steps = self.steps.saturating_add(steps);
        self
    }

    /// Prepends a subterm's diagnostics.
    #[must_use]
    pub fn with_diagnostics(mut self, diagnostics: Vec<Line>) -> Self {
        let mut all = diagnostics;
        all.append(&mut self.diagnostics);
        self.diagnostics = all;
        self
    }

    /// Folds a second outcome's steps and diagnostics into this one.
    #[must_use]
    pub fn merge(mut self, other: Self) -> Self {
        self.steps = self.steps.saturating_add(other.steps);
        self.diagnostics.extend(other.diagnostics);
        self
    }

    /// Clears the value and records why this outcome is not a value.
    #[must_use]
    pub fn stuck_with(mut self, message: String) -> Self {
        self.value = None;
        self.diagnostics.push(Line(message));
        self
    }

    /// The value's text; empty when there is no value.
    #[must_use]
    pub fn text(&self) -> String {
        self.value.as_ref().map_or_else(String::new, Value::text)
    }

    /// `Int`, `Bool`, `function`, or `stuck`.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        self.value.as_ref().map_or("stuck", Value::kind)
    }
}
