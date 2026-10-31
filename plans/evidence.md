# Evidence index

Every claim in `implementation-status.md` points at a test or an example. This
file lists them so a reviewer can run one command per claim.

```sh
cargo test --workspace                # everything below
cargo test -p reactive --test gates   # acceptance gates
cargo test -p reactive-runtime --test kernel   # kernel semantics
cargo test -p reactive --test end_to_end       # macro surface
cargo test -p reactive --test tree_view        # tree family
cargo test -p reactive --test suspension       # suspended demand
```

## Suspension (§8.5–§8.7)

| Claim | Test |
|---|---|
| A demand waiting on another task's node yields its worker, so queued work starts and the blocked computation observes it | `a_waiting_demand_yields_its_worker_to_queued_work` |
| Cancellation reaches a suspended task and it publishes nothing | `a_suspended_demand_can_be_cancelled` |
| Many concurrent demands share one invocation across suspends and wake-ups, with identical results | `concurrent_demands_share_an_invocation_across_suspensions` |
| Progress accounting tracks a running demand and drains to zero when it completes | `progress_accounting_shows_and_drains_in_flight_demands` |
| A writer waits for demands that hold read permits | `gate::tests::a_writer_waits_for_every_reader` |
| A read permit can be released by a different thread | `gate::tests::a_permit_can_be_released_by_another_thread` |

## Semantic kernel (§4, §8)

| Claim | Test |
|---|---|
| First read evaluates required bodies once | `first_read_evaluates_every_required_body_once` |
| Repeated reads reuse a certified revision | `repeated_reads_reuse_the_certified_revision` |
| No-op edits publish no revision and execute nothing | `no_op_edit_publishes_no_revision_and_executes_nothing` |
| Unrelated inputs do not rerun the observing body | `unrelated_input_edit_does_not_rerun_the_observing_body` |
| Branch switching replaces the active dependency set | `switching_a_branch_replaces_the_active_dependency_set` |
| Equal component output stops downstream execution | `equal_component_output_stops_downstream_execution` |
| Invocation identity distinguishes transient arguments | `transient_arguments_identify_distinct_invocations` |
| Snapshots pin their revision | `snapshot_content_is_pinned_to_its_revision`, `snapshots_keep_their_revision_after_edits` |
| Equal output still replaces the trace | `an_equal_output_still_replaces_the_dependency_trace` |

## From-scratch agreement (§12, §13)

| Claim | Test |
|---|---|
| 2,000 randomized transactions match a from-scratch oracle | `randomized_transactions_match_from_scratch_evaluation` |
| Fixed points match a from-scratch solver across insert/delete | `fixed_points_inside_components_follow_insertions_and_deletions` |
| Incremental join matches a from-scratch join | `incremental_join_matches_from_scratch_and_touches_only_changed_keys` |
| Reclamation preserves results | `abandoned_branches_are_reclaimed_without_changing_results` |

## Views (§5)

| Claim | Test |
|---|---|
| Map misses are dependencies; unrelated keys execute nothing | `missing_key_reads_are_dependencies_and_other_keys_are_isolated` |
| Graph payload edits leave topology alone; node deletion retracts edges | `graph_topology_and_payloads_are_independent` |
| Graph neighbours follow edges | `graph_neighbours_follow_edges` |
| Tree kinds, fields and children read independently | `tree_reads_walk_kinds_and_fields` |
| Replacing a tree node preserves identity | `a_replaced_node_keeps_its_identity_and_repairs_consumers` |
| Moving a subtree preserves identities | `moving_a_subtree_preserves_identities` |
| Removing a subtree retracts its nodes | `removing_a_subtree_retracts_its_nodes` |
| Reduced fields track their aggregate | `reduced_sequence_fields_track_their_aggregate` |
| Balanced reduction combines logarithmically | `point_edits_recombine_only_a_logarithmic_path` |

## Definitions and routing (§6)

| Claim | Test |
|---|---|
| Two providers for one unit are rejected | `two_providers_cannot_define_the_same_unit` |
| A dynamic expansion collision is diagnosed | `a_dynamic_expansion_that_collides_is_diagnosed` |
| The supported branch composition republishes values | `branch_chosen_values_republish_every_execution` |
| A handover survives demand order | `provider_handover_survives_demand_order` |
| A handover updates its observer instead of leaving a stale subscription | `a_provider_handover_updates_its_observer` |

## Concurrency and failure (§8)

| Claim | Test |
|---|---|
| Cancelled demands publish nothing | `a_cancelled_demand_publishes_nothing` |
| Cancelling an idle runtime is harmless | `cancelling_an_idle_runtime_is_harmless` |
| A panic fails one scope and recovers next revision | `a_panic_fails_one_scope_and_the_next_revision_recovers` |
| Results are invariant across worker counts | `results_are_invariant_across_worker_counts` |
| Repeated schedules agree | `repeated_schedules_produce_identical_results` |
| Concurrent host reads share one execution | `concurrent_host_reads_share_one_execution` |
| Parallel members share one invocation without deadlock | `parallel_members_share_one_invocation_without_deadlock` |
| Deep recursion is stack safe | `deep_recursion_is_stack_safe` |
| Value cycles are reported, not hung | `a_value_cycle_is_reported_not_hung` |
| A cycle between two tasks is reported, not hung | `a_cycle_between_two_tasks_is_reported_not_hung` |
| A diamond tuple never mixes revisions | `a_diamond_tuple_never_mixes_revisions` |

## Components and APIs (§9, §10)

| Claim | Test |
|---|---|
| Recursive and mutually recursive components are memoized | `recursive_and_mutually_recursive_components_are_memoized` |
| General components initialize, update and commit state | `general_components_initialize_update_and_commit_state` |
| The default reset policy reinitializes | `general_components_default_to_reset_on_invalidation` |
| A switched scope retires its old dependency | `a_switched_scope_retires_its_old_dependency` |
| Async components are driven to completion | `async_components_are_driven_to_completion` |
| Direct demand works without a root | `a_component_can_be_demanded_directly` |
| Runtimes are isolated | `runtimes_are_isolated_namespaces` |
| Snapshots can be taken without a root | `snapshots_can_be_taken_without_a_root` |
| Observers receive changed view versions | `observers_receive_the_changed_view_version` |
| Explanations name the dependency that moved | `explanations_name_the_dependency_that_moved` |
| `Result` and `?` are tracked data | `result_control_flow_is_tracked_like_any_other_value` |

## Scaling (§14)

| Claim | Example |
|---|---|
| Mapped-body executions are independent of list length | `examples/scaling.rs` |
| Balanced reduction combines logarithmically | `examples/reduction.rs` |
| No-op, unrelated-key and tree workloads | `examples/workloads.rs` |
