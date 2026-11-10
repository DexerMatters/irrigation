//! Waiting, waking, failure, and retirement.
//!
//! Part of the kernel's `impl Inner`; the struct and its lock live in the
//! parent module.

use super::*;

impl Inner {
    /// Releases everything a node that is no longer reachable owns.
    ///
    /// Shared nodes (still referenced by another trace) are left alone. The
    /// node's value is cleared so that if it becomes reachable again it
    /// re-executes and restages its definitions.
    pub(super) fn retire_unreachable(
        &self,
        state: &mut State,
        node: NodeId,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
    ) {
        if node == NodeId::NONE || !state.node(node).dependents.is_empty() {
            return;
        }
        if matches!(
            state.node(node).kind,
            NodeKind::Unit | NodeKind::RegionContent
        ) {
            // Unit and content nodes never register a write claim.
            return;
        }
        // A node owned by a live task, or with waiters, is not dead: clearing
        // it would hand a second task the same claim. Leave it, claim included;
        // a later commit or reclamation retires it once the claim is released.
        if state.node(node).is_running() || !state.node(node).waiters.is_empty() {
            return;
        }
        state.release_claims(node);
        let definitions = state.node(node).definitions.clone();
        for definition in definitions {
            let view = definition.unit.view;
            Self::with_taken_region(state, view, |state, region| {
                Self::retract_unit(
                    state,
                    region,
                    node,
                    &definition.unit,
                    revision,
                    changed_nodes,
                    false,
                );
            });
        }
        let trace = state.node(node).trace.clone();
        for dep in &trace {
            state.remove_dependent(dep.node, node);
        }
        {
            let record = state.node_mut(node);
            record.value = None;
            record.trace.clear();
            record.definitions.clear();
            record.state = NodeState::Vacant;
            record.verified_at = 0;
            record.verified_validity = 0;
            record.suspect_validity = 0;
            record.suspect_at = 0;
            record.changed_at = 0;
            record.value_version = 0;
            record.failure = None;
        }
        for dep in trace {
            self.retire_unreachable(state, dep.node, revision, changed_nodes);
        }
    }

    pub(super) fn fail<T>(
        &self,
        node: NodeId,
        error: ComputeError,
        revision: Revision,
    ) -> Result<T> {
        let mut state = self.lock();
        state
            .stats
            .execution_failures
            .fetch_add(1, Ordering::Relaxed);
        state.release_claims(node);
        let claim_owner = match state.node(node).state {
            NodeState::Running { owner, .. } => Some(owner),
            _ => None,
        };
        {
            let record = state.node_mut(node);
            if error.is_cancelled() {
                record.state = NodeState::Vacant;
                record.failure = None;
            } else {
                record.state = NodeState::Failed;
                record.failure = Some(error.clone());
                record.failed_at = revision;
            }
        }
        if let Some(owner) = claim_owner {
            forget_claim(&mut state, owner, node);
        }
        let mut waiters = Self::wake_waiters(&mut state, node);
        drop(state);
        #[cfg(feature = "observe")]
        self.emit(crate::observe::RuntimeEvent::NodeFailed {
            node,
            kind: crate::observe::error_kind(&error),
        });
        self.wake.notify_all();
        self.enqueue(std::mem::take(&mut waiters.tasks));
        waiters.wake();
        self.emit_woken(std::mem::take(&mut waiters.woken));
        Err(error)
    }

    /// Records a wait on `node`, or parks when the task cannot yield.
    ///
    /// A pooled task registers itself and returns [`ComputeError::Suspended`]
    /// so its attempt unwinds and the worker is free. An inline task has no
    /// worker to yield and blocks until the node leaves `Running`.
    pub(super) fn wait_for(&self, node: NodeId, me: TaskId) -> Result<()> {
        let mut state = self.lock();
        let owner = match state.node(node).state {
            NodeState::Running { owner, .. } => owner,
            _ => return Ok(()),
        };
        // A task already executing on this stack cannot wait for itself.
        if task::stack_contains(owner) {
            return Err(ComputeError::Cycle {
                path: vec![format!(
                    "re-entrant demand of {} ({node:?})",
                    state.node(node).label
                )],
            });
        }
        if Self::would_cycle(&state, owner, me) {
            return Err(ComputeError::Cycle {
                path: cycle_path(&state, me, node),
            });
        }
        if frame::current().is_some_and(|frame| frame.cancel().is_cancelled()) {
            return Err(ComputeError::Cancelled);
        }
        state.wait_graph.insert(me, node);
        if state.tasks.get(&me).map(|slot| slot.mode) == Some(TaskMode::Pooled) {
            // Yield the worker: record the wait and let the attempt unwind. The
            // claim set is unioned, not replaced: a node claimed for ordered
            // validation is owned even though its frame never opened.
            let held = task::active_snapshot();
            if let Some(slot) = state.tasks.get_mut(&me) {
                slot.status = TaskStatus::Waiting(node);
            }
            merge_held(&mut state, me, &held);
            state.stats.suspensions.fetch_add(1, Ordering::Relaxed);
            state.node_mut(node).waiters.push(me);
            drop(state);
            #[cfg(feature = "observe")]
            self.emit(crate::observe::RuntimeEvent::TaskWaiting { task: me, node });
            return Err(ComputeError::Suspended);
        }
        // Inline tasks have no worker to yield; park the calling thread.
        loop {
            let running = match state.node(node).state {
                NodeState::Running { owner: current, .. } => Some(current),
                _ => None,
            };
            match running {
                Some(current) if current == owner => {
                    drop(state);
                    let helped = self.pool.try_run_one();
                    state = self.lock();
                    if helped {
                        continue;
                    }
                    self.wake.wait_for(&mut state, self.config.wait_slice);
                }
                _ => break,
            }
        }
        state.wait_graph.remove(&me);
        Ok(())
    }

    /// Queues the tasks suspended on `node`. Called under the state lock; the
    /// caller enqueues the returned tasks and reports the wake events once the
    /// lock is released.
    pub(crate) fn wake_waiters(state: &mut State, node: NodeId) -> Waiters {
        let waiters = std::mem::take(&mut state.node_mut(node).waiters);
        let wakers = std::mem::take(&mut state.node_mut(node).wakers);
        let mut tasks = Vec::new();
        #[cfg(feature = "observe")]
        let mut woken = Vec::new();
        for waiter in waiters {
            let pooled = match state.tasks.get_mut(&waiter) {
                Some(slot) if slot.status == TaskStatus::Waiting(node) => {
                    slot.status = TaskStatus::Queued;
                    state.wait_graph.remove(&waiter);
                    slot.mode == TaskMode::Pooled
                }
                _ => false,
            };
            if pooled {
                tasks.push(waiter);
            }
            #[cfg(feature = "observe")]
            woken.push((waiter, node));
        }
        Waiters {
            tasks,
            wakers,
            #[cfg(feature = "observe")]
            woken,
            #[cfg(not(feature = "observe"))]
            woken: Vec::new(),
        }
    }

    /// Returns the node cycles in the current wait graph.
    #[must_use]
    pub fn wait_cycles(&self) -> Vec<Vec<NodeId>> {
        wait_cycles(&self.lock())
    }

    pub(super) fn would_cycle(state: &State, from: TaskId, target: TaskId) -> bool {
        let mut current = from;
        let mut visited = HashSet::new();
        while visited.insert(current) {
            let Some(node) = state.wait_graph.get(&current) else {
                return false;
            };
            match state.node(*node).state {
                NodeState::Running { owner, .. } => {
                    if owner == target {
                        return true;
                    }
                    current = owner;
                }
                _ => return false,
            }
        }
        false
    }

    /// Makes a dropped deferred write group fail its owner for the revision.
    ///
    /// The group was taken out of the pending set, so its definitions will never
    /// publish. Its owner already committed and certified its value, so without
    /// this the next demand at the same revision would reuse that value and
    /// report success for a write that never landed.
    pub(super) fn fail_dropped_writes(
        &self,
        owners: &[NodeId],
        error: &ComputeError,
        revision: Revision,
    ) {
        let mut failed: Vec<NodeId> = Vec::with_capacity(owners.len());
        for &owner in owners {
            if failed.contains(&owner) {
                continue;
            }
            failed.push(owner);
            let _ = self.fail::<()>(owner, error.clone(), revision);
        }
        // The owner committed the value that fed its observers before the group
        // was deferred, so they are not reusable either: invalidating them makes
        // a later demand reach the failed owner and surface the same error
        // instead of replaying a result whose write never landed.
        let mut state = self.lock();
        invalidate(&mut state, failed, revision);
    }

    /// Reopens the owner of a deferred group its demand never drained.
    ///
    /// The group will never install, but this path has not re-checked its
    /// requirement: a from-scratch evaluation at the revision may well install
    /// it, so failing the owner here would invent a failure the drain has not
    /// proved. Releasing the claims and dropping the owner's certification
    /// instead makes the next demand re-execute it, which stages a fresh group;
    /// that demand's drain then installs or fails it exactly as a fresh run
    /// would. The nodes that observed the owner are only marked suspect, so a
    /// replay that reproduces the same output keeps their change versions.
    pub(crate) fn discard_pending_write(state: &mut State, write: &crate::state::PendingWrite) {
        // A re-executing owner already took the group's scopes and value back
        // under its own task, so this teardown has nothing left to undo.
        if state.node(write.owner).is_running() {
            return;
        }
        state.release_claims(write.owner);
        // Order matters: `invalidate` stops at a seed it already considers
        // suspect, so the owner is marked while it still looks clean.
        let _ = invalidate(state, std::iter::once(write.owner), write.revision);
        uncertify(state, write.owner, write.revision);
    }

    /// Resolves write groups deferred to the end of a demand.
    ///
    /// A group whose trace is stale is not installed: its owner is forced to
    /// re-execute against the now-current state and the demand retries. This is
    /// the common path when a producer satisfied the group's requirement during
    /// the same demand.
    pub(crate) fn drain_pending_writes(&self, task: TaskId) -> Result<DrainOutcome> {
        let selector = self.route_selector.lock().clone();
        // Route collisions are collected under the lock and decided without it:
        // the selector is host code and may call back into the runtime.
        let (mut pending, revision, collisions) = {
            let mut state = self.lock();
            let mut pending: Vec<crate::state::PendingWrite> = Vec::new();
            let mut remaining = Vec::new();
            for write in state.pending_writes.drain(..) {
                if write.task == task {
                    pending.push(write);
                } else {
                    remaining.push(write);
                }
            }
            state.pending_writes = remaining;
            if pending.is_empty() {
                return Ok(DrainOutcome::Done);
            }
            let revision = state.revision;
            let mut collisions = Vec::new();
            for write in &pending {
                for collision in route_collisions(&state, write.owner, &write.drafts) {
                    if !collisions.iter().any(|(unit, _, _)| *unit == collision.0) {
                        collisions.push(collision);
                    }
                }
            }
            (pending, revision, collisions)
        };
        let decisions = decide_routes(selector.as_ref(), &collisions);
        let mut state = self.lock();
        let mut overtaken = false;
        loop {
            let mut installed_any = false;
            let mut stale = None;
            let mut index = 0;
            while index < pending.len() {
                let group = WriteGroup {
                    provider: pending[index].owner,
                    drafts: &pending[index].drafts,
                    retractions: &pending[index].retractions,
                    restate_all: pending[index].restate_all,
                    // A deferred group never carries a content replacement: a
                    // group that replaces content cannot be deferred.
                    content_views: &[],
                };
                let holds = pending[index]
                    .requirements
                    .iter()
                    .all(|requirement| requirement_holds(&state, requirement, group));
                if !holds {
                    index += 1;
                    continue;
                }
                let current = pending[index].trace.iter().all(|dep| {
                    let record = state.node(dep.node);
                    record.value.is_some()
                        && record.has_valid_trace()
                        && record.value_version == dep.version
                        && record.routing_at == dep.routing
                });
                if !current {
                    stale = Some(index);
                    break;
                }
                let write = pending.remove(index);
                let mut changed = Vec::new();
                if let Err(error) = Self::validate_definitions(
                    &state,
                    write.owner,
                    &write.drafts,
                    &write.retractions,
                    write.enforce_views,
                ) {
                    // A rejected group takes the rest of the demand's groups
                    // with it: nothing may publish, and every claim they hold
                    // has to be released or the next demand sees a phantom
                    // writer. The owners keep their committed value only until
                    // the failure below marks them, so the loss is not silent.
                    let mut dropped: Vec<NodeId> =
                        pending.iter().map(|pending| pending.owner).collect();
                    dropped.push(write.owner);
                    for &owner in &dropped {
                        state.release_claims(owner);
                    }
                    drop(state);
                    self.fail_dropped_writes(&dropped, &error, revision);
                    return Err(error);
                }
                Self::install_definitions(
                    &mut state,
                    write.owner,
                    write.drafts,
                    (write.retractions, write.restate_all),
                    revision,
                    write.enforce_views,
                    &decisions,
                    &mut changed,
                );
                if !changed.is_empty() {
                    // An install here happens after the demand's last commit,
                    // so a dependent this revision already finalized is a
                    // result that now contradicts the state the drain leaves.
                    // No commit follows to revalidate it: the demand has to
                    // replay instead of carrying that value out.
                    let (_, finalized) =
                        invalidate_tracked(&mut state, changed.iter().copied(), revision);
                    overtaken |= finalized;
                }
                state.release_claims(write.owner);
                installed_any = true;
            }
            if let Some(index) = stale {
                // The stale group is dropped on purpose: its owner re-executes
                // and stages the group again. The groups staged after it are
                // still this demand's work, and the retry keeps the same task,
                // so they stay pending for the retried demand's drain instead
                // of vanishing with the attempt that found them.
                let write = pending.remove(index);
                state.pending_writes.extend(pending);
                state.release_claims(write.owner);
                force_recompute(&mut state, &[write.owner], revision);
                drop(state);
                return Ok(DrainOutcome::Retry);
            }
            if !installed_any {
                let write = pending.first().expect("pending is non-empty");
                let group = WriteGroup {
                    provider: write.owner,
                    drafts: &write.drafts,
                    retractions: &write.retractions,
                    restate_all: write.restate_all,
                    content_views: &[],
                };
                let requirement = write
                    .requirements
                    .iter()
                    .find(|requirement| !requirement_holds(&state, requirement, group))
                    .expect("a pending group with no satisfied requirement");
                let error = precondition_error(requirement);
                let dropped: Vec<NodeId> = pending.iter().map(|pending| pending.owner).collect();
                for &owner in &dropped {
                    state.release_claims(owner);
                }
                drop(state);
                self.fail_dropped_writes(&dropped, &error, revision);
                return Err(error);
            }
            if pending.is_empty() {
                // The pool is finished before the retry is reported, so every
                // group this demand could resolve has installed and the retry
                // starts from the state the drain published.
                return Ok(if overtaken {
                    DrainOutcome::Overtaken
                } else {
                    DrainOutcome::Done
                });
            }
        }
    }

    /// Ends one demand: drains the write groups it deferred, then resolves the
    /// definition conflicts its commits staged.
    ///
    /// Deferral is part of the demand protocol rather than of one kind of root,
    /// so every demand surface finishes here — a general root and a host `call`
    /// defer exactly like a lowered one. Only the attempt that owns the revision
    /// gate may drain: a nested call is part of an enclosing attempt, and its
    /// groups are handed to that attempt when its task closes.
    pub(crate) fn finish_demand(&self, revision: Revision, roots: &[NodeId]) -> Result<()> {
        let me = task::current_or_detached();
        let owns_gate = self.lock().tasks.get(&me).is_some_and(|slot| slot.gate);
        if !owns_gate {
            return Ok(());
        }
        match self.drain_pending_writes(me)? {
            // Both outcomes owe a retry: a stale group means its owner has to
            // re-execute against the state the drain published, and an
            // overtaken value means a root committed before an install moved
            // its port. The retry is what revalidates either result.
            DrainOutcome::Retry | DrainOutcome::Overtaken => {
                return self.replay_demand(roots, revision);
            }
            DrainOutcome::Done => {}
        }
        match self.resolve_conflicts(revision)? {
            // A restore is a post-commit install too, so it reports the same
            // overtaken decision the drain does.
            ConflictOutcome::Overtaken => self.replay_demand(roots, revision),
            ConflictOutcome::Done => Ok(()),
        }
    }

    /// Replays a demand whose post-commit work disturbed a result it had
    /// already finalized.
    ///
    /// Forcing the roots makes the retried attempt evaluate them against the
    /// state the install left instead of carrying the contradicted value out.
    pub(super) fn replay_demand(&self, roots: &[NodeId], revision: Revision) -> Result<()> {
        force_recompute(&mut self.lock(), roots, revision);
        Err(ComputeError::Retry)
    }
}
