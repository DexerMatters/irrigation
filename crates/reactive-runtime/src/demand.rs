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

use reactive_core::error::{ComputeError, Result};
use reactive_core::frame::CancelToken;
use reactive_core::ids::NodeId;

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
        let started = std::time::Instant::now();
        if cause.evaluates() {
            self.lock().stats.demands.fetch_add(1, Ordering::Relaxed);
        }
        if crate::scheduler::on_worker() {
            // The enclosing task already holds the gate and the worker.
            let mut body = body;
            return self.demand_inline(cancel, cause, &mut body, false);
        }
        if self.pool.workers() == 0 {
            let mut body = body;
            return self.demand_inline(cancel, cause, &mut body, true);
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
                self.close_task(id);
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

    /// Runs a demand on the calling thread, parking it when it has to wait.
    fn demand_inline<T>(
        &self,
        cancel: CancelToken,
        cause: DemandCause,
        body: &mut dyn FnMut() -> Result<T>,
        hold_gate: bool,
    ) -> Result<T> {
        let started = std::time::Instant::now();
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
        self.set_active_task(Some(id));
        self.note_demand_started(id, cause);
        let _close = InlineTask { inner: self, id };
        let _enter = task::enter(id);
        let result = body();
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
    }

    fn finish_attempt(&self, id: TaskId, body: Box<dyn Task>, attempt: Attempt) {
        let mut state = self.lock();
        let Some(slot) = state.tasks.get_mut(&id) else {
            return;
        };
        slot.body = Some(body);
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
        if let Some(body) = self.close_task(id) {
            body.deliver();
        }
    }

    /// Removes a finished task and releases everything it held.
    ///
    /// Returns the body when the task still has one, so the caller can deliver
    /// its result outside the metadata lock.
    fn close_task(&self, id: TaskId) -> Option<Box<dyn Task>> {
        let (mut waiters, gate, body) = {
            let mut state = self.lock();
            let mut slot = state.tasks.remove(&id)?;
            state.wait_graph.remove(&id);
            let held = std::mem::take(&mut slot.held);
            let waiters = self.release_claims(&mut state, id, &held);
            (waiters, slot.gate, slot.body)
        };
        self.clear_active_task(id);
        self.wake.notify_all();
        self.enqueue(std::mem::take(&mut waiters.tasks));
        waiters.wake();
        if gate {
            self.gate.exit_read();
        }
        body
    }

    /// Releases claims a task still owns after its last attempt.
    fn release_claims(&self, state: &mut State, task: TaskId, held: &[NodeId]) -> Waiters {
        let mut waiters = Waiters {
            tasks: Vec::new(),
            wakers: Vec::new(),
        };
        for &node in held {
            if matches!(state.node(node).state, NodeState::Running { owner, .. } if owner == task) {
                state.node_mut(node).state = NodeState::Vacant;
                waiters.merge(self.wake_waiters(state, node));
            }
        }
        waiters
    }

    fn clear_active_task(&self, id: TaskId) {
        let mut active = self.active_task.lock();
        if *active == Some(id) {
            *active = None;
        }
    }
}

/// Closes an inline task however its body ends, including a panic.
struct InlineTask<'a> {
    inner: &'a Inner,
    id: TaskId,
}

impl Drop for InlineTask<'_> {
    fn drop(&mut self) {
        let _ = self.inner.close_task(self.id);
    }
}
