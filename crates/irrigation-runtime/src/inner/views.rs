//! View registration, routing, snapshots, and observation.
//!
//! Part of the kernel's `impl Inner`; the struct and its lock live in the
//! parent module.

use super::*;

impl Inner {
    /// Returns a strong reference to this runtime.
    #[must_use]
    pub fn arc(&self) -> Arc<Self> {
        self.self_ref
            .get()
            .and_then(Weak::upgrade)
            .expect("runtime inner outlived its strong owner")
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, State> {
        // `state.lock()` is a non-reentrant mutex, so a thread that calls back
        // into the runtime from inside host code running under it would wait on
        // itself forever. Host code does run under it — an argument `PartialEq`
        // during invocation matching, and a unit accessor's initializer — so the
        // case is reachable, and a named refusal beats a silent hang.
        if user_code_depth() > 0 {
            panic_engine(ComputeError::ReentrantUse {
                operation: "runtime accessor",
                reason: "host code is running under the metadata lock and called back into the runtime",
            });
        }
        self.state.lock()
    }

    /// Current revision.
    #[must_use]
    pub fn current_revision(&self) -> Revision {
        self.revision.load(Ordering::Acquire)
    }

    /// Publishes a new revision after an edit transaction.
    pub(crate) fn publish_revision(&self, revision: Revision) {
        self.revision.store(revision, Ordering::Release);
    }

    /// Registers a View descriptor and materializes its region.
    pub fn register_view(&self, descriptor: &'static ViewDescriptor) {
        let mut state = self.lock();
        if let Some(existing) = state.views.get(&descriptor.id) {
            // Identity is the full 64-bit name hash; a collision between two
            // different names is rejected instead of aliasing the namespaces.
            assert!(
                existing.name == descriptor.name,
                "two View names share one ViewId: `{}` and `{}`",
                existing.name,
                descriptor.name
            );
            return;
        }
        state.views.insert(descriptor.id, descriptor);
        let content = (descriptor.content.empty)();
        let content_node = state.alloc(NodeRecord::empty("view content", NodeKind::RegionContent));
        {
            let revision = state.revision;
            let value_version = state.alloc_value_version();
            state.node_mut(content_node).install_value(
                Some(content.clone()),
                value_version,
                revision,
            );
        }
        state.units.insert(
            Arc::new(UnitKey::field(
                descriptor.id,
                irrigation_core::ids::FieldId(0),
                UnitKind::Content,
            )),
            content_node,
        );
        state.regions.insert(
            descriptor.id,
            Region {
                descriptor,
                content,
                content_node,
                units: irrigation_core::key::KeyTable::new(),
                owners: irrigation_core::key::KeyTable::new(),
                routing_version: 0,
            },
        );
    }

    /// Returns the content of a registered View.
    #[must_use]
    pub fn view_content(&self, view: ViewId) -> Option<Arc<dyn Any + Send + Sync>> {
        self.lock()
            .regions
            .get(&view)
            .map(|region| Arc::clone(&region.content))
    }

    /// Returns the descriptor of a registered View.
    #[must_use]
    pub fn view_descriptor(&self, view: ViewId) -> Option<&'static ViewDescriptor> {
        self.lock().views.get(&view).copied()
    }

    /// Allocates or returns the child scope.
    pub fn child_scope(&self, parent: ScopeId, discriminant: u64, _label: &'static str) -> ScopeId {
        self.lock().scopes.child(parent, discriminant)
    }

    /// Whether a field currently uses the keyed derived protocol.
    pub fn field_is_keyed(&self, view: ViewId, field: FieldId) -> bool {
        self.lock().keyed_fields.contains(&(view, field))
    }

    /// Allocates or returns the child scope for a typed key.
    pub fn child_scope_keyed(
        &self,
        parent: ScopeId,
        key: Arc<dyn AnyKey>,
        _label: &'static str,
    ) -> ScopeId {
        self.lock().scopes.child_keyed(parent, key)
    }

    /// Returns (creating on first use) the node for one expression site.
    pub fn site_node(&self, scope: ScopeId, site: SiteId) -> NodeId {
        let mut state = self.lock();
        if let Some(node) = state.sites.get(&(scope, site)) {
            state
                .stats
                .site_reuses
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return *node;
        }
        let node = state.alloc(NodeRecord::site(scope, site));
        state
            .stats
            .site_creations
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        state.sites.insert((scope, site), node);
        node
    }

    /// Returns one View's committed content, or an empty value when it has no
    /// region yet: the caller's initializer decides what that means.
    pub(super) fn region_content(state: &State, view: ViewId) -> Arc<dyn Any + Send + Sync> {
        state.regions.get(&view).map_or_else(
            || Arc::new(()) as Arc<dyn Any + Send + Sync>,
            |region| Arc::clone(&region.content) as Arc<dyn Any + Send + Sync>,
        )
    }

    /// Returns a node's routing generation.
    #[must_use]
    pub fn routing_at(&self, node: NodeId) -> Revision {
        self.lock().node(node).routing_at
    }

    /// Returns the stamp at which the node was last certified.
    #[must_use]
    pub fn validity_at(&self, node: NodeId) -> u64 {
        self.lock().node(node).verified_validity
    }

    /// Returns the monotonic version of the node's value.
    #[must_use]
    pub fn value_version_at(&self, node: NodeId) -> u64 {
        self.lock().node(node).value_version
    }

    /// Returns the provider node that currently owns `unit`, if any.
    #[must_use]
    pub fn provider_of(&self, unit: &UnitKey) -> Option<NodeId> {
        let state = self.lock();
        state
            .regions
            .get(&unit.view)
            .and_then(|region| region.owners.get(&owner_key(unit)))
            .map(|provider| provider.node)
    }

    /// Captures the current provider recipe for one View region.
    #[must_use]
    pub fn routing_recipe(&self, view: ViewId) -> Option<crate::runtime::RoutingRecipe> {
        let state = self.lock();
        let region = state.regions.get(&view)?;
        let mut providers = Vec::new();
        for (key, _) in region.units.iter() {
            let Some(unit) = key.as_any().downcast_ref::<UnitKey>() else {
                continue;
            };
            if let Some(provider) = region.owners.get(&owner_key(unit)) {
                providers.push((unit.clone(), provider.node));
            }
        }
        let planned = state
            .route_plans
            .iter()
            .filter(|(unit, _)| unit.view == view)
            .map(|(unit, component)| (unit.clone(), *component))
            .collect();
        Some(crate::runtime::RoutingRecipe {
            stamp: crate::runtime::RoutingStamp {
                view,
                version: region.routing_version,
            },
            providers,
            planned,
        })
    }

    /// Returns a unit's routing generation.
    #[must_use]
    pub fn routing_generation(&self, unit: &UnitKey) -> Revision {
        let state = self.lock();
        state
            .units
            .get(unit)
            .copied()
            .or_else(|| {
                state
                    .regions
                    .get(&unit.view)
                    .and_then(|region| region.units.get(unit))
                    .map(|slot| slot.node)
            })
            .map_or(0, |node| state.node(node).routing_at)
    }

    /// Returns the revision at which a node's value last changed.
    #[must_use]
    pub fn changed_at(&self, node: NodeId) -> Revision {
        self.lock().node(node).changed_at
    }

    /// Runs `body` with one View's region temporarily out of the map.
    ///
    /// The region has to leave the map because the mutation needs the rest of
    /// the state as well. A View's content vtable is the one host callback that
    /// still runs under the metadata lock — it is confined to the descriptor's
    /// pure content operations, and every function that reaches it goes through
    /// here — so a panic in it must not leave the View without its data: the
    /// region goes back before the panic continues unwinding. Route selection
    /// and the value cutoff `eq` are resolved before this point, without the
    /// lock.
    pub(super) fn with_taken_region<R>(
        state: &mut State,
        view: ViewId,
        body: impl FnOnce(&mut State, &mut Region) -> R,
    ) -> Option<R> {
        let mut region = state.regions.remove(&view)?;
        let outcome = catch_engine(std::panic::AssertUnwindSafe(|| body(state, &mut region)));
        let replaced = state.regions.insert(view, region);
        debug_assert!(
            replaced.is_none(),
            "a View region was replaced while it was taken"
        );
        Some(outcome.unwrap_or_else(|error| panic_engine(error)))
    }

    /// Allocates a fresh entity identity.
    pub fn alloc_entity(&self) -> u64 {
        self.lock().alloc_entity()
    }

    /// Demands the installed root at the current revision.
    /// Demands one root output projection without evaluating its siblings.
    pub fn demand_root_projection(&self, cancel: &CancelToken, index: usize) -> Result<Value> {
        let revision = self.current_revision();
        let (root, general_root) = {
            let state = self.lock();
            (state.root.clone(), state.general_root)
        };
        if let Some(root) = root {
            let Some(node) = root.outputs.get(index).copied() else {
                return Err(ComputeError::internal("projection index out of range"));
            };
            let runtime: Arc<dyn RuntimeOps> = self.arc();
            // Frames are thread-local execution state; see the note in `eval`.
            #[allow(clippy::arc_with_non_send_sync)]
            let scope_frame = Arc::new(Frame::scoped(
                Arc::clone(&runtime),
                NodeId::NONE,
                ScopeId::ROOT,
                0,
                cancel.clone(),
            ));
            let _guard = FrameGuard::push(scope_frame);
            let projection_eq = root.projection_eq;
            let value = catch_engine(|| {
                let body_value = crate::component::eval_value(
                    &runtime,
                    root.body,
                    || (root.run)(),
                    &root.body_eq,
                );
                crate::component::eval_value(
                    &runtime,
                    node,
                    || {
                        runtime
                            .observe(root.body)
                            .unwrap_or_else(|error| irrigation_core::ops::panic_engine(error));
                        (root.project)(&body_value, index)
                    },
                    &|left, right| projection_eq(index, left, right),
                )
            })?;
            self.finish_demand(revision, &[root.body])?;
            return Ok(value);
        }
        let Some(general_node) = general_root else {
            return Err(ComputeError::internal(
                "no root component installed; call `runtime.install` first",
            ));
        };
        if index != 0 {
            return Err(ComputeError::internal("projection index out of range"));
        }
        let runtime: Arc<dyn RuntimeOps> = self.arc();
        #[allow(clippy::arc_with_non_send_sync)]
        let scope_frame = Arc::new(Frame::scoped(
            Arc::clone(&runtime),
            NodeId::NONE,
            ScopeId::ROOT,
            0,
            cancel.clone(),
        ));
        let _guard = FrameGuard::push(scope_frame);
        let mut compute = || -> Result<Value> {
            Err(ComputeError::internal(
                "a general component does not use a compute closure",
            ))
        };
        let eq = {
            let state = self.lock();
            let general = state
                .node(general_node)
                .general
                .map(|index| Arc::clone(&state.generals[index]));
            move |left: &Value, right: &Value| {
                general
                    .as_ref()
                    .is_some_and(|general| general.output_eq(left, right))
            }
        };
        let value = self.eval(general_node, &mut compute, &eq)?;
        self.finish_demand(revision, &[general_node])?;
        Ok(value)
    }

    /// Demands the installed root and evaluates every output projection.
    pub fn demand_root(&self, cancel: &CancelToken) -> Result<()> {
        let revision = self.current_revision();
        let root = {
            let state = self.lock();
            state.root.clone()
        };
        let Some(root) = root else {
            let general_root = {
                let state = self.lock();
                state.general_root
            };
            let Some(general_node) = general_root else {
                return Err(ComputeError::internal(
                    "no root component installed; call `runtime.install` first",
                ));
            };
            let runtime: Arc<dyn RuntimeOps> = self.arc();
            // Frames are thread-local execution state; see the note in `eval`.
            #[allow(clippy::arc_with_non_send_sync)]
            let scope_frame = Arc::new(Frame::scoped(
                Arc::clone(&runtime),
                NodeId::NONE,
                ScopeId::ROOT,
                0,
                cancel.clone(),
            ));
            let _guard = FrameGuard::push(scope_frame);
            let mut compute = || -> Result<Value> {
                Err(ComputeError::internal(
                    "a general component does not use a compute closure",
                ))
            };
            let eq = {
                let state = self.lock();
                let general = state
                    .node(general_node)
                    .general
                    .map(|index| Arc::clone(&state.generals[index]));
                move |left: &Value, right: &Value| {
                    general
                        .as_ref()
                        .is_some_and(|general| general.output_eq(left, right))
                }
            };
            self.eval(general_node, &mut compute, &eq)?;
            return self.finish_demand(revision, &[general_node]);
        };
        let runtime: Arc<dyn RuntimeOps> = self.arc();
        // Frames are thread-local execution state; see the note in `eval`.
        #[allow(clippy::arc_with_non_send_sync)]
        let scope_frame = Arc::new(Frame::scoped(
            Arc::clone(&runtime),
            NodeId::NONE,
            ScopeId::ROOT,
            0,
            cancel.clone(),
        ));
        let _guard = FrameGuard::push(scope_frame);
        catch_engine(|| {
            let body_value =
                crate::component::eval_value(&runtime, root.body, || (root.run)(), &root.body_eq);
            for (index, node) in root.outputs.iter().enumerate() {
                let projection_eq = root.projection_eq;
                crate::component::eval_value(
                    &runtime,
                    *node,
                    || {
                        runtime
                            .observe(root.body)
                            .unwrap_or_else(|error| irrigation_core::ops::panic_engine(error));
                        (root.project)(&body_value, index)
                    },
                    &|left, right| projection_eq(index, left, right),
                );
            }
        })?;
        // A child commit can retire an old branch and invalidate the root after
        // the root itself published. The demand is not allowed to carry that
        // stale value out; force the root to re-execute against the new state.
        let root_dirty = {
            let state = self.lock();
            !state.node(root.body).has_valid_trace()
        };
        if root_dirty {
            force_recompute(&mut self.lock(), &[root.body], revision);
            return Err(ComputeError::Retry);
        }
        self.finish_demand(revision, &[root.body])
    }

    /// Captures every registered View's content at the current revision.
    #[must_use]
    pub fn capture_snapshot(&self) -> crate::snapshot::Snapshot {
        let state = self.lock();
        Self::snapshot_of(&state)
    }

    /// Captures the committed content and runs `stage` against the same state.
    ///
    /// One locked pass reads both halves of a consumer's read pair — the View
    /// content and the unit versions that decide whether it moved — so a
    /// concurrent publish cannot land between them. The caller still owns the
    /// revision pin: only the gate keeps an edit from committing between the
    /// demand that produced the content and this capture. The snapshot is
    /// shared with `stage` because every delivery carries it.
    pub(crate) fn capture_snapshot_with<T>(
        &self,
        stage: impl FnOnce(&mut State, &Arc<crate::snapshot::Snapshot>) -> T,
    ) -> T {
        let mut state = self.lock();
        let snapshot = Arc::new(Self::snapshot_of(&state));
        stage(&mut state, &snapshot)
    }

    pub(super) fn snapshot_of(state: &State) -> crate::snapshot::Snapshot {
        let revision = state.revision;
        let mut views: FxHashMap<ViewId, Arc<dyn Any + Send + Sync>> = FxHashMap::default();
        let mut versions: FxHashMap<ViewId, Version> = FxHashMap::default();
        let mut routing: FxHashMap<ViewId, u64> = FxHashMap::default();
        for (id, region) in &state.regions {
            views.insert(*id, Arc::clone(&region.content));
            versions.insert(*id, state.node(region.content_node).changed_at);
            routing.insert(*id, region.routing_version);
        }
        crate::snapshot::Snapshot::new(revision, views, versions, routing)
    }

    /// Number of worker threads.
    #[must_use]
    pub fn workers(&self) -> usize {
        self.pool.workers()
    }

    /// Whether an installed observer wants events at this level.
    ///
    /// The level is the observer's own declaration, so the kernel is the side that
    /// has to honour it: an event below it is not delivered, and a call site that
    /// has to build a payload to emit one asks this first. Without it, `Full`-only
    /// detail was built for every observer and filtered by the consumer.
    #[cfg(feature = "observe")]
    pub(crate) fn observes(&self, level: crate::observe::ObserveLevel) -> bool {
        self.observer
            .get()
            .is_some_and(|observer| observer.level() >= level)
    }

    /// Emits one observation event when an observer is installed.
    ///
    /// The level is not checked here: `Runtime::with_observer` wraps the host
    /// observer in a `LevelFilter` that owns that rule for delivery. What the
    /// kernel owes is the other half -- not *building* a payload the filter will
    /// drop -- which is what [`Self::observes`] is for at the expensive sites.
    #[cfg(feature = "observe")]
    pub(crate) fn emit(&self, event: crate::observe::RuntimeEvent) {
        if let Some(observer) = self.observer.get() {
            observer.record(event);
        }
    }

    /// Emits the wake events collected while the state lock was held.
    ///
    /// This runs only after the batch reached the scheduler: an observer is
    /// host code and can panic, and that panic must not drop a wake or the gate
    /// permit the converting task still holds.
    pub(crate) fn emit_woken(&self, woken: Vec<(TaskId, NodeId)>) {
        #[cfg(feature = "observe")]
        for (task, node) in woken {
            self.emit(crate::observe::RuntimeEvent::TaskWoken { task, node });
        }
        #[cfg(not(feature = "observe"))]
        let _ = (self, woken);
    }

    /// Emits an invalidation event for one reverse-dependency walk.
    #[cfg(feature = "observe")]
    pub(crate) fn emit_invalidated(&self, revision: Revision, seeds: &[NodeId], count: u64) {
        let seeds: smallvec::SmallVec<[NodeId; 4]> = seeds.iter().copied().take(4).collect();
        self.emit(crate::observe::RuntimeEvent::Invalidated {
            revision,
            seeds,
            count,
        });
    }

    /// Emits the commit event for one executed node.
    #[cfg(feature = "observe")]
    pub(crate) fn note_commit(
        &self,
        node: NodeId,
        version: Version,
        revision: Revision,
        started: std::time::Instant,
    ) {
        // The payload below walks the node's definitions under the metadata lock;
        // an observer that does not want node-level detail should not pay for it.

        if !self.observes(crate::observe::ObserveLevel::Nodes) {
            self.lock()
                .stats
                .observe_payloads_skipped
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return;
        }
        let (views, equal_views) = {
            let state = self.lock();
            let mut views: smallvec::SmallVec<[irrigation_core::ids::ViewId; 4]> =
                smallvec::SmallVec::new();
            let mut equal_views: smallvec::SmallVec<[irrigation_core::ids::ViewId; 4]> =
                smallvec::SmallVec::new();
            let Some(record) = state.nodes.get(node.index()) else {
                return;
            };
            for definition in &record.definitions {
                let target = if state
                    .units
                    .get(&definition.unit)
                    .is_some_and(|value_node| state.node(*value_node).changed_at == revision)
                {
                    &mut views
                } else {
                    &mut equal_views
                };
                if target.len() < 4 && !target.contains(&definition.unit.view) {
                    target.push(definition.unit.view);
                }
            }
            (views, equal_views)
        };
        self.emit(crate::observe::RuntimeEvent::NodeCommitted {
            node,
            views,
            equal_views,
            changed: version == revision,
            invalidated: 0,
            definitions: 0,
            duration_ns: started.elapsed().as_nanos() as u64,
        });
    }

    /// Records an observer-refresh failure without failing the transaction.
    pub(crate) fn set_notification_error(&self, error: ComputeError) {
        self.lock().notification_error = Some(error);
    }

    /// Clears the observer-refresh failure after a successful refresh.
    pub(crate) fn clear_notification_error(&self) {
        self.lock().notification_error = None;
    }

    /// Returns `true` when a root component (ordinary or general) is installed.
    /// Marks the most recently started demand as cancelled, if there is one.
    pub fn cancel_active(&self) {
        let task = *self.active_task.lock();
        let Some(task) = task else {
            return;
        };
        let queued = {
            let mut state = self.lock();
            let Some(slot) = state.tasks.get_mut(&task) else {
                return;
            };
            slot.cancel.cancel();
            let wake = matches!(slot.status, TaskStatus::Waiting(_));
            let pooled = slot.mode == TaskMode::Pooled;
            if wake {
                slot.status = TaskStatus::Queued;
            }
            if wake {
                state.wait_graph.remove(&task);
            }
            (wake && pooled).then_some(task)
        };
        self.wake.notify_all();
        if let Some(task) = queued {
            self.enqueue(vec![task]);
        }
    }

    /// Records the demand that a later [`Inner::cancel_active`] targets.
    pub(crate) fn set_active_task(&self, task: Option<TaskId>) {
        let mut active = self.active_task.lock();
        *active = task;
    }
}
