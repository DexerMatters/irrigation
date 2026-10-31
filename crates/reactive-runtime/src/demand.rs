//! Demand tasks: submission, attempts, suspension, and resumption.
//!
//! A public read turns into one task. The task runs attempts on a worker; an
//! attempt that needs a node owned by another task suspends instead of parking
//! the worker, and the task is queued again when that node completes. The
//! demand root restarts, but every node committed before the suspension is
//! clean, so the restart reuses that work.

use std::sync::mpsc::sync_channel;

use reactive_core::error::{ComputeError, Result};
use reactive_core::frame::CancelToken;

use crate::inner::Inner;
use crate::task::{self, Attempt, Task, TaskId, TaskMode, TaskStatus};

impl Inner {
    /// Runs one demand as a task and returns its result.
    pub(crate) fn demand<T: Send + 'static>(
        &self,
        cancel: CancelToken,
        body: impl FnMut() -> Result<T> + Send + 'static,
    ) -> Result<T> {
        if self.pool.workers() == 0 || crate::scheduler::on_worker() {
            let mut body = body;
            return self.demand_inline(cancel, &mut body);
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
        match receiver.recv() {
            Ok(result) => result,
            Err(_) => {
                self.abort_task(id);
                Err(ComputeError::Panicked {
                    message: String::from("a demand task panicked"),
                })
            }
        }
    }

    /// Runs a demand on the calling thread, blocking when it has to wait.
    fn demand_inline<T>(
        &self,
        cancel: CancelToken,
        body: &mut dyn FnMut() -> Result<T>,
    ) -> Result<T> {
        let id = {
            let mut state = self.lock();
            let id = state.alloc_task(None, TaskMode::Inline, false, cancel);
            let slot = state.tasks.get_mut(&id).expect("task was just allocated");
            slot.status = TaskStatus::Running;
            id
        };
        self.set_active_task(Some(id));
        let result = {
            let _enter = task::enter(id);
            body()
        };
        self.finish_inline(id);
        self.clear_active_task(id);
        result
    }

    fn finish_inline(&self, id: TaskId) {
        let (queued, gate) = {
            let mut state = self.lock();
            let Some(mut slot) = state.tasks.remove(&id) else {
                return;
            };
            state.wait_graph.remove(&id);
            let held = std::mem::take(&mut slot.held);
            let queued = Self::release_claims(&mut state, id, &held);
            (queued, slot.gate)
        };
        self.wake.notify_all();
        self.enqueue(queued);
        if gate {
            self.gate.exit_read();
        }
    }

    /// Queues one task for a worker.
    pub(crate) fn submit_task(&self, id: TaskId) {
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
            let requeue = slot.status == TaskStatus::Queued;
            drop(state);
            if requeue {
                self.submit_task(id);
            }
            return;
        }
        let held = std::mem::take(&mut slot.held);
        let queued = Self::release_claims(&mut state, id, &held);
        let mut slot = state.tasks.remove(&id).expect("task is present");
        drop(state);
        self.wake.notify_all();
        self.enqueue(queued);
        self.clear_active_task(id);
        if slot.gate {
            self.gate.exit_read();
        }
        if let Some(body) = slot.body.take() {
            body.deliver();
        }
    }

    /// Releases a task that never delivered a result.
    fn abort_task(&self, id: TaskId) {
        let (queued, gate) = {
            let mut state = self.lock();
            let Some(mut slot) = state.tasks.remove(&id) else {
                return;
            };
            state.wait_graph.remove(&id);
            let held = std::mem::take(&mut slot.held);
            let queued = Self::release_claims(&mut state, id, &held);
            (queued, slot.gate)
        };
        self.clear_active_task(id);
        self.enqueue(queued);
        if gate {
            self.gate.exit_read();
        }
    }

    fn clear_active_task(&self, id: TaskId) {
        let mut active = self
            .active_task
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if *active == Some(id) {
            *active = None;
        }
    }
}
