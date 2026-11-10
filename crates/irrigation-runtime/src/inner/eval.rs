//! Demand, execution, and the commit of one attempt.
//!
//! Part of the kernel's `impl Inner`; the struct and its lock live in the
//! parent module.

use super::*;

impl Inner {
    /// Demands the general component that produces a View.
    pub(super) fn demand_producer(&self, producer: NodeId) -> Result<Value> {
        let mut compute = || {
            Err(ComputeError::internal(
                "a general component producer does not use a compute closure",
            ))
        };
        let eq = {
            let state = self.lock();
            let general = state
                .node(producer)
                .general
                .map(|index| Arc::clone(&state.generals[index]));
            move |left: &Value, right: &Value| {
                general
                    .as_ref()
                    .is_some_and(|general| general.output_eq(left, right))
            }
        };
        self.eval(producer, &mut compute, &eq)
    }

    /// Re-certifies a general component whose observed units are all unchanged.
    ///
    /// Observed units may look unverified only because their producers have not
    /// run yet. Producers are certified first (in a scratch frame, so the walk
    /// never becomes part of any trace); if every observed version survives, the
    /// component is re-certified without running its body.
    pub(super) fn certify_observed(&self, trace: &[Dep], revision: Revision) -> Result<()> {
        let views: Vec<ViewId> = {
            let state = self.lock();
            trace
                .iter()
                .filter_map(|dep| {
                    let record = state.node(dep.node);
                    let unit = record.unit.as_ref()?;
                    (record.verified_at != revision || !record.has_valid_trace())
                        .then_some(unit.view)
                })
                .collect()
        };
        if views.is_empty() {
            return Ok(());
        }
        let mut seen = irrigation_core::hash::FxHashSet::default();
        for view in views {
            ensure_view_current(self, view, &mut seen, 0)?;
        }
        Ok(())
    }

    /// Demands a node, executing `compute` when reuse is impossible.
    ///
    /// The claim decision, the frame construction, and the commit tail live in
    /// separate functions on purpose: this frame stays live for every ancestor
    /// demand in a recursive computation, so it must not carry the locals of
    /// the claim protocol or of commit. As a single function it was a 4.6 KB
    /// debug-build frame, and four of those are live per component-call level.
    pub fn eval(
        &self,
        node: NodeId,
        compute: &mut dyn FnMut() -> Result<Value>,
        eq: &dyn Fn(&Value, &Value) -> bool,
    ) -> Result<Value> {
        // A slot-less evaluation has no task teardown to release the claims it
        // takes, so this call is its teardown.
        let _claims = DetachedClaims::new(self);
        match self.begin_eval(node)? {
            Begin::Done(value) => Ok(value),
            Begin::General { index, claim } => self.eval_general(node, index, claim),
            Begin::Run(claim) => self.run_claimed(node, compute, eq, claim),
        }
    }

    /// Runs a custom component under its own active-node marker.
    #[inline(never)]
    pub(super) fn eval_general(&self, node: NodeId, index: usize, claim: Claim) -> Result<Value> {
        task::active_push(node);
        let result = self.run_general(node, index, claim.revision, claim.owner, claim.generation);
        task::active_pop(node);
        result
    }

    /// Claims the node and decides whether its cached value can be reused.
    ///
    /// Kept out of [`Inner::eval`] so the claim protocol's state is not part of
    /// the stack frame that a recursive demand keeps alive.
    #[inline(never)]
    pub(super) fn begin_eval(&self, node: NodeId) -> Result<Begin> {
        if frame::current().is_some_and(|frame| frame.cancel().is_cancelled()) {
            return Err(ComputeError::Cancelled);
        }
        let me = task::current_or_detached();
        let generation = loop {
            let action = {
                let mut state = self.lock();
                let revision = state.revision;
                let validity = state.next_validity;
                let record = state.node_mut(node);
                if let Some(failure) = &record.failure {
                    if record.failed_at == revision {
                        Action::Fail(failure.clone())
                    } else {
                        record.failure = None;
                        record.state = NodeState::Vacant;
                        record.attempt_validity = validity;
                        let generation = state.next_generation + 1;
                        state.next_generation = generation;
                        state.node_mut(node).state = NodeState::Running {
                            owner: me,
                            generation,
                        };
                        Action::Claim(generation)
                    }
                } else if record.verified_at == revision
                    && record.value.is_some()
                    && record.has_valid_trace()
                {
                    Action::Return(
                        record.value.clone().expect("checked"),
                        record.value_version,
                        record.routing_at,
                        record.verified_validity,
                    )
                } else if record.is_running() {
                    match record.state {
                        NodeState::Running { owner, generation } if owner == me => {
                            if task::active_contains(node) {
                                Action::Fail(ComputeError::Cycle {
                                    path: vec![format!(
                                        "re-entrant demand of {} ({node:?})",
                                        record.label
                                    )],
                                })
                            } else {
                                Action::Resume(generation)
                            }
                        }
                        _ => Action::Wait,
                    }
                } else if record.suspect_at <= record.verified_at
                    && record.has_valid_trace()
                    && record.value.is_some()
                {
                    record.certify(revision);
                    record.validated_at = revision;
                    Action::Return(
                        record.value.clone().expect("checked"),
                        record.value_version,
                        record.routing_at,
                        record.verified_validity,
                    )
                } else {
                    state.next_generation += 1;
                    let generation = state.next_generation;
                    state.node_mut(node).attempt_validity = validity;
                    state.node_mut(node).state = NodeState::Running {
                        owner: me,
                        generation,
                    };
                    Action::Claim(generation)
                }
            };

            match action {
                Action::Return(value, version, routing, validity) => {
                    frame::record_dep(Dep {
                        node,
                        version,
                        routing,
                        validity,
                    });
                    return Ok(Begin::Done(value));
                }
                Action::Fail(error) => return Err(error),
                Action::Wait => self.wait_for(node, me)?,
                Action::Claim(generation) => {
                    // Ownership starts here, not when the frame opens: ordered
                    // validation runs user code, and an error, panic, or
                    // cancellation on that path must still release the claim.
                    let mut state = self.lock();
                    hold_claim(&mut state, me, node);
                    drop(state);
                    #[cfg(feature = "observe")]
                    self.emit(crate::observe::RuntimeEvent::NodeClaimed { node, task: me });
                    break generation;
                }
                Action::Resume(generation) => {
                    let mut state = self.lock();
                    hold_claim(&mut state, me, node);
                    break generation;
                }
            }
        };

        // Ordered validation. Re-certifying a node without running it needs its
        // observed inputs to be final, which is a property of every traced node
        // rather than of one component kind: certify the producers of the views
        // this node observed (depth-first, in a scratch frame) before comparing
        // versions. Without this a consumer could validate against a producer
        // that has not run yet and mistake an unchanged observation for a stale
        // one.
        let (revision, scope, trace, has_value) = {
            let state = self.lock();
            let record = state.node(node);
            (
                state.revision,
                record.scope,
                record.trace.clone(),
                record.value.is_some(),
            )
        };
        if has_value {
            self.certify_observed(&trace, revision)?;
        }
        if has_value && self.validate_trace(&trace, revision) {
            let (value, version, routing, validity) = {
                let mut state = self.lock();
                let general = state.node(node).general.is_some();
                if general {
                    state.stats.no_change_runs.fetch_add(1, Ordering::Relaxed);
                }
                let record = state.node_mut(node);
                record.state = NodeState::Ready;
                record.certify(revision);
                record.validated_at = revision;
                record.suspect_at = 0;
                let observed = (
                    record
                        .value
                        .clone()
                        .expect("validated node without a value"),
                    record.value_version,
                    record.routing_at,
                    record.verified_validity,
                );
                forget_claim(&mut state, me, node);
                observed
            };
            frame::record_dep(Dep {
                node,
                version,
                routing,
                validity,
            });
            #[cfg(feature = "observe")]
            self.emit(crate::observe::RuntimeEvent::NodeValidated {
                node,
                deps: trace.len() as u32,
            });
            return Ok(Begin::Done(value));
        }

        let claim = Claim {
            revision,
            scope,
            generation,
            owner: me,
        };
        // Custom components run their own algorithm instead of the caller's
        // closure; their state and output commit atomically.
        let general_index = self.lock().node(node).general;
        if let Some(index) = general_index {
            return Ok(Begin::General { index, claim });
        }
        Ok(Begin::Run(claim))
    }

    /// Pushes the execution frame for one claimed node.
    ///
    /// The frame object is large in debug builds; constructing it in its own
    /// function keeps that temporary out of the frame that stays live across
    /// the body's recursive demands.
    #[inline(never)]
    pub(super) fn open_frame(&self, node: NodeId, claim: Claim) -> Result<ClaimedFrame> {
        let parent = frame::current();
        let (depth, cancel) = match &parent {
            Some(parent) => (parent.depth(), parent.cancel().clone()),
            None => (0, CancelToken::default()),
        };
        if depth + 1 > self.config.max_depth {
            return self.fail(
                node,
                ComputeError::DepthExceeded {
                    limit: self.config.max_depth,
                },
                claim.revision,
            );
        }
        // Frames are thread-local execution state: `RefCell` is intentional and
        // the frame never crosses a thread boundary.
        #[allow(clippy::arc_with_non_send_sync)]
        let frame = Arc::new(Frame::computing(
            self.arc(),
            node,
            claim.scope,
            depth + 1,
            cancel.clone(),
        ));
        task::active_push(node);
        let guard = FrameGuard::push(Arc::clone(&frame));
        Ok(ClaimedFrame {
            frame,
            guard,
            cancel,
            started: std::time::Instant::now(),
        })
    }

    /// Runs a claimed node's caller-supplied closure and hands off to commit.
    ///
    /// Only the locals that must survive the body's recursive demands live
    /// here; everything the commit needs is read after the body returns.
    #[inline(never)]
    pub(super) fn run_claimed(
        &self,
        node: NodeId,
        compute: &mut dyn FnMut() -> Result<Value>,
        eq: &dyn Fn(&Value, &Value) -> bool,
        claim: Claim,
    ) -> Result<Value> {
        let claimed = self.open_frame(node, claim)?;
        let result = catch_engine(compute);
        self.finish_eval(node, claimed, result, eq, claim)
    }

    /// Takes the execution result, commits the write group, and records the
    /// observation in the caller.
    #[inline(never)]
    pub(super) fn finish_eval(
        &self,
        node: NodeId,
        claimed: ClaimedFrame,
        result: Result<Result<Value>>,
        eq: &dyn Fn(&Value, &Value) -> bool,
        claim: Claim,
    ) -> Result<Value> {
        let ClaimedFrame {
            frame,
            guard,
            cancel,
            started,
        } = claimed;
        let definitions: Vec<DefinitionDraft> = frame.take_staged();
        let requirements = frame.take_requirements();
        let retractions = frame.take_retractions();
        let deps = frame.take_deps();
        drop(guard);
        task::active_pop(node);

        if cancel.is_cancelled() {
            return self.fail(node, ComputeError::Cancelled, claim.revision);
        }
        match result {
            Ok(Ok(value)) => {
                // A lowered component's body restates its whole output on every
                // run, so its commit replaces the definition set. Only a custom
                // component writes deltas.
                let committed = self.commit(
                    node,
                    value,
                    deps,
                    definitions,
                    requirements,
                    Vec::new(),
                    (retractions, true),
                    true,
                    true,
                    None,
                    FxHashMap::default(),
                    FxHashSet::default(),
                    eq,
                    claim.revision,
                    claim.owner,
                    claim.generation,
                    &cancel,
                )?;
                let committed = match committed {
                    CommitOutcome::Published(committed) => committed,
                    CommitOutcome::SelfInvalidated => return Err(ComputeError::Retry),
                };
                #[cfg(feature = "observe")]
                let changed_at = self.changed_at(node);
                let version = self.value_version_at(node);
                let routing = self.routing_at(node);
                let validity = self.validity_at(node);
                frame::record_dep(Dep {
                    node,
                    version,
                    routing,
                    validity,
                });
                #[cfg(feature = "observe")]
                self.note_commit(node, changed_at, claim.revision, started);
                #[cfg(not(feature = "observe"))]
                let _ = (started, version);
                Ok(committed)
            }
            Ok(Err(error)) | Err(error) => {
                // A suspension is scheduling, not failure: the claim is kept
                // and the task is queued again when the awaited node completes.
                if error.is_suspended() || error.is_retry() {
                    return Err(error);
                }
                self.fail(node, error, claim.revision)
            }
        }
    }

    /// Runs one custom component: initialize or update, then commit the new
    /// state, output, trace, and definitions atomically.
    pub(super) fn run_general(
        &self,
        node: NodeId,
        index: usize,
        revision: Revision,
        owner: TaskId,
        generation: u64,
    ) -> Result<Value> {
        #[cfg(feature = "observe")]
        let started = std::time::Instant::now();
        let (general, old_trace, old_state, scope, old_unit_scopes, old_retained) = {
            let state = self.lock();
            let record = state.node(node);
            let general = Arc::clone(&state.generals[index]);
            (
                general,
                record.trace.clone(),
                record.general_state.clone(),
                record.scope,
                record.general_unit_scopes.clone(),
                record.general_retained_scopes.clone(),
            )
        };
        // The batch is lazy: it is materialized when the body first asks, which
        // is after the body's own reads have pulled the producers it observes.
        // No pre-walk is needed to make it accurate, and evaluation order no
        // longer decides what update sees.
        let changes = {
            let weak = Arc::downgrade(&self.arc());
            let trace = old_trace;
            crate::general::ChangeBatch::lazy(move || {
                let Some(inner) = weak.upgrade() else {
                    return Vec::new();
                };
                let state = inner.lock();
                observed_changes(&state, &trace, &old_unit_scopes, &old_retained)
            })
        };
        let parent = frame::current();
        let (depth, cancel) = match &parent {
            Some(parent) => (parent.depth(), parent.cancel().clone()),
            None => (0, CancelToken::default()),
        };
        if depth + 1 > self.config.max_depth {
            return self.fail(
                node,
                ComputeError::DepthExceeded {
                    limit: self.config.max_depth,
                },
                revision,
            );
        }
        // Frames are thread-local execution state; see the note in `eval`.
        #[allow(clippy::arc_with_non_send_sync)]
        let frame = Arc::new(Frame::computing(
            self.arc(),
            node,
            scope,
            depth + 1,
            cancel.clone(),
        ));
        let guard = FrameGuard::push(Arc::clone(&frame));
        let mut cx = crate::general::GeneralCx::new(self.arc(), revision, scope);
        let output_views = general.output_views();
        if !output_views.is_empty() {
            cx.restrict_output_views(output_views);
        }
        let use_update = general.policy() == crate::general::DependencyUpdates::Incremental
            && old_state.is_some();
        let result = catch_engine(|| match (&old_state, use_update) {
            (Some(state), true) => general.update(&mut cx, state, &changes),
            _ => general.initialize(&mut cx),
        });
        let pending_claims = cx.take_pending_claims();
        if !pending_claims.is_empty() {
            let scopes: Vec<WriteScope> = pending_claims.iter().map(WriteScope::entity).collect();
            if let Err(error) = frame
                .runtime()
                .claim_writes(&scopes, "general output", None)
            {
                cx.set_definition_error(format!("{error}"));
            }
        }
        let (unit_scopes, retained_scopes) = cx.take_scopes();
        let definition_error = cx.take_definition_error();
        let content_updates = cx.take_content_updates();
        let (mut retractions, restate_all) = cx.take_definition_mode();
        let definitions: Vec<DefinitionDraft> = frame.take_staged();
        let requirements = frame.take_requirements();
        let frame_retractions = frame.take_retractions();
        retractions.extend(frame_retractions);
        let deps = frame.take_deps();
        drop(guard);
        // A component that retains its state outside the node record stages it
        // for this attempt; the commit tail below promotes it exactly on success
        // and drops it on every other exit, so a cancelled or retried attempt
        // leaves the previous state valid.
        if frame_cancelled(&parent) {
            crate::general::discard_local_state(node);
            return self.fail(node, ComputeError::Cancelled, revision);
        }
        if let Some(reason) = definition_error {
            crate::general::discard_local_state(node);
            return self.fail(
                node,
                ComputeError::DefinitionDenied {
                    unit: String::from("general component output"),
                    reason,
                },
                revision,
            );
        }
        match result {
            Ok(Ok((state, output))) => {
                let eq = |left: &Value, right: &Value| general.output_eq(left, right);
                let committed = self.commit(
                    node,
                    output,
                    deps,
                    definitions,
                    requirements,
                    content_updates,
                    (retractions, restate_all),
                    false,
                    false,
                    Some(state),
                    unit_scopes,
                    retained_scopes,
                    &eq,
                    revision,
                    owner,
                    generation,
                    &cancel,
                );
                let published = match committed {
                    Ok(CommitOutcome::Published(committed)) => Some(committed),
                    Ok(CommitOutcome::SelfInvalidated) => None,
                    Err(error) => {
                        crate::general::discard_local_state(node);
                        return Err(error);
                    }
                };
                // The group published either way, so the successor this run
                // staged belongs to that publication. Only an attempt that
                // published nothing is discarded; dropping a published run's
                // state leaves the retry reading the trace of the output it
                // published against the state before it.
                crate::general::commit_local_state(node);
                let Some(committed) = published else {
                    return Err(ComputeError::Retry);
                };
                #[cfg(feature = "observe")]
                let changed_at = self.changed_at(node);
                let version = self.value_version_at(node);
                let routing = self.routing_at(node);
                let validity = self.validity_at(node);
                frame::record_dep(Dep {
                    node,
                    version,
                    routing,
                    validity,
                });
                #[cfg(feature = "observe")]
                self.note_commit(node, changed_at, revision, started);
                #[cfg(not(feature = "observe"))]
                let _ = version;
                Ok(committed)
            }
            Ok(Err(error)) | Err(error) => {
                crate::general::discard_local_state(node);
                if error.is_suspended() || error.is_retry() {
                    return Err(error);
                }
                self.fail(node, error, revision)
            }
        }
    }

    /// Validates a trace in order.
    ///
    /// Returns `true` only when every observation is certified for `revision`
    /// and carries the version recorded at execution time.
    pub(super) fn validate_trace(&self, trace: &[Dep], revision: Revision) -> bool {
        let mut state = self.lock();
        state
            .stats
            .validation_attempts
            .fetch_add(1, Ordering::Relaxed);
        for dep in trace {
            let (verified, clean, changed, routing) = {
                let record = state.node(dep.node);
                (
                    record.verified_at == revision,
                    record.suspect_at <= record.verified_at
                        && record.has_valid_trace()
                        && record.value.is_some(),
                    record.value_version,
                    record.routing_at,
                )
            };
            if !clean {
                state
                    .stats
                    .validation_failures
                    .fetch_add(1, Ordering::Relaxed);
                return false;
            }
            if !verified {
                state.node_mut(dep.node).certify(revision);
            }
            if changed != dep.version || routing != dep.routing {
                state
                    .stats
                    .validation_failures
                    .fetch_add(1, Ordering::Relaxed);
                return false;
            }
        }
        state.stats.validations.fetch_add(1, Ordering::Relaxed);
        true
    }

    /// Whether this attempt still owns the node it is publishing.
    pub(super) fn claim_is_current(
        state: &State,
        node: NodeId,
        owner: TaskId,
        generation: u64,
    ) -> bool {
        matches!(
            state.node(node).state,
            NodeState::Running {
                owner: current_owner,
                generation: current_generation,
            } if current_owner == owner && current_generation == generation
        )
    }

    /// Whether every observation the attempt made is still current.
    pub(super) fn trace_is_current(state: &State, node: NodeId, deps: &[Dep]) -> bool {
        state.next_validity == state.node(node).attempt_validity
            || deps.iter().all(|dep| {
                let record = state.node(dep.node);
                record.value.is_some()
                    && record.has_valid_trace()
                    && record.value_version == dep.version
                    && record.routing_at == dep.routing
            })
    }

    /// Publishes one attempt's value, trace, definitions, and content atomically.
    ///
    /// The commit runs in three phases. The first revalidates the claim with the
    /// lock held and reads what host code needs; the second releases the lock to
    /// run the host's route selector and the value cutoff `eq` — user code never
    /// runs under the metadata lock; the third revalidates and publishes the
    /// whole write group in one critical section.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn commit(
        &self,
        node: NodeId,
        value: Value,
        deps: Vec<Dep>,
        definitions: Vec<DefinitionDraft>,
        requirements: Vec<Requirement>,
        content_updates: Vec<(irrigation_core::ids::ViewId, Arc<dyn Any + Send + Sync>)>,
        definition_mode: (Vec<UnitKey>, bool),
        enforce_views: bool,
        deferrable: bool,
        retained_state: Option<crate::general::ErasedState>,
        unit_scopes: FxHashMap<UnitKey, u64>,
        retained_scopes: FxHashSet<u64>,
        eq: &dyn Fn(&Value, &Value) -> bool,
        revision: Revision,
        owner: TaskId,
        generation: u64,
        cancel: &CancelToken,
    ) -> Result<CommitOutcome> {
        let route_selector = self.route_selector.lock().clone();
        // Phase one: revalidate the claim and read the state host code needs.
        let (previous_value, collisions) = {
            let state = self.lock();
            if !Self::claim_is_current(&state, node, owner, generation) {
                return Err(ComputeError::internal(
                    "commit from a claim that is no longer owned",
                ));
            }
            if cancel.is_cancelled() {
                drop(state);
                return self.fail(node, ComputeError::Cancelled, revision);
            }
            if state.revision != revision {
                drop(state);
                return self.fail(
                    node,
                    ComputeError::internal(
                        "input revision changed while a computation was running",
                    ),
                    revision,
                );
            }
            if !Self::trace_is_current(&state, node, &deps) {
                drop(state);
                return Err(ComputeError::Retry);
            }
            let previous_value = state.node(node).value.clone();
            let collisions = if route_selector.is_some() {
                route_collisions(&state, node, &definitions)
            } else {
                Vec::new()
            };
            (previous_value, collisions)
        };
        // Phase two: host code, without the metadata lock.
        let decisions = decide_routes(route_selector.as_ref(), &collisions);
        let changed = previous_value.as_ref().is_none_or(|old| !eq(old, &value));

        // Phase three: revalidate and publish in one critical section.
        let mut state = self.lock();
        // A commit from a claim that is no longer ours must not publish. This is
        // the generation check the plan requires; it also makes the retirement
        // race harmless if a guard is ever missed.
        if !Self::claim_is_current(&state, node, owner, generation) {
            return Err(ComputeError::internal(
                "commit from a claim that is no longer owned",
            ));
        }
        if cancel.is_cancelled() {
            drop(state);
            return self.fail(node, ComputeError::Cancelled, revision);
        }
        if state.revision != revision {
            drop(state);
            return self.fail(
                node,
                ComputeError::internal("input revision changed while a computation was running"),
                revision,
            );
        }
        // The lock was released in phase two, so every observation is checked
        // again before anything is published. A rejected attempt must not
        // install a definition or replace View content on the way out.
        if !Self::trace_is_current(&state, node, &deps) {
            drop(state);
            return Err(ComputeError::Retry);
        }
        // Everything the install writes or retracts takes a validity stamp
        // above this mark, so a stale observation carrying an older stamp was
        // moved by someone else while this attempt was running.
        let install_mark = state.next_validity;
        // A replacement that stages the content the View already holds installs
        // nothing, so it is not a write: claiming its ports would fail a body
        // over a group that publishes no content at all.
        let content_views: Vec<ViewId> = content_updates
            .iter()
            .filter(|(view, content)| {
                state.views.get(view).is_some_and(|descriptor| {
                    let committed = state
                        .regions
                        .get(view)
                        .is_some_and(|region| (descriptor.content.eq)(&region.content, content));
                    !committed
                })
            })
            .map(|(view, _)| *view)
            .collect();
        let group = WriteGroup {
            provider: node,
            drafts: &definitions,
            retractions: &definition_mode.0,
            restate_all: definition_mode.1,
            content_views: &content_views,
        };
        // A computation that tracked-read a port its own write group installs
        // is self-dependent: the install invalidates the read the write was
        // computed from, so the demand either re-executes against the value
        // this commit just published or exhausts the retry budget. The plan
        // makes that a program error rather than a retry, and the read may sit
        // in a different site node of the same attempt, so it is decided here
        // over the attempt's whole site trace.
        if let Some(port) = self_dependent_port(&state, node, &deps, group) {
            let error = ComputeError::PreconditionUnmet {
                condition: String::from("writer reads a port it writes"),
                ports: vec![format!("{port:?}")],
                reason: irrigation_core::error::PreconditionReason::SelfDependent,
            };
            drop(state);
            return self.fail(node, error, revision);
        }
        // Structural requirements are checked at commit. A false requirement
        // either defers the whole write group to the end of the demand or fails
        // the attempt; it never publishes a definition that was not true. A
        // group that cannot be deferred fails here: its retained state and
        // output commit with its definitions or not at all.
        let unmet = requirements
            .iter()
            .find(|requirement| !requirement_holds(&state, requirement, group));
        let deferred = if let Some(requirement) = unmet {
            // A deferred group is drained by the demand task that owns it. An
            // evaluation with no live task — a parallel region's scoped thread —
            // has no such boundary: no close and no drain will ever match its
            // identity, so the group and its claims would stay pending for ever.
            if !deferrable
                || definitions.is_empty()
                || !content_updates.is_empty()
                || !state.tasks.contains_key(&owner)
            {
                let error = precondition_error(requirement);
                drop(state);
                return self.fail(node, error, revision);
            }
            true
        } else {
            false
        };
        // The arbitration event leaves this section with the batch instead of
        // firing inside it: an observer is host code and must not run while the
        // metadata guard is held.
        let (definitions, definition_mode, _requirements, deferred_port) = if deferred {
            let port = definitions
                .first()
                .map_or_else(|| format!("{node:?}"), |draft| format!("{:?}", draft.unit));
            let pending = crate::state::PendingWrite {
                owner: node,
                task: owner,
                revision,
                generation,
                drafts: definitions,
                retractions: definition_mode.0,
                restate_all: definition_mode.1,
                enforce_views,
                requirements,
                trace: deps.clone(),
                claims: state.node(node).claims.clone(),
            };
            state.pending_writes.push(pending);
            state.stats.deferred_writes.fetch_add(1, Ordering::Relaxed);
            #[cfg(feature = "observe")]
            let port = Some(port);
            #[cfg(not(feature = "observe"))]
            let port: Option<String> = {
                let _ = port;
                None
            };
            (Vec::new(), (Vec::new(), false), Vec::new(), port)
        } else {
            (definitions, definition_mode, requirements, None)
        };
        let mut changed_nodes = Vec::new();
        if !deferred {
            // The whole write group is validated before any of it is applied,
            // so a rejected install cannot leave View content behind.
            if let Err(error) = Self::validate_definitions(
                &state,
                node,
                &definitions,
                &definition_mode.0,
                enforce_views,
            ) {
                drop(state);
                return self.fail(node, error, revision);
            }
        }
        // A content replacement joins the same critical section: it is applied
        // only after the definitions were proved installable, and the
        // definitions are installed against the replacement content. It is the
        // same whole-region transition a host `set_content` stages, so both go
        // through the one publisher: content, its version, and the structure
        // derived from it move together, and a replacement of a View the group
        // installs no unit into is still a change its observers see.
        // The units this group drafts are installed immediately after the
        // replacement, so the replacement owes them no generation of their own.
        // The set is the exact units installed: an entity's presence cell does
        // not stand in for a payload the group never staged.
        let installed: FxHashSet<UnitKey> =
            definitions.iter().map(|draft| draft.unit.clone()).collect();
        // The drafts are installed after this replacement, so the content it
        // publishes cannot project them yet: the reconcile that checks a
        // transaction's own writes against its content belongs to the edit
        // path, where the writes and the content commit together.
        for (view, content) in content_updates {
            let eq = state
                .views
                .get(&view)
                .copied()
                .map(|descriptor| descriptor.content.eq);
            let applied = eq.is_some_and(|eq| {
                Self::publish_region_content(
                    &mut state,
                    view,
                    content,
                    eq,
                    revision,
                    CellDelta {
                        installed: &installed,
                        stated: false,
                    },
                    &mut changed_nodes,
                )
            });
            debug_assert!(
                applied,
                "a staged content replacement targets a registered View"
            );
        }
        state.node_mut(node).content_writes = content_views;
        if !deferred {
            Self::install_definitions(
                &mut state,
                node,
                definitions,
                definition_mode,
                revision,
                enforce_views,
                &decisions,
                &mut changed_nodes,
            );
        }

        let old_trace = state.node(node).trace.clone();
        let value_version = changed.then(|| state.alloc_value_version());
        for dep in &old_trace {
            state.remove_dependent(dep.node, node);
        }
        let new_dependencies: FxHashSet<NodeId> = deps.iter().map(|dep| dep.node).collect();
        for dep in &deps {
            state.add_dependent(dep.node, node);
        }
        {
            let record = state.node_mut(node);
            record.value = Some(value.clone());
            record.trace = deps;
            record.state = NodeState::Ready;
            record.certify(revision);
            record.executed_at = revision;
            record.validated_at = revision;
            record.suspect_at = 0;
            record.failure = None;
            if let Some(retained) = retained_state {
                record.general_state = Some(retained);
            }
            record.general_unit_scopes = unit_scopes;
            record.general_retained_scopes = retained_scopes;
            if changed {
                record.changed_at = revision;
                record.value_version = value_version.expect("changed");
            }
        }
        if !deferred {
            state.release_claims(node);
        }
        // The claim published; the task no longer owns it, so a later exit must
        // not release a claim that is now another task's to take.
        forget_claim(&mut state, owner, node);
        state.stats.executions.fetch_add(1, Ordering::Relaxed);
        if changed {
            let dependents = state.node(node).dependents.clone();
            changed_nodes.extend(dependents);
        }
        // Retire observations that the new trace no longer contains. A retired
        // branch must release its definitions and its value, so it re-executes
        // and restages if it becomes reachable again.
        let retired: Vec<NodeId> = old_trace
            .iter()
            .filter(|dep| !new_dependencies.contains(&dep.node))
            .map(|dep| dep.node)
            .collect();
        for retired in retired {
            self.retire_unreachable(&mut state, retired, revision, &mut changed_nodes);
        }
        let mut invalidated = 0;
        if !changed_nodes.is_empty() {
            let started = std::time::Instant::now();
            invalidated = invalidate(&mut state, changed_nodes.iter().copied(), revision);
            state.stats.invalidation_ns.fetch_add(
                started.elapsed().as_nanos() as u64,
                std::sync::atomic::Ordering::Relaxed,
            );
        }
        // A commit that installs a cell its own body observed — `let base =
        // list.to_vec(); list.insert_at(base.len(), ..)` — invalidates the
        // trace it is publishing. That read was of the pre-write state, which
        // is the from-scratch answer for this attempt; re-running the body
        // would apply the write to the state this commit just published.
        // Re-certify instead, and retry only for an observation that moved
        // before this commit began installing.
        let external = Self::trace_moved_before(&state, node, install_mark);
        if !external {
            let record = state.node_mut(node);
            record.certify(revision);
            record.suspect_at = 0;
        }
        let seeds = changed_nodes;
        let mut waiters = Self::wake_waiters(&mut state, node);
        drop(state);
        #[cfg(feature = "observe")]
        if let Some(port) = deferred_port {
            self.emit(crate::observe::RuntimeEvent::Arbitration {
                kind: "deferred_write",
                node,
                port,
            });
        }
        #[cfg(not(feature = "observe"))]
        let _ = deferred_port;
        #[cfg(feature = "observe")]
        self.emit_invalidated(revision, &seeds, invalidated);
        #[cfg(not(feature = "observe"))]
        let _ = (invalidated, seeds);
        self.wake.notify_all();
        self.enqueue(std::mem::take(&mut waiters.tasks));
        waiters.wake();
        self.emit_woken(std::mem::take(&mut waiters.woken));
        if external {
            return Ok(CommitOutcome::SelfInvalidated);
        }
        Ok(CommitOutcome::Published(value))
    }

    /// Whether the attempt observed a unit that moved before this commit began
    /// installing.
    ///
    /// The trace was certified under this lock before the install, so an
    /// observation that no longer matches its recorded version was moved either
    /// by an earlier commit — the attempt must re-run against that state — or by
    /// this commit's own install. Every cell the install writes or retracts, and
    /// every dependent the install invalidates, takes a validity stamp above
    /// `install_mark`, so an observation still carrying an older stamp is not
    /// this attempt's to re-certify.
    pub(super) fn trace_moved_before(state: &State, node: NodeId, install_mark: u64) -> bool {
        state.node(node).trace.iter().any(|dep| {
            let record = state.node(dep.node);
            record.suspect_validity <= install_mark
                && (record.value_version != dep.version
                    || record.routing_at != dep.routing
                    || !record.has_valid_trace())
        })
    }
}
