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
    /// The node's monotonic value version when it was observed.
    ///
    /// This is what validation compares: an observation is unchanged only when
    /// the node's value version and routing generation are both unchanged.
    pub version: u64,
    /// The node's routing generation when it was observed.
    pub routing: u64,
    /// The node's certification stamp when it was observed.
    ///
    /// Recorded for diagnostics, **not** a validation criterion: the stamp
    /// advances on a same-revision invalidation even when nothing else moved, so
    /// comparing it would reject an unchanged output. `validate_trace` compares
    /// `version` and `routing` only, and the test
    /// `unchanged_output_does_not_invalidate_consumers` is what says so.
    ///
    /// Invalidation advances the stamp even when the revision, value version,
    /// and routing generation are unchanged. It is carried with an observation
    /// so a consumer can tell whether the observation predates the node's
    /// current certification; publication is decided by [`Self::version`] and
    /// [`Self::routing`], because an equal value version already pins the value
    /// that was read.
    pub validity: u64,
}

/// A structural precondition on a write.
///
/// The requirement is re-evaluated at commit from the observed ports. A false
/// requirement may defer the write group to the end of the demand, where the
/// remaining producers have run; a requirement that still does not hold fails
/// the write with [`crate::error::ComputeError::PreconditionUnmet`]. It is
/// never satisfied by the absence that made the condition false.
#[derive(Clone, Debug)]
pub struct Requirement {
    /// The ports whose presence the condition concerns.
    pub ports: Vec<crate::descriptor::UnitKey>,
    /// Whether every port must be present (`true`) or absent (`false`).
    ///
    /// This is the presence state the write requires at commit, not the value
    /// the caller's precondition happened to have when it was staged.
    pub expected: bool,
    /// Human-readable condition, used in diagnostics.
    pub description: &'static str,
}

impl Requirement {
    /// Requires every port to be present at commit.
    #[must_use]
    pub fn present(ports: Vec<crate::descriptor::UnitKey>, description: &'static str) -> Self {
        Self {
            ports,
            expected: true,
            description,
        }
    }

    /// Requires every port to be absent at commit.
    #[must_use]
    pub fn absent(ports: Vec<crate::descriptor::UnitKey>, description: &'static str) -> Self {
        Self {
            ports,
            expected: false,
            description,
        }
    }
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
    providers: Option<RefCell<crate::hash::FxHashSet<NodeId>>>,
    definitions: Option<RefCell<Vec<Box<dyn Any + Send + Sync>>>>,
    requirements: Option<Box<RefCell<Vec<Requirement>>>>,
    retractions: Option<Box<RefCell<Vec<crate::descriptor::UnitKey>>>>,
    /// Whether observations made here belong to the enclosing computation.
    /// A scratch frame is a trace barrier: it records nothing and forwards
    /// nothing outward.
    delegating: bool,
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
            providers: Some(RefCell::new(crate::hash::FxHashSet::default())),
            definitions: Some(RefCell::new(Vec::new())),
            requirements: Some(Box::new(RefCell::new(Vec::new()))),
            retractions: Some(Box::new(RefCell::new(Vec::new()))),
            delegating: true,
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
            providers: None,
            definitions: None,
            requirements: None,
            retractions: None,
            delegating: true,
        }
    }

    /// Builds a frame that records nothing and forwards nothing outward.
    ///
    /// Housekeeping demands — certifying a View's producer for a caller that
    /// already observed one of its units, for example — execute inside one so
    /// the work they force never joins the caller's trace.
    #[must_use]
    pub fn scratch(
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
            providers: None,
            definitions: None,
            requirements: None,
            retractions: None,
            delegating: false,
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

    /// Records a provider observation at most once per run.
    ///
    /// Provider observations exist for handover detection, not for the value a
    /// read returns: one per provider is enough, and repeating them per unit
    /// read would make traces grow with the number of reads instead of the
    /// number of producers observed.
    pub fn record_provider(&self, dep: Dep) {
        if dep.node == self.node {
            return;
        }
        let Some(providers) = &self.providers else {
            return;
        };
        if providers.borrow_mut().insert(dep.node) {
            self.record(dep);
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

    /// Records a structural requirement for the current write group.
    pub fn require(&self, requirement: Requirement) {
        if let Some(requirements) = &self.requirements {
            requirements.borrow_mut().push(requirement);
        }
    }

    /// Takes the recorded requirements, leaving the frame empty.
    #[must_use]
    pub fn take_requirements(&self) -> Vec<Requirement> {
        self.requirements
            .as_ref()
            .map(|requirements| std::mem::take(&mut *requirements.borrow_mut()))
            .unwrap_or_default()
    }

    /// Records an explicit unit retraction for the current write group.
    pub fn retract(&self, unit: crate::descriptor::UnitKey) {
        if let Some(retractions) = &self.retractions {
            retractions.borrow_mut().push(unit);
        }
    }

    /// Takes the recorded retractions, leaving the frame empty.
    #[must_use]
    pub fn take_retractions(&self) -> Vec<crate::descriptor::UnitKey> {
        self.retractions
            .as_ref()
            .map(|retractions| std::mem::take(&mut *retractions.borrow_mut()))
            .unwrap_or_default()
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
                popped
                    .as_ref()
                    .is_some_and(|frame| Arc::ptr_eq(frame, &self.frame)),
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
        None => {
            crate::ops::panic_engine(crate::error::ComputeError::OutsideComputation { operation })
        }
    }
}

/// Records an observation in the innermost frame that owns a recorder.
///
/// Scope-only frames delegate to their enclosing computation. A scratch frame
/// is a barrier: the walk stops there and the observation is dropped.
pub fn record_dep(dep: Dep) {
    FRAMES.with(|frames| {
        for frame in frames.borrow().iter().rev() {
            if !frame.delegating {
                return;
            }
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

/// Records a provider observation in the innermost frame that owns a recorder.
///
/// Repeated observations of the same provider collapse to one, so a trace
/// lists each producer once no matter how many of its units were read.
/// Scope-only frames delegate; a scratch frame stops the walk.
pub fn record_provider_dep(dep: Dep) {
    FRAMES.with(|frames| {
        for frame in frames.borrow().iter().rev() {
            if !frame.delegating {
                return;
            }
            if frame.node() == dep.node {
                return;
            }
            if frame.recorder.is_some() {
                frame.record_provider(dep);
                return;
            }
        }
    });
}

/// Stages an item on the innermost frame that owns a staging list.
///
/// Scope-only frames delegate to their enclosing computation. A scratch frame
/// is a barrier: nothing staged there is committed.
pub fn stage<T: Any + Send + Sync>(item: T) {
    FRAMES.with(|frames| {
        for frame in frames.borrow().iter().rev() {
            if !frame.delegating {
                return;
            }
            if frame.definitions.is_some() {
                frame.stage(item);
                return;
            }
        }
    });
}

/// Records a structural requirement on the innermost frame that owns a
/// requirement list.
///
/// Scope-only frames delegate to their enclosing computation, so a component
/// body called inside a per-iteration scope still constrains the write group
/// it is part of. A scratch frame is a barrier.
pub fn require(requirement: Requirement) {
    FRAMES.with(|frames| {
        for frame in frames.borrow().iter().rev() {
            if !frame.delegating {
                return;
            }
            if frame.requirements.is_some() {
                frame.require(requirement);
                return;
            }
        }
    });
}

/// Records an explicit retraction on the innermost frame that owns a
/// retraction list.
///
/// Retraction mirrors installation: both walk to the frame that will commit,
/// so a retraction staged inside a per-iteration scope is not lost.
pub fn retract(unit: crate::descriptor::UnitKey) {
    FRAMES.with(|frames| {
        for frame in frames.borrow().iter().rev() {
            if !frame.delegating {
                return;
            }
            if frame.retractions.is_some() {
                frame.retract(unit);
                return;
            }
        }
    });
}

/// Returns the scope of the innermost frame that will commit work staged on
/// this thread, if any.
///
/// Scope-only frames delegate, so a definition made inside a per-iteration
/// scope belongs to the enclosing computation. A scratch frame is a barrier
/// and reports no scope, which keeps housekeeping work out of the caller's
/// write group.
#[must_use]
pub fn commit_scope() -> Option<ScopeId> {
    FRAMES.with(|frames| {
        frames
            .borrow()
            .iter()
            .rev()
            .take_while(|frame| frame.delegating)
            .find(|frame| frame.definitions.is_some())
            .map(|frame| frame.scope())
    })
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
    // Frames are thread-local execution state: `RefCell` is intentional and the
    // frame never crosses a thread boundary.
    #[allow(clippy::arc_with_non_send_sync)]
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
#[path = "../tests/unit/frame/tests.rs"]
mod tests;
