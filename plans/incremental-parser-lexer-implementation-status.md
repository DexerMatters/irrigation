# Incremental Parser and Lexer: implementation status

Source plan: [`incremental-parser-lexer-plan.md`](incremental-parser-lexer-plan.md).

Status: **in progress**. The crate builds and the text/lexer/LR/stack-GLR,
prefix/suffix-incremental-parser, byte-complete CST, and basic fast-recovery
layers have executable acceptance tests. A bounded GSS batch frontier exists
with a byte-complete CST and skip/missing recovery, and is the default batch
frontier. GSS prefix checkpoints (single- and multi-path) feed incremental
parsing; RNGLR-specific reductions and CPCT+-style complete repair search are
not implemented yet. This file is deliberately conservative: a feature is
listed only when a test exercises its current contract.

## Implemented

| Area | Evidence |
|---|---|
| `reactive-syntax` crate wired into the workspace | `cargo test -p reactive-syntax` |
| Versioned text snapshots, anchors, edit maps, UTF-8 boundary-safe coarse diffs | `text::tests::*`, `anchors_resolve_forward_across_edits`, `edits_since_long_chain_is_a_coarse_replacement` |
| Grammar `.grammar` scanner shared by lexer/parser | `lexer::spec`, parser DSL tests |
| Lexer modes, value stack, guards, actions, zero-width tokens, error tokens | `stateful_tag_matching`, `indentation_tokens_are_zero_width_and_generic`, `unknown_character_produces_an_error_token_and_diagnostic` |
| Incremental lexing with token identity reuse and reconvergence | `incremental_lex_matches_from_scratch_and_reuses_suffix`, `randomized_incremental_lexing_matches_from_scratch` (300 edits) |
| Parser grammar DSL, fields, EBNF `* + ?`, groups, alternatives | JSON/expression parser tests |
| LALR(1) default table construction by merging canonical LR(1) cores | expression/JSON parser tests |
| Optional canonical LR(1) tables via `table = lr1;` | `grammar_can_select_lalr_and_lr1_tables` |
| LALR/LR(1) language-corpus agreement | `lalr_and_lr1_accept_the_same_corpus` |
| Nullable grammar termination and parsing | `nullable_grammar_terminates_and_parses` |
| Nullable parser agrees with an Earley recognizer | `nullable_parser_matches_an_earley_recognizer` |
| Right-nullable analysis (`nullable_nonterminals`, `is_right_nullable`, `rn_reduction_prefixes`, `rnglr_reductions`) | `right_nullable_suffixes_are_identified` |
| Mutually recursive nullable rule semantics | `mutually_recursive_nullable_rules_terminate` |
| GSS right-nullable prefix reductions | `rn_reduction_prefixes` drive alternate GSS reductions; nullable tests pass |
| GSS and stack agree on nullable grammar trees | `gss_and_stack_agree_on_a_nullable_grammar` |
| Conflict errors suggest precedence or conflict declarations | `table_conflicts_suggest_precedence_or_declaration` |
| IELR table-mode fallback | `grammar_can_select_lalr_and_lr1_tables` also checks `ielr` equals canonical LR(1) state count |
| Nullable-cycle rejection with rule-offset diagnostics | `nullable_cycles_are_rejected` |
| Precedence/associativity conflict resolution | `precedence_resolves_expression_conflicts` |
| Declared conflicts retained as multiple table actions | `declared_conflict_is_explored_by_glr` |
| Stack-version GLR exploration and dynamic-precedence selection | ambiguous-expression tests |
| Batch LR/GLR parse to a persistent CST with fields and token leaves | `parses_json_object`, `parser_attaches_fields_to_pair_children` |
| Deterministic frontier checkpoints for incremental prefix reuse | `Parser::parse_state`, `ParserState::checkpoints` |
| Incremental prefix reuse with stable reused `NodeId`s | `incremental_parser_reuses_a_prefix_and_matches_batch_structure` |
| Suffix reuse of unchanged visible subtrees | `incremental_parser_reuses_an_unchanged_suffix_subtree` |
| Incremental/batch structural agreement across edit shapes | `incremental_parser_matches_batch_across_valid_edit_shapes` |
| Randomized incremental parser oracle | `randomized_incremental_parser_matches_batch_on_valid_edits` (240 edits) |
| Arbitrary malformed-input oracle | `arbitrary_malformed_inputs_never_panic` (120 random inputs) |
| Parser and lexer cargo-fuzz targets | `crates/reactive-syntax/fuzz/fuzz_targets/*` |
| Batch/incremental timing harness | `cargo run -p reactive-syntax --example bench` (2,000-pair JSON: batch ~560 ms, incremental ~147 ms in debug) |
| Criterion benchmark matrix | `cargo bench -p reactive-syntax --bench parser` (batch/incremental at 50/500/2,000 pairs plus lexing; after the persistent DFA cache, 23,781-byte lexing is ~2.06 ms and incremental 2,000-pair parsing is ~56.7 ms) |
| Syntax CI workflow | `.github/workflows/syntax.yml` (format, tests, bench build) |
| Scheduled fuzz smoke workflow | `.github/workflows/syntax-fuzz.yml` (weekly/manual parser and lexer cargo-fuzz runs) |
| Exact-literal lexer fast path | `lexer::pattern::tests::exact_literals_use_the_fast_path` |
| Conflicting literal lexer rules diagnosed | `conflicting_literal_rules_are_diagnosed` |
| Conservative first-byte lexer filter | `lexer::pattern::tests::first_byte_filter_is_conservative` |
| Capture-free NFA pattern simulation | all lexer and randomized lexing tests |
| Lazy DFA construction over the NFA, cached per pattern | `lexer::pattern::tests::dfa_and_backtracking_agree_on_end_positions`; parser suite drops to ~0.39 s |
| GSS batch frontier gates | `gss_frontier_matches_the_stack_nontrivia_signature_on_json`, nullable, conflict, precedence, recovery, fallback, budget, byte-completeness, and default-frontier tests; GSS diagnostics are deduplicated |
| GSS prefix checkpoints | `default_gss_state_retains_prefix_checkpoints`, `ambiguous_gss_state_retains_multiple_checkpoint_paths`, default-GSS incremental prefix/suffix tests |
| Unique token identities in an incremental tree | `incremental_tree_token_ids_are_unique` |
| Canonical token IDs for unchanged prefix/suffix tokens | `incremental_parse_reuses_unchanged_token_ids` |
| Correct left-edge parser state on CST nodes | suffix reuse gate exercises `NodeMeta::start_state`/`end_state` |
| Right-context hashes on CST nodes | suffix reuse rejects candidates whose following-terminal hash differs |
| Hidden-span reuse from checkpoint stack entries | `hidden_suffix_spans_are_reused_from_checkpoints` |
| Reused-span observability | `ParseOutput::reused_spans` |
| Byte-complete CST with trivia attached as extra token children | `cst_covers_every_lexed_token_and_the_full_source_range` |
| Reactive `Parser` adapter over a `SourceFile` View | `parser_component_reads_and_updates_the_source_view` |
| Reactive `Lexer` adapter over a `SourceFile` View | `lexer_component_reads_the_source_view` |
| Optional `Syntax` view with per-node membership/kind/span/children/parent, token text/children, and field payload units | `syntax_view_exposes_per_node_units` |
| `Syntax` retraction of removed node identities | `syntax_view_retracts_removed_nodes` |
| Explicit `ERROR` root/path for invalid input (no parser panic) | `invalid_input_produces_an_error_tree_not_a_panic` |
| Fast recovery: missing-token insertion | `missing_token_recovery_inserts_a_colon` |
| Fast recovery: skipped-token `ERROR` nodes | `skipped_token_recovery_wraps_the_token_in_an_error_node` |
| Minimum-cost ranking and error budget for recovered parses | `recovery_cost_budget_can_disable_repairs`, recovery-cost assertions |
| Recover-to-state by closing an incomplete construct | `recover_to_state_closes_an_unmatched_bracket` |
| GSS accepted-root ranking by `ERROR`/`MISSING` count | GSS recovery tests; min-error roots feed `ParseOutput::repairs` |
| Adjacent skipped-token `ERROR` nodes merged | `adjacent_skipped_errors_are_merged` |
| Recovery/error trees cover all input tokens without duplicates | `recovery_tree_covers_all_input_tokens_without_duplicates` |
| Minimum-cost alternative repair roots exposed on `ParseOutput` | `recovery_exposes_minimum_cost_repair_roots` |
| Bounded breadth-first multi-token repair search | `complete_repair_search_finds_one_edit_fixes`, `complete_repair_search_finds_two_edit_fixes` |
| Repair-search candidate budget | `ParserOptions::max_repair_candidates`, `repair_search_honors_the_candidate_budget` |
| Exhaustive small-input repair mode | `ParserOptions::complete_repair_search`, `exhaustive_repair_search_disables_the_candidate_cap` |
| Weighted Dijkstra repair search | `ParserOptions::repair_insert_cost`/`repair_delete_cost`/`repair_replace_cost`, `weighted_repair_search_prefers_cheap_edits` |
| Repair edit-cost exposure | `parse_complete_repairs_with_cost`, `repair_search_reports_edit_cost` |
| One-edit repair search matches brute force | `one_edit_search_matches_bruteforce_on_a_small_input` |
| Two-edit repair search matches brute force | `two_edit_search_matches_bruteforce_on_a_small_input` |
| Three-edit repair search | `repair_search_can_find_three_edits` |
| Stable grammar/field names across compiles | `grammar_names_are_stable_across_compiles` |
| Stack-frontier configuration merging by state/index/cost | all recovery and incremental tests; parser suite runtime drops from ~1.05 s to ~0.71 s |
| Deterministic repair deduplication | repairs are signature-sorted and deduplicated in stack and GSS paths; `repair_sets_are_deterministic_across_parses` |

## Not implemented yet

| Plan area | Current state |
|---|---|
| IELR(1) tables | `table = ielr;` is accepted using canonical LR(1) states as the documented future-compatible fallback; true IELR minimization remains an optional future optimization |
| GSS batch frontier | default for `Parser::parse`; shares `(state, token-index)` nodes and predecessor edges, packs local ambiguity, supports nullable/declared-conflict grammars, attaches trivia for a byte-complete CST, and has bounded skip/missing recovery (`gss_frontier_*` tests). Nullable recursion uses bounded cyclic reduction paths. Single- and multi-path GSS checkpoints feed prefix-incremental parsing. Recovery is ranked by the minimum count of `ERROR`/`MISSING` nodes in accepted roots, and diagnostics are deduplicated. |
| RNGLR right-nullable reductions | table generation records `RnReduction` entries with prefix/suffix lengths, installs `ReduceRn` actions through conflict resolution, and stack/GSS runtime paths materialize empty visible suffix nodes (`rn_reductions_materialize_empty_suffix_nodes`) |
| Incremental parser reuse certificate | GSS prefix checkpoints (single- and multi-path), visible and hidden suffix reuse, matching left/right lexer states, canonical unchanged token IDs, right-context hashes, and fragile marking for declared-conflict grammars. Failed incremental parses fall back to a batch parse. |
| Full fast recovery tier | missing-token/skip/recover-to-state branches are ranked by minimum error cost and bounded by `ParserOptions::max_error_cost`; adjacent skipped errors are merged, and GSS roots are ranked by `ERROR`/`MISSING` count |
| Complete CPCT+ repair search | bounded breadth-first enumeration, exhaustive small-input mode, configuration merging, one-/two-edit brute-force equivalence, weighted Dijkstra costs, and deterministic deduplicated repair sets are implemented |
| Randomized parser oracle, fuzz targets, benchmark matrix | parser oracle, cargo-fuzz targets, criterion matrix, syntax CI, and a scheduled fuzz-smoke workflow exist |
| Lexer pattern fast path | exact literals bypass matching, capture-free patterns use a lazily constructed DFA over an NFA, and every rule has a conservative first-byte filter; DFA states are cached per match, not pre-minimized |

## Commands

```sh
cargo test -p reactive-syntax
cargo test --workspace
```

## Next implementation slice

1. Add RNGLR-specific right-nullable handling and improve GSS recovery
   cost/diagnostic ranking.
2. Complete the reuse certificate with any remaining old-tree-input-stream
   edge cases; hidden spans, right-context hashes, GLR checkpoints, and token-ID
   reconciliation are in place.
3. Add CPCT+-style complete repair search and brute-force equivalence on top of
   the existing bounded search and configuration merging.
4. Add CI fuzz runs and the general DFA lexer path.
