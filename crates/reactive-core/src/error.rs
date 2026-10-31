//! Structured computation errors.
//!
//! Engine errors are values: they can be cached, compared, and reported.
//! User errors are ordinary Rust values (typically `Result` payloads) and are
//! tracked like any other data.

use core::fmt;

use crate::ids::NodeId;

/// Result alias used across the reactive crates.
pub type Result<T> = core::result::Result<T, ComputeError>;

/// A failure produced by the incremental engine rather than by user data.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ComputeError {
    /// A demand cycle was discovered while validating or executing.
    ///
    /// `path` names the invocations or nodes participating in the cycle, in
    /// demand order. Ordinary recursive components must descend structurally;
    /// a value cycle needs an explicit fixed-point region.
    Cycle {
        /// Human-readable description of the cycle path.
        path: Vec<String>,
    },
    /// Two different providers tried to define one logical output unit.
    ///
    /// The plan makes a unique defining computation per unit a construction
    /// invariant; ambiguous definitions are diagnosed, never merged by timing.
    DuplicateDefinition {
        /// View and unit being defined.
        unit: String,
        /// First provider description.
        first: String,
        /// Second, conflicting provider description.
        second: String,
    },
    /// The demand that owned this work was cancelled.
    Cancelled,
    /// User code panicked; the panic did not escape the execution scope.
    Panicked {
        /// Panic message, if the payload was a string.
        message: String,
    },
    /// Recursion exceeded the configured execution depth.
    DepthExceeded {
        /// Configured limit.
        limit: u32,
    },
    /// A tracked read was attempted outside a computation.
    OutsideComputation {
        /// The operation that required a computation frame.
        operation: &'static str,
    },
    /// A construct was rejected by the lowering macro or by an adapter.
    Unsupported {
        /// Description of the rejected construct.
        operation: String,
    },
    /// A definition was attempted without the capability for that region.
    DefinitionDenied {
        /// View and unit being defined.
        unit: String,
        /// Why the definition was rejected.
        reason: String,
    },
    /// A node referenced by a dependency trace no longer exists.
    MissingNode {
        /// The missing node.
        node: NodeId,
    },
    /// A stored value had an unexpected concrete type.
    TypeMismatch {
        /// Expected type name.
        expected: &'static str,
    },
    /// Internal invariant violation. Never used for expected user mistakes.
    Internal {
        /// Description of the violated invariant.
        message: String,
    },
}

impl ComputeError {
    /// Builds a [`ComputeError::Unsupported`] from any displayable value.
    pub fn unsupported(operation: impl Into<String>) -> Self {
        Self::Unsupported {
            operation: operation.into(),
        }
    }

    /// Builds a [`ComputeError::Internal`] from any displayable value.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal {
            message: message.into(),
        }
    }

    /// Returns `true` for failures that are caused by cancellation.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }

    /// Returns `true` when retrying the same demand cannot succeed without
    /// user action (duplicate definitions, unsupported constructs).
    #[must_use]
    pub fn is_permanent(&self) -> bool {
        matches!(
            self,
            Self::DuplicateDefinition { .. }
                | Self::Unsupported { .. }
                | Self::DefinitionDenied { .. }
                | Self::TypeMismatch { .. }
                | Self::Internal { .. }
        )
    }
}

impl fmt::Display for ComputeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cycle { path } => {
                write!(f, "computation cycle detected")?;
                if !path.is_empty() {
                    write!(f, ": {}", path.join(" -> "))?;
                }
                Ok(())
            }
            Self::DuplicateDefinition {
                unit,
                first,
                second,
            } => write!(
                f,
                "unit `{unit}` is defined twice: by `{first}` and by `{second}`"
            ),
            Self::Cancelled => f.write_str("demand was cancelled"),
            Self::Panicked { message } => write!(f, "component panicked: {message}"),
            Self::DepthExceeded { limit } => {
                write!(f, "execution depth limit {limit} exceeded")
            }
            Self::OutsideComputation { operation } => write!(
                f,
                "`{operation}` performed a tracked read outside a computation; \
                 reads must happen inside a component body, a `GeneralComponent`, or a snapshot"
            ),
            Self::Unsupported { operation } => write!(f, "unsupported construct: {operation}"),
            Self::DefinitionDenied { unit, reason } => {
                write!(f, "definition of `{unit}` denied: {reason}")
            }
            Self::MissingNode { node } => write!(f, "dependency refers to missing {node:?}"),
            Self::TypeMismatch { expected } => {
                write!(f, "stored value was not a `{expected}`")
            }
            Self::Internal { message } => write!(f, "internal error: {message}"),
        }
    }
}

impl std::error::Error for ComputeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_is_retryable_and_duplicates_are_permanent() {
        assert!(ComputeError::Cancelled.is_cancelled());
        assert!(!ComputeError::Cancelled.is_permanent());
        assert!(
            ComputeError::DuplicateDefinition {
                unit: "Types[ExprRef(1)]".into(),
                first: "infer".into(),
                second: "check".into(),
            }
            .is_permanent()
        );
    }

    #[test]
    fn display_includes_both_definers() {
        let error = ComputeError::DuplicateDefinition {
            unit: "Types[x]".into(),
            first: "a.rs:10".into(),
            second: "b.rs:20".into(),
        };
        let rendered = error.to_string();
        assert!(rendered.contains("a.rs:10"));
        assert!(rendered.contains("b.rs:20"));
    }
}
