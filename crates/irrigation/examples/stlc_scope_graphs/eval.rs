//! Call-by-value evaluation, one invocation per `(term, environment, depth)`.
//!
//! The environment is keyed by binding nodes, never by names or types. Unlike
//! the type checker, evaluation values are dynamic: a lambda parameter's value
//! comes from the application and a closure captures its environment, so the
//! value environment is a semantic argument. `eval_term` is still per term,
//! with no `&Terms`/`&ScopeGraph` parameters: the singleton View handles are
//! read ambiently.

use std::collections::BTreeMap;

use crate::report::Line;
use crate::terms::{Literal, Term};
use crate::{Evaluation, ScopeGraph, Terms};

/// How deep a run may recurse.
const DEPTH_LIMIT: u32 = 512;

/// A value environment, keyed by binding nodes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ValueEnv(BTreeMap<u64, Value>);

impl ValueEnv {
    fn get(&self, binding: u64) -> Option<&Value> {
        self.0.get(&binding)
    }

    fn insert(&mut self, binding: u64, value: Value) {
        self.0.insert(binding, value);
    }
}

/// A runtime value.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Value {
    /// An integer.
    Int(i64),
    /// A boolean.
    Bool(bool),
    /// A closure.
    Closure {
        /// The binding node of the parameter.
        binder: u64,
        /// The body term.
        body: u64,
        /// The captured environment.
        env: ValueEnv,
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

/// The result of evaluating one term.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EvalOutcome {
    /// The value, when evaluation produced one.
    pub value: Option<Value>,
    /// Beta and `let` steps performed.
    pub steps: u64,
    /// Why evaluation did not produce a value, if it did not.
    pub diagnostics: Vec<Line>,
}

impl EvalOutcome {
    fn value(value: Value) -> Self {
        Self {
            value: Some(value),
            steps: 0,
            diagnostics: Vec::new(),
        }
    }

    fn stuck(message: String) -> Self {
        Self {
            value: None,
            steps: 0,
            diagnostics: vec![Line(message)],
        }
    }

    fn plus_steps(mut self, steps: u64) -> Self {
        self.steps = self.steps.saturating_add(steps);
        self
    }

    fn merge(mut self, other: Self) -> Self {
        self.steps = self.steps.saturating_add(other.steps);
        self.diagnostics.extend(other.diagnostics);
        self
    }

    fn with_diagnostics(mut self, diagnostics: Vec<Line>) -> Self {
        let mut all = diagnostics;
        all.extend(self.diagnostics);
        self.diagnostics = all;
        self
    }

    fn text(&self) -> String {
        self.value.as_ref().map_or_else(String::new, Value::text)
    }

    fn kind(&self) -> &'static str {
        self.value.as_ref().map_or("stuck", Value::kind)
    }
}

/// Evaluates one term under one environment.
pub fn eval_term(term: u64, env: ValueEnv, depth: u32) -> EvalOutcome {
    eval_term_now(term, env, depth)
}

fn eval_term_now(term: u64, env: ValueEnv, depth: u32) -> EvalOutcome {
    if depth > DEPTH_LIMIT {
        return EvalOutcome::stuck(format!("stopped after {DEPTH_LIMIT} nested terms"));
    }
    match Terms.nodes().get(&term) {
        Some(Term::Var) => {
            let Some(binding) = ScopeGraph.targets().get(&term) else {
                return EvalOutcome::stuck(format!("unresolved variable #{term:x}"));
            };
            match env.get(binding) {
                Some(value) => EvalOutcome::value(value.clone()),
                None => EvalOutcome::stuck(format!("unbound variable #{term:x}")),
            }
        }
        Some(Term::Lam { body }) => EvalOutcome::value(Value::Closure {
            binder: term,
            body,
            env,
        }),
        Some(Term::App { func, arg }) => {
            let function = eval_term(func, env.clone(), depth.saturating_add(1));
            let argument = eval_term(arg, env, depth.saturating_add(1));
            let argument_value = argument.value.clone();
            let mut outcome = function.merge(argument);
            if outcome.value.is_none() {
                return outcome;
            }
            let Some(Value::Closure {
                binder,
                body,
                mut env,
            }) = outcome.value.take()
            else {
                outcome
                    .diagnostics
                    .push(Line(format!("application #{term:x} of a non-function")));
                return outcome;
            };
            let Some(value) = argument_value else {
                return outcome;
            };
            env.insert(binder, value);
            eval_term(body, env, depth.saturating_add(1))
                .plus_steps(outcome.steps.saturating_add(1))
                .with_diagnostics(outcome.diagnostics)
        }
        Some(Term::Let { binding, body }) => {
            let bound = eval_term(binding, env.clone(), depth.saturating_add(1));
            if bound.value.is_none() {
                return bound;
            }
            let value = bound.value.clone().expect("checked above");
            let mut inner = env;
            inner.insert(term, value);
            eval_term(body, inner, depth.saturating_add(1))
                .plus_steps(bound.steps.saturating_add(1))
                .with_diagnostics(bound.diagnostics)
        }
        Some(Term::Int) => match Terms.literals().get(&term) {
            Some(Literal::Int(value)) => EvalOutcome::value(Value::Int(value)),
            _ => EvalOutcome::stuck(format!("integer #{term:x} has no payload")),
        },
        Some(Term::Bool) => match Terms.literals().get(&term) {
            Some(Literal::Bool(value)) => EvalOutcome::value(Value::Bool(value)),
            _ => EvalOutcome::stuck(format!("boolean #{term:x} has no payload")),
        },
        Some(Term::If { cond, yes, no }) => {
            let condition = eval_term(cond, env.clone(), depth.saturating_add(1));
            let Some(Value::Bool(value)) = condition.value.clone() else {
                let mut outcome = condition;
                outcome.value = None;
                outcome
                    .diagnostics
                    .push(Line(format!("if #{term:x}: condition is not Bool")));
                return outcome;
            };
            let branch = if value {
                eval_term(yes, env, depth.saturating_add(1))
            } else {
                eval_term(no, env, depth.saturating_add(1))
            };
            branch
                .plus_steps(condition.steps)
                .with_diagnostics(condition.diagnostics)
        }
        None => EvalOutcome::stuck(format!("term #{term:x} has no shape")),
    }
}

/// Writes the one summary `Evaluation` from the root outcome.
pub fn evaluate_now() -> Evaluation {
    let outcome = Terms.root().map_or_else(EvalOutcome::default, |root| {
        eval_term(root, ValueEnv::default(), 0)
    });
    Evaluation.define_result(Line(outcome.text()));
    Evaluation.define_kind(Line(outcome.kind().to_owned()));
    Evaluation.define_steps(outcome.steps);
    Evaluation.define_diagnostics(outcome.diagnostics);
    Evaluation
}
