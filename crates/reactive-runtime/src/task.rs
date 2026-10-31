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
use std::sync::mpsc::SyncSender;
use std::sync::atomic::{AtomicU64, Ordering};

use reactive_core::error::{ComputeError, Result};
use reactive_core::ids::NodeId;

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

/// Scheduling state of one task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskStatus {
    /// Queued for a worker.
    Queued,
    /// An attempt is executing.
    Running,
    /// The attempt suspended on a node.
    Waiting(NodeId),
    /// The attempt finished.
    Done,
}

/// One attempt's outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attempt {
    /// The body produced a result.
    Done,
    /// The body suspended on another task's node.
    Suspended,
}

/// A type-erased demand body.
pub trait Task: Send {
    /// Runs one attempt from the demand root.
    fn attempt(&mut self) -> Attempt;
    /// Delivers the recorded result to the host that submitted the task.
    fn deliver(self: Box<Self>);
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
            result => {
                self.result = Some(result);
                Attempt::Done
            }
        }
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
    task: TaskId,
    active: Vec<NodeId>,
}

thread_local! {
    static STACK: RefCell<Vec<Frame>> = const { RefCell::new(Vec::new()) };
    static DETACHED: Cell<Option<TaskId>> = const { Cell::new(None) };
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
pub fn enter(task: TaskId) -> Enter {
    STACK.with(|stack| {
        stack.borrow_mut().push(Frame {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_tasks_restore_the_enclosing_one() {
        let outer = enter(1);
        assert_eq!(current(), Some(1));
        active_push(NodeId(1));
        {
            let _inner = enter(2);
            assert_eq!(current(), Some(2));
            assert!(active_snapshot().is_empty());
            active_push(NodeId(2));
        }
        assert_eq!(current(), Some(1));
        assert_eq!(active_snapshot(), vec![NodeId(1)]);
        active_pop(NodeId(1));
        drop(outer);
        assert_eq!(current(), None);
    }

    #[test]
    fn detached_identity_is_stable_per_thread() {
        let first = current_or_detached();
        assert_eq!(first, current_or_detached());
        assert!(first >= 1 << 63);
        assert_eq!(current(), Some(first));
        active_push(NodeId(3));
        assert!(active_contains(NodeId(3)));
        active_pop(NodeId(3));
        let other = std::thread::spawn(current_or_detached).join().expect("thread");
        assert_ne!(first, other);
    }
}
