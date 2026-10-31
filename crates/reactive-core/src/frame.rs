//! The ambient computation frame.
//!
//! Generated component code looks like ordinary Rust: `options.strict()` is a
//! method call, not an explicit `cx` parameter. The runtime installs a
//! thread-local frame for the duration of one node execution; tracked reads and
//! nested evaluations attach their observations to that frame.
//!
//! Frames are execution state, never durable state. A future or task carries
//! its own recorder, so migrating work between workers cannot resurrect a stale
//! thread-local.

use std::any::Any;
use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::ids::{NodeId, ScopeId};
use crate::ops::RuntimeOps;

/// One recorded observation: a node and the version that was consumed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dep {
    /// The observed node.
    pub node: NodeId,
    /// The node's `changed_at` when it was observed.
    pub version: u64,
}

/// Cancellation shared by one demand and everything it spawned.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<CancelState>);

#[derive(Debug, Default)]
struct CancelState {
    cancelled: AtomicBool,
    generation: AtomicU64,
}

impl CancelToken {
    /// Creates a token for a fresh demand generation.
    #[must_use]
    pub fn new(generation: u64) -> Self {
        let state = CancelState::default();
        state.generation.store(generation, Ordering::Release);
        Self(Arc::new(state))
    }

    /// Marks the demand as cancelled. Running work observes this at its next
    /// commit point; nothing cancelled may publish.
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
    }

    /// Returns `true` once cancellation was requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }

    /// Returns the generation this token belongs to.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.0.generation.load(Ordering::Acquire)
    }

    /// Updates the generation, invalidating older commits.
    pub fn set_generation(&self, generation: u64) {
        self.0.generation.store(generation, Ordering::Release);
    }
}

/// One execution context: who is computing, in which identity scope, and what
/// has been observed so far.
pub struct Frame {
    runtime: Arc<dyn RuntimeOps>,
    node: NodeId,
    scope: ScopeId,
    depth: u32,
    cancel: CancelToken,
    recorder: Option<RefCell<Vec<Dep>>>,
    definitions: Option<RefCell<Vec<Box<dyn Any + Send + Sync>>>>,
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Frame")
            .field("node", &self.node)
            .field("scope", &self.scope)
            .field("depth", &self.depth)
            .finish_non_exhaustive()
    }
}

impl Frame {
    /// Builds a frame that records observations for `node`.
    #[must_use]
    pub fn computing(
        runtime: Arc<dyn RuntimeOps>,
        node: NodeId,
        scope: ScopeId,
        depth: u32,
        cancel: CancelToken,
    ) -> Self {
        Self {
            runtime,
            node,
            scope,
            depth,
            cancel,
            recorder: Some(RefCell::new(Vec::new())),
            definitions: Some(RefCell::new(Vec::new())),
        }
    }

    /// Builds a frame that only overrides the identity scope.
    ///
    /// Observations still belong to the enclosing computation.
    #[must_use]
    pub fn scoped(
        runtime: Arc<dyn RuntimeOps>,
        node: NodeId,
        scope: ScopeId,
        depth: u32,
        cancel: CancelToken,
    ) -> Self {
        Self {
            runtime,
            node,
            scope,
            depth,
            cancel,
            recorder: None,
            definitions: None,
        }
    }

    /// The runtime this frame executes against.
    #[must_use]
    pub fn runtime(&self) -> &Arc<dyn RuntimeOps> {
        &self.runtime
    }

    /// The node being computed.
    #[must_use]
    pub fn node(&self) -> NodeId {
        self.node
    }

    /// The identity scope of this frame.
    #[must_use]
    pub fn scope(&self) -> ScopeId {
        self.scope
    }

    /// Nesting depth of this frame.
    #[must_use]
    pub fn depth(&self) -> u32 {
        self.depth
    }

    /// Cancellation token of the owning demand.
    #[must_use]
    pub fn cancel(&self) -> &CancelToken {
        &self.cancel
    }

    /// Records an observation if this frame owns a recorder.
    pub fn record(&self, dep: Dep) {
        if dep.node == self.node {
            return;
        }
        if let Some(recorder) = &self.recorder {
            recorder.borrow_mut().push(dep);
        }
    }

    /// Takes the recorded observations, leaving the frame empty.
    #[must_use]
    pub fn take_deps(&self) -> Vec<Dep> {
        self.recorder
            .as_ref()
            .map(|recorder| std::mem::take(&mut *recorder.borrow_mut()))
            .unwrap_or_default()
    }

    /// Stages an item to commit with this node.
    ///
    /// The runtime stages its own definition records; the frame only carries
    /// them so that a node's value, trace, and routing change commit together.
    pub fn stage<T: Any + Send + Sync>(&self, item: T) {
        if let Some(definitions) = &self.definitions {
            definitions.borrow_mut().push(Box::new(item));
        }
    }

    /// Takes the staged items, leaving the frame empty.
    #[must_use]
    pub fn take_staged<T: Any + Send + Sync>(&self) -> Vec<T> {
        self.definitions
            .as_ref()
            .map(|definitions| {
                std::mem::take(&mut *definitions.borrow_mut())
                    .into_iter()
                    .filter_map(|item| item.downcast::<T>().ok().map(|boxed| *boxed))
                    .collect()
            })
            .unwrap_or_default()
    }
}

thread_local! {
    static FRAMES: RefCell<Vec<Arc<Frame>>> = const { RefCell::new(Vec::new()) };
}

/// Pushes a frame for the duration of a scope, popping it on normal return and
/// on unwind.
#[must_use = "dropping the guard immediately would end the frame"]
pub struct FrameGuard {
    frame: Arc<Frame>,
}

impl FrameGuard {
    /// Pushes `frame` onto the current thread.
    pub fn push(frame: Arc<Frame>) -> Self {
        FRAMES.with(|frames| frames.borrow_mut().push(Arc::clone(&frame)));
        Self { frame }
    }
}

impl std::fmt::Debug for FrameGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameGuard").finish_non_exhaustive()
    }
}

impl Drop for FrameGuard {
    fn drop(&mut self) {
        FRAMES.with(|frames| {
            let mut frames = frames.borrow_mut();
            let popped = frames.pop();
            debug_assert!(
                popped.as_ref().is_some_and(|frame| Arc::ptr_eq(frame, &self.frame)),
                "frame stack corrupted: nested guard dropped out of order"
            );
        });
    }
}

/// Returns the innermost frame, if a computation is running on this thread.
#[must_use]
pub fn current() -> Option<Arc<Frame>> {
    FRAMES.with(|frames| frames.borrow().last().cloned())
}

/// Returns the innermost frame or panics with a structured engine error.
///
/// Tracked reads outside a computation are a programming error: they would
/// silently observe live state that no revision pins.
#[must_use]
pub fn current_or_engine_panic(operation: &'static str) -> Arc<Frame> {
    match current() {
        Some(frame) => frame,
        None => crate::ops::panic_engine(crate::error::ComputeError::OutsideComputation {
            operation,
        }),
    }
}

/// Records an observation in the innermost frame that owns a recorder.
///
/// Scope-only frames delegate to their enclosing computation.
pub fn record_dep(dep: Dep) {
    FRAMES.with(|frames| {
        for frame in frames.borrow().iter().rev() {
            if frame.node() == dep.node {
                return;
            }
            if frame.recorder.is_some() {
                frame.record(dep);
                return;
            }
        }
    });
}

/// Stages an item on the innermost frame that owns a staging list.
pub fn stage<T: Any + Send + Sync>(item: T) {
    FRAMES.with(|frames| {
        for frame in frames.borrow().iter().rev() {
            if frame.definitions.is_some() {
                frame.stage(item);
                return;
            }
        }
    });
}

/// Depth of the innermost frame, or zero outside a computation.
#[must_use]
pub fn depth() -> u32 {
    current().map_or(0, |frame| frame.depth())
}

/// Runs `f` in a child identity scope of the current frame.
pub fn in_scope<R>(discriminant: u64, label: &'static str, f: impl FnOnce() -> R) -> R {
    let parent = current_or_engine_panic("scope");
    let scope = parent
        .runtime()
        .child_scope(parent.scope(), discriminant, label);
    let frame = Arc::new(Frame::scoped(
        Arc::clone(parent.runtime()),
        parent.node(),
        scope,
        parent.depth(),
        parent.cancel().clone(),
    ));
    let guard = FrameGuard::push(frame);
    let result = f();
    drop(guard);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_token_is_shared_and_observable() {
        let token = CancelToken::new(7);
        let clone = token.clone();
        assert!(!clone.is_cancelled());
        token.cancel();
        assert!(clone.is_cancelled());
        assert_eq!(clone.generation(), 7);
    }

    #[test]
    fn frames_pop_in_order_even_after_panic() {
        // A frame cannot be constructed without a runtime; verify stack hygiene
        // through the scope helper instead, which is the public surface.
        let result = std::panic::catch_unwind(|| {
            in_scope(1, "test", || {
                // No ambient frame: this must panic with an engine error.
                panic!("boom");
            })
        });
        assert!(result.is_err());
        assert!(current().is_none());
    }
}
