//! Demand tasks: submission, attempts, suspension, and resumption.
//!
//! A public read turns into one task. The task runs attempts on a worker; an
//! attempt that needs a node owned by another task suspends instead of parking
//! the worker, and the task is queued again when that node completes. The
//! demand restarts from its root, but every node committed before the
//! suspension is clean, so the restart reuses that work.
//!
//! A demand that runs on its calling thread — because the runtime has no
//! workers, or because it is nested in an attempt that is already on one —
//! cannot yield that thread. It is marked inline and its waits park instead.

use std::sync::atomic::Ordering;
use std::sync::mpsc::sync_channel;

use irrigation_core::error::{ComputeError, Result};
use irrigation_core::frame::CancelToken;
use irrigation_core::ids::NodeId;

use crate::inner::{Inner, Waiters};
use crate::state::{NodeState, State};
use crate::task::{self, Attempt, DemandCause, Task, TaskId, TaskMode, TaskStatus};

impl Inner {
    /// Runs one demand as a task and returns its result.
    pub(crate) fn demand<T: Send + 'static>(
        &self,
        cancel: CancelToken,
        cause: DemandCause,
        body: impl FnMut() -> Result<T> + Send + 'static,
    ) -> Result<T> {
        // The edit closure holds the write side of the gate; a demand from
        // inside it would wait for a permit only this thread can release, so it
        // is refused before any branch below can wait.
        if crate::gate::in_edit() {
            return Err(ComputeError::ReentrantUse {
                operation: "read",
                reason: "an edit transaction is open on this thread and holds the revision gate",
            });
        }
        let started = std::time::Instant::now();
        if cause.evaluates() {
            self.lock().stats.demands.fetch_add(1, Ordering::Relaxed);
        }
        // A demand started from inside a running attempt belongs to it: the
        // enclosing task holds the revision gate for its whole body and is the
        // boundary that drains what its nested demands defer. The slot test is
        // what makes it nested — `current_or_detached` also answers for a
        // parallel region's scoped thread, whose identity no task will close.
        if let Some(parent) = task::current().filter(|id| self.lock().tasks.contains_key(id)) {
            let mut body = body;
            return self.demand_inline(cancel, cause, &mut body, Some(parent));
        }
        if crate::scheduler::on_worker() || self.pool.workers() == 0 {
            // Nothing encloses this demand on this thread, so it is a demand
            // root and owns a gate permit of its own.
            let mut body = body;
            return self.demand_inline(cancel, cause, &mut body, None);
        }
        let (sender, receiver) = sync_channel(1);
        self.gate.enter_read();
        let id = {
            let mut state = self.lock();
            state.alloc_task(
                Some(Box::new(crate::task::Body::new(body, sender))),
                TaskMode::Pooled,
                true,
                cancel,
            )
        };
        self.set_active_task(Some(id));
        self.submit_task(id);
        let (result, ok) = match receiver.recv() {
            Ok(result) => {
                let ok = result.is_ok();
                (result, ok)
            }
            Err(_) => {
                self.close_task(id, None);
                (
                    Err(ComputeError::Panicked {
                        message: String::from("a demand panicked outside its computation"),
                    }),
                    false,
                )
            }
        };
        self.note_demand_started(id, cause);
        self.note_demand_finished(id, ok, started);
        result
    }

    /// Reports a started demand, after it has an identity.
    fn note_demand_started(&self, task: TaskId, cause: DemandCause) {
        #[cfg(feature = "observe")]
        self.emit(crate::observe::RuntimeEvent::DemandStarted { task, cause });
        #[cfg(not(feature = "observe"))]
        let _ = (task, cause);
    }

    fn note_demand_finished(&self, task: TaskId, ok: bool, started: std::time::Instant) {
        #[cfg(feature = "observe")]
        self.emit(crate::observe::RuntimeEvent::DemandFinished {
            task,
            ok,
            duration_ns: started.elapsed().as_nanos() as u64,
        });
        #[cfg(not(feature = "observe"))]
        let _ = (task, ok, started);
    }

    /// Reports a retried attempt, after the caller releases the metadata lock.
    fn note_retry(&self, task: TaskId) {
        #[cfg(feature = "observe")]
        self.emit(crate::observe::RuntimeEvent::Arbitration {
            kind: "demand_retry",
            node: NodeId::NONE,
            port: format!("task {task}"),
        });
        #[cfg(not(feature = "observe"))]
        let _ = task;
    }

    /// Runs a demand on the calling thread, parking it when it has to wait.
    ///
    /// `parent` names the enclosing attempt when this demand is nested in one.
    /// A nested demand's deferrals belong to that attempt's demand root, so they
    /// are handed to `parent` when this task closes instead of being dropped
    /// with it; only a demand with no parent is a root and takes the gate.
    fn demand_inline<T>(
        &self,
        cancel: CancelToken,
        cause: DemandCause,
        body: &mut dyn FnMut() -> Result<T>,
        parent: Option<TaskId>,
    ) -> Result<T> {
        let started = std::time::Instant::now();
        let hold_gate = parent.is_none();
        if hold_gate {
            self.gate.enter_read();
        }
        let id = {
            let mut state = self.lock();
            let id = state.alloc_task(None, TaskMode::Inline, hold_gate, cancel);
            let slot = state.tasks.get_mut(&id).expect("task was just allocated");
            slot.status = TaskStatus::Running;
            id
        };
        // Installed before the first emission: from here a panic in observer
        // code must close the task instead of leaving it running with its gate.
        let _close = InlineTask {
            inner: self,
            id,
            parent,
        };
        self.set_active_task(Some(id));
        self.note_demand_started(id, cause);
        let _enter = task::enter(id);
        let mut retries = 0u32;
        let result = loop {
            match body() {
                Err(error) if error.is_retry() => {
                    retries += 1;
                    self.lock()
                        .stats
                        .demand_retries
                        .fetch_add(1, Ordering::Relaxed);
                    if retries > self.config.max_retries {
                        break Err(ComputeError::UnstableDemand {
                            retries,
                            ports: Vec::new(),
                        });
                    }
                }
                result => break result,
            }
        };
        self.note_demand_finished(id, result.is_ok(), started);
        result
    }

    /// Queues one task for a worker.
    fn submit_task(&self, id: TaskId) {
        let inner = self.arc();
        self.pool.submit(move || inner.run_task(id));
    }

    /// Queues the tasks a commit released, outside the metadata lock.
    pub(crate) fn enqueue(&self, tasks: Vec<TaskId>) {
        for task in tasks {
            self.submit_task(task);
        }
    }

    /// Runs one attempt of a queued task.
    fn run_task(&self, id: TaskId) {
        let body = {
            let mut state = self.lock();
            let ready = state
                .tasks
                .get(&id)
                .is_some_and(|slot| slot.status == TaskStatus::Queued && slot.body.is_some());
            if !ready {
                return;
            }
            state.wait_graph.remove(&id);
            let slot = state.tasks.get_mut(&id).expect("task is ready");
            slot.status = TaskStatus::Running;
            slot.body.take()
        };
        // The slot is running and the task owns its gate permit from here on:
        // every exit, including an unwind through host code, must close it.
        let running = RunningTask::new(self, id);
        #[cfg(feature = "observe")]
        self.emit(crate::observe::RuntimeEvent::TaskRunning { task: id });
        let Some(mut body) = body else {
            return;
        };
        let attempt = {
            let _enter = task::enter(id);
            body.attempt()
        };
        self.finish_attempt(id, body, attempt);
        running.defuse();
    }

    fn finish_attempt(&self, id: TaskId, body: Box<dyn Task>, attempt: Attempt) {
        let mut state = self.lock();
        if attempt == Attempt::Retry {
            state.stats.demand_retries.fetch_add(1, Ordering::Relaxed);
        }
        let Some(slot) = state.tasks.get_mut(&id) else {
            return;
        };
        slot.body = Some(body);
        if attempt == Attempt::Retry {
            let retries = slot.retries + 1;
            let exhausted = retries > self.config.max_retries;
            if !exhausted {
                slot.retries = retries;
                slot.status = TaskStatus::Queued;
            }
            drop(state);
            // The arbitration is host code: emit it once the transition it
            // reports is complete and the metadata lock is released.
            self.note_retry(id);
            if exhausted {
                let error = ComputeError::UnstableDemand {
                    retries,
                    ports: Vec::new(),
                };
                if let Some(body) = self.close_task(id, None) {
                    body.fail(error);
                }
            } else {
                self.submit_task(id);
            }
            return;
        }
        if attempt == Attempt::Suspended {
            debug_assert!(
                slot.status == TaskStatus::Queued || matches!(slot.status, TaskStatus::Waiting(_)),
                "a suspended attempt must leave its task waiting"
            );
            let requeue = slot.status == TaskStatus::Queued;
            drop(state);
            if requeue {
                self.submit_task(id);
            }
            return;
        }
        drop(state);
        if let Some(body) = self.close_task(id, None) {
            body.deliver();
        }
    }

    /// Removes a finished task and releases everything it held.
    ///
    /// Returns the body when the task still has one, so the caller can deliver
    /// its result outside the metadata lock.
    fn close_task(&self, id: TaskId, parent: Option<TaskId>) -> Option<Box<dyn Task>> {
        let (mut waiters, gate, body) = {
            let mut state = self.lock();
            let mut slot = state.tasks.remove(&id)?;
            state.wait_graph.remove(&id);
            let held = std::mem::take(&mut slot.held);
            let waiters = Self::release_claims(&mut state, id, &held);
            // A group deferred inside a nested demand belongs to the enclosing
            // demand, which drains it when it finishes. A group whose demand is
            // over instead has no drain left: it is discarded and its owner
            // reopened, so the next attempt stages it again.
            let mut pending = Vec::with_capacity(state.pending_writes.len());
            let mut discarded = Vec::new();
            for mut write in std::mem::take(&mut state.pending_writes) {
                if write.task != id {
                    pending.push(write);
                } else if let Some(parent) = parent {
                    write.task = parent;
                    pending.push(write);
                } else {
                    discarded.push(write);
                }
            }
            state.pending_writes = pending;
            for write in &discarded {
                Self::discard_pending_write(&mut state, write);
            }
            (waiters, slot.gate, slot.body)
        };
        // The permit releases before the teardown dials any host code below.
        // A waker can panic or re-enter the runtime, and a permit leaked on
        // that path parks every later edit on the gate; `GatePermit` also
        // releases when an unwind leaves this function early.
        let permit = GatePermit {
            gate: &self.gate,
            held: gate,
        };
        self.restore_active_task(id, parent);
        self.wake.notify_all();
        self.enqueue(std::mem::take(&mut waiters.tasks));
        drop(permit);
        waiters.wake();
        self.emit_woken(std::mem::take(&mut waiters.woken));
        body
    }

    /// Releases claims a task still owns after its last attempt.
    fn release_claims(state: &mut State, task: TaskId, held: &[NodeId]) -> Waiters {
        let mut waiters = Waiters::new();
        for &node in held {
            if matches!(state.node(node).state, NodeState::Running { owner, .. } if owner == task) {
                state.node_mut(node).state = NodeState::Vacant;
                state.release_claims(node);
                waiters.merge(Inner::wake_waiters(state, node));
            }
        }
        waiters
    }

    /// Hands the active-task registration back to the task that enclosed `id`.
    ///
    /// A nested inline demand replaces the enclosing task's registration, so
    /// closing it must restore `parent` rather than clear the slot: otherwise
    /// the rest of the enclosing attempt stops being the target of
    /// `Inner::cancel_active`.
    fn restore_active_task(&self, id: TaskId, parent: Option<TaskId>) {
        let mut active = self.active_task.lock();
        if *active == Some(id) {
            *active = parent;
        }
    }
}

/// Releases a task's gate read permit however its teardown ends.
///
/// `close_task` removes the task slot — with it the only durable record that
/// the permit was held — before it calls host code, so the permit has to be a
/// value that releases itself even when a host waker unwinds.
struct GatePermit<'a> {
    gate: &'a crate::gate::Gate,
    held: bool,
}

impl Drop for GatePermit<'_> {
    fn drop(&mut self) {
        if self.held {
            self.gate.exit_read();
        }
    }
}

/// Closes an inline task however its body ends, including a panic.
struct InlineTask<'a> {
    inner: &'a Inner,
    id: TaskId,
    parent: Option<TaskId>,
}

impl Drop for InlineTask<'_> {
    fn drop(&mut self) {
        let _ = self.inner.close_task(self.id, self.parent);
    }
}

/// Closes a pooled task that unwinds after its slot became running.
///
/// A task holds a gate read permit and, through its slot, the sender its host
/// waits on. An unwind between [`TaskStatus::Running`] and the task's normal
/// close would otherwise leave it running forever and park every later edit on
/// the gate, so an armed guard fails it exactly as a failed attempt is failed.
struct RunningTask<'a> {
    inner: &'a Inner,
    id: TaskId,
    armed: bool,
}

impl<'a> RunningTask<'a> {
    fn new(inner: &'a Inner, id: TaskId) -> Self {
        Self {
            inner,
            id,
            armed: true,
        }
    }

    /// Hands the task back to its normal completion path.
    fn defuse(mut self) {
        self.armed = false;
    }
}

impl Drop for RunningTask<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Some(body) = self.inner.close_task(self.id, None) {
            body.fail(ComputeError::Panicked {
                message: String::from("a demand task panicked outside its computation"),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inner::RuntimeConfig;
    use crate::state::{NodeKind, NodeRecord};
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::time::Duration;

    /// A host waker that fails the way a sandboxed callback can: mid-teardown.
    struct PanickingWaker;

    impl std::task::Wake for PanickingWaker {
        fn wake(self: Arc<Self>) {
            panic!("the host waker panicked");
        }
    }

    #[test]
    fn a_panicking_waker_does_not_leak_the_task_gate_permit() {
        let inner = Inner::new(RuntimeConfig {
            workers: 1,
            stack_size: 4 * 1024 * 1024,
            ..RuntimeConfig::default()
        });
        // The read permit a pooled demand takes before it allocates its task.
        inner.gate.enter_read();
        let task = {
            let mut state = inner.lock();
            let node = state.alloc(NodeRecord::empty("unit", NodeKind::Unit));
            let task = state.alloc_task(None, TaskMode::Pooled, true, CancelToken::default());
            state.node_mut(node).state = NodeState::Running {
                owner: task,
                generation: 1,
            };
            let slot = state.tasks.get_mut(&task).expect("task was just allocated");
            slot.held.push(node);
            state
                .node_mut(node)
                .wakers
                .push(std::task::Waker::from(Arc::new(PanickingWaker)));
            task
        };
        // The slot is gone by the time the waker runs, so this unwind is the
        // only path left that can release the permit.
        let closed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            inner.close_task(task, None)
        }));
        assert!(closed.is_err(), "the teardown must dial the waker it woke");

        // Every edit takes the write side, which waits for a leaked permit.
        let (sender, receiver) = mpsc::channel();
        let writer = Arc::clone(&inner);
        std::thread::spawn(move || {
            let _guard = writer.gate.write();
            let _ = sender.send(());
        });
        assert!(
            receiver.recv_timeout(Duration::from_secs(10)).is_ok(),
            "a panicking waker leaked the task's gate permit; no edit can start"
        );
    }
}
