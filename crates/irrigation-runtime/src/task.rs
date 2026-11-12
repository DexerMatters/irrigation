//! Demand tasks: the scheduling unit that suspends instead of blocking.
//!
//! One demand — a host read, a component call, a snapshot — is a task. A task
//! executes attempts. Inside an attempt, requiring a node owned by another task
//! does not park the worker: the attempt ends with [`ComputeError::Suspended`],
//! the task is recorded as waiting on that node, and the worker takes the next
//! job. When the node completes, the task is queued again and its attempt
//! restarts from the demand root. Nodes committed before the suspension are
//! clean, so the restart reuses them; nodes the attempt had claimed stay
//! claimed, so single-flight is preserved across suspensions.
//!
//! Inline tasks are the exception: when a demand runs on the calling thread
//! (a nested read, or a runtime with no workers) a wait must park that thread,
//! because there is no worker to yield.

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::SyncSender;

use irrigation_core::error::{ComputeError, Result};
use irrigation_core::ids::NodeId;

/// Identity of one demand task.
pub type TaskId = u64;

/// How a task is scheduled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskMode {
    /// Runs on a worker and yields that worker when it waits.
    Pooled,
    /// Runs on the calling thread and blocks that thread when it waits.
    Inline,
}

/// Why a demand task was started.
///
/// Recorded with the task so an observer can attribute work to a host read, a
/// call, or an observation refresh.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DemandCause {
    /// A host read: `read`, `read_outputs`, or `read_projection`.
    HostRead,
    /// A host call: `call` or `call_projection`.
    HostCall,
    /// A host snapshot. Capturing content cannot evaluate a node.
    HostSnapshot,
    /// An observation refresh caused by an edit.
    ///
    /// One refresh serves every live subscription, so the cause names the
    /// refresh rather than a single subscription; `SubscriptionNotified`
    /// events carry the subscription-level detail.
    ObservationRefresh,
}

impl DemandCause {
    /// Whether this demand can execute user computation.
    #[must_use]
    pub fn evaluates(self) -> bool {
        !matches!(self, Self::HostSnapshot)
    }

    /// The wire name for this cause's kind.
    #[must_use]
    pub fn kind(self) -> &'static str {
        match self {
            Self::HostRead => "host_read",
            Self::HostCall => "host_call",
            Self::HostSnapshot => "host_snapshot",
            Self::ObservationRefresh => "observation_refresh",
        }
    }
}

/// Scheduling state of one task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskStatus {
    /// Queued for a worker.
    Queued,
    /// An attempt is executing.
    Running,
    /// The attempt suspended on a node.
    Waiting(NodeId),
}

/// One attempt's outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attempt {
    /// The body produced a result.
    Done,
    /// The body suspended on another task's node.
    Suspended,
    /// The attempt observed a state that changed before it could publish.
    Retry,
}

/// A type-erased demand body.
pub trait Task: Send {
    /// Runs one attempt from the demand root.
    fn attempt(&mut self) -> Attempt;
    /// Delivers the recorded result to the host that submitted the task.
    fn deliver(self: Box<Self>);

    /// Delivers a terminal scheduling failure instead of a body result.
    fn fail(self: Box<Self>, error: ComputeError);
}

/// A demand body and its result channel.
pub struct Body<T: Send + 'static> {
    body: Box<dyn FnMut() -> Result<T> + Send>,
    result: Option<Result<T>>,
    out: Option<SyncSender<Result<T>>>,
}

impl<T: Send + 'static> std::fmt::Debug for Body<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Body")
            .field("finished", &self.result.is_some())
            .field("connected", &self.out.is_some())
            .finish_non_exhaustive()
    }
}

impl<T: Send + 'static> Body<T> {
    /// Wraps a body and the channel that receives its result.
    #[must_use]
    pub fn new(
        body: impl FnMut() -> Result<T> + Send + 'static,
        out: SyncSender<Result<T>>,
    ) -> Self {
        Self {
            body: Box::new(body),
            result: None,
            out: Some(out),
        }
    }
}

impl<T: Send + 'static> Task for Body<T> {
    fn attempt(&mut self) -> Attempt {
        match (self.body)() {
            Err(error) if error.is_suspended() => Attempt::Suspended,
            Err(error) if error.is_retry() => Attempt::Retry,
            result => {
                self.result = Some(result);
                Attempt::Done
            }
        }
    }

    fn fail(mut self: Box<Self>, error: ComputeError) {
        self.result = Some(Err(error));
        self.deliver();
    }

    fn deliver(mut self: Box<Self>) {
        let Some(out) = self.out.take() else {
            return;
        };
        let result = self.result.take().unwrap_or_else(|| {
            Err(ComputeError::internal(
                "a demand task finished without recording a result",
            ))
        });
        let _ = out.send(result);
    }
}

struct Frame {
    /// The runtime whose task this is.
    runtime: u64,
    task: TaskId,
    active: Vec<NodeId>,
}

thread_local! {
    static STACK: RefCell<Vec<Frame>> = const { RefCell::new(Vec::new()) };
    static DETACHED: Cell<Option<TaskId>> = const { Cell::new(None) };
    static DETACHED_HELD: RefCell<DetachedHeld> = const { RefCell::new(DetachedHeld::new()) };
}

/// Nodes a slot-less evaluation on this thread has claimed.
///
/// A detached identity has no [`TaskSlot`](crate::state::TaskSlot), so its
/// claims cannot live on the task and the evaluation that took them is what
/// releases them.
struct DetachedHeld {
    nodes: Vec<(u64, NodeId)>,
    stamp: u64,
}

impl DetachedHeld {
    const fn new() -> Self {
        Self {
            nodes: Vec::new(),
            stamp: 0,
        }
    }
}

/// Detached ids are distinct from every task id allocated by a runtime.
static NEXT_DETACHED: AtomicU64 = AtomicU64::new(1 << 63);

/// Installs `task` as the current execution context until the guard drops.
pub struct Enter {
    _private: (),
}

impl std::fmt::Debug for Enter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Enter")
    }
}

impl Drop for Enter {
    fn drop(&mut self) {
        STACK.with(|stack| {
            stack.borrow_mut().pop();
        });
    }
}

/// Makes `task` the current task for the calling thread.
#[must_use]
pub fn enter(runtime: u64, task: TaskId) -> Enter {
    STACK.with(|stack| {
        stack.borrow_mut().push(Frame {
            runtime,
            task,
            active: Vec::new(),
        });
    });
    Enter { _private: () }
}

/// The current task, if this thread is executing one.
#[must_use]
pub fn current() -> Option<TaskId> {
    STACK.with(|stack| stack.borrow().last().map(|frame| frame.task))
}

/// The current task, when this thread is executing one of `runtime`'s.
///
/// `TaskId` is allocated per runtime, so the top frame's id may belong to a
/// different runtime used on the same thread. A caller that asks "am I inside one
/// of *my* tasks" has to say which runtime it is, or a task of the same number in
/// another runtime answers for it.
#[must_use]
pub fn current_in(runtime: u64) -> Option<TaskId> {
    STACK.with(|stack| {
        stack
            .borrow()
            .last()
            .filter(|frame| frame.runtime == runtime)
            .map(|frame| frame.task)
    })
}

/// The current task, or a per-thread identity for evaluation that runs outside
/// any demand (a parallel region's worker threads).
#[must_use]
pub fn current_or_detached() -> TaskId {
    if let Some(task) = current() {
        return task;
    }
    DETACHED.with(|slot| match slot.get() {
        Some(task) => task,
        None => {
            let task = NEXT_DETACHED.fetch_add(1, Ordering::Relaxed);
            slot.set(Some(task));
            // The frame keeps claim and cycle bookkeeping uniform with real
            // tasks: a detached computation can still re-enter its own node.
            STACK.with(|stack| {
                stack.borrow_mut().push(Frame {
                    // A detached identification belongs to no runtime, so no
                    // runtime's `current_in` answers for it -- which is the point:
                    // a slot-less evaluation is never an enclosing task.
                    runtime: 0,
                    task,
                    active: Vec::new(),
                });
            });
            task
        }
    })
}

/// Whether `node` is on the current attempt's execution stack.
#[must_use]
pub fn active_contains(node: NodeId) -> bool {
    STACK.with(|stack| {
        stack
            .borrow()
            .last()
            .is_some_and(|frame| frame.active.contains(&node))
    })
}

/// Marks `node` as executing in the current attempt.
pub fn active_push(node: NodeId) {
    STACK.with(|stack| {
        if let Some(frame) = stack.borrow_mut().last_mut() {
            frame.active.push(node);
        }
    });
}

/// Clears the execution mark of `node`.
pub fn active_pop(node: NodeId) {
    STACK.with(|stack| {
        if let Some(frame) = stack.borrow_mut().last_mut() {
            debug_assert_eq!(frame.active.last(), Some(&node), "unbalanced claim");
            frame.active.pop();
        }
    });
}

/// Nodes the current attempt is executing, innermost last.
#[must_use]
pub fn active_snapshot() -> Vec<NodeId> {
    STACK.with(|stack| {
        stack
            .borrow()
            .last()
            .map_or_else(Vec::new, |frame| frame.active.clone())
    })
}

/// Whether `task` is executing anywhere on this thread's nested stack.
#[must_use]
pub fn stack_contains(task: TaskId) -> bool {
    STACK.with(|stack| stack.borrow().iter().any(|frame| frame.task == task))
}

/// Records a claim taken by evaluation that runs outside any task.
pub(crate) fn hold_detached_claim(node: NodeId) {
    DETACHED_HELD.with(|held| {
        let mut held = held.borrow_mut();
        if held.nodes.iter().any(|(_, existing)| *existing == node) {
            return;
        }
        held.stamp += 1;
        let stamp = held.stamp;
        held.nodes.push((stamp, node));
    });
}

/// Drops a detached claim its evaluation published or failed.
pub(crate) fn forget_detached_claim(node: NodeId) {
    DETACHED_HELD.with(|held| {
        held.borrow_mut()
            .nodes
            .retain(|(_, existing)| *existing != node);
    });
}

/// Marks the current position in this thread's detached claim list.
#[must_use]
pub(crate) fn detached_claim_mark() -> u64 {
    DETACHED_HELD.with(|held| held.borrow().stamp)
}

/// Takes and returns every claim recorded since `mark`.
///
/// Claims are stamped, not indexed: a nested evaluation can publish a node
/// claimed before its mark, and the remaining entries must keep their identity.
pub(crate) fn take_detached_claims_since(mark: u64) -> Vec<NodeId> {
    DETACHED_HELD.with(|held| {
        let mut held = held.borrow_mut();
        let mut taken = Vec::new();
        held.nodes.retain(|(stamp, node)| {
            if *stamp > mark {
                taken.push(*node);
                false
            } else {
                true
            }
        });
        taken
    })
}

#[cfg(test)]
#[path = "../tests/unit/task/tests.rs"]
mod tests;
