# Incremental Parser and Lexer adapters

**Status:** historical. The CST-based lexer/parser described here was replaced by
the AST-only pipeline of
[`ast-incremental-granularity-plan.md`](ast-incremental-granularity-plan.md); the
CST modules are removed. Kept as the record of the original design. See
[`incremental-parser-lexer-implementation-status.md`](incremental-parser-lexer-implementation-status.md).

**Research basis:** primary papers and implementation sources listed in §11;
the tree-sitter claims were checked against `parser.c`, `stack.c`,
`subtree.h`, and `error_costs.h`, not only against its documentation.

**Scope:** two predefined adapters for the reactive runtime, named exactly
`Parser` and `Lexer`:

* `Lexer` — a general, stateful, incremental scanner with error tokens and a
  declarative token specification language;
* `Parser` — an incremental generalized-LR parser with error recovery, a
  declarative grammar specification language, and stable syntax-node identity.

The parser is GLR, not deterministic LR. It uses the right-nulled GLR
(RNGLR) variant for nullable grammars, and the incremental generalized-LR
(IGLR) strategy described by Wagner for subtree reuse. Error recovery combines
the online recovery machinery of tree-sitter with the complete minimum-cost
repair search of CPCT+. Neither adapter is a core-runtime feature: both are
implemented as `GeneralComponent`s, so the runtime's scheduling, snapshot,
dependency, cancellation, and atomic-publication rules still hold.

This document is a plan, an algorithm description, and an acceptance spec. It
does not claim measurements that were not taken. The research sources and the
parts of each source that were actually used are listed in §2 and §11.

**Relationship to the existing plan documents:**

* [`concurrent-incremental-system-plan.md`](concurrent-incremental-system-plan.md)
  §10.5 defines `GeneralComponent` as the escape hatch for algorithms with
  retained state, and explicitly names an incremental parser as an example.
* [`../docs/architecture.md`](../docs/architecture.md) describes the unit,
  revision, demand, cutoff, and atomic-definition rules that these adapters
  must obey.
* [`heterogeneous-granularity-plan.md`](heterogeneous-granularity-plan.md)
  establishes the per-entity identity discipline that the optional
  `Syntax` view in §3.7 extends to syntax nodes.
* [`implementation-status.md`](implementation-status.md) records what the
  runtime currently implements. This plan deliberately depends on only the
  implemented `GeneralComponent`, `Edit`, `Snapshot`, and definition APIs,
  with the small additions called out in §3.4 and §3.6.

---

## 0. Short version

1. **One grammar file, two generated products.** A `.grammar` file contains a
   lexer block (states, rules, guards, stack actions) and a parser block
   (EBNF productions, fields, precedence, explicit conflicts). `Grammar::compile`
   validates both, lowers EBNF to BNF, builds the lexer DFAs, builds LALR(1)
   tables plus RN reductions, and checks every declared conflict. The same
   file is the documentation, the test input, and the generated runtime data.
2. **Lexer = deterministic pushdown transducer.** A lexer has named modes and
   a persistent value stack. A rule may inspect the matched text, the current
   column, the top of the value stack, and whether the input is at EOF; its
   action may switch or push a mode and push/pop a value. There are no built-in
   indentation or XML rules: indentation is a comparison of `column` with a
   value; XML matching is equality of a captured tag name with a value.
   Maximal munch, declared priorities, and zero-width-token progress rules are
   generic. Every token stores its start state and end state; this is what makes
   relexing a suffix sound.
3. **Incremental lexing = relex the window, then reconverge.** Edits invalidate
   the token whose span or lookahead window touches the edit; relexing starts
   at the earliest such token and stops as soon as the new lexer state equals
   the old state at an old token boundary whose suffix is unchanged. Old tokens
   are reused by identity when kind, lexeme, and end state agree.
4. **Parser = RNGLR with a reuse certificate.** The parser uses a graph-
   structured stack (GSS), local ambiguity packing, LALR(1) tables with
   explicitly declared conflicts, and right-nulled reductions for nullable
   suffixes. A subtree is reused only if it is not fragile, its recorded start
   state equals the current state, its lexer state at its first token equals the
   current lexer state, its text span and lookahead window are untouched, and
   its right-context hash matches. A mismatch breaks the subtree down instead
   of guessing. Nodes created during a conflict, dynamic lookahead, or recovery
   are marked fragile and are never reused as a whole.
5. **Error recovery has two tiers.** The fast tier is tree-sitter-style online
   recovery: recover to an earlier stack state, skip a token into an `ERROR`
   node, or insert a `MISSING` zero-width token; GLR versions are ranked by
   error cost and dynamic precedence. The quality tier is a CPCT+-style
   cost-ordered search over insert/delete repairs; it reports the complete set
   of minimum-cost repair sequences, ranks them by how far parsing continues,
   and is bounded by time, node, and repair budgets. Default mode is
   `balanced`: online recovery first, complete search when the bad region is
   small enough. `complete` mode is for diagnostics and tests, not for every
   keystroke.
6. **Everything is deterministic.** Symbol IDs come from declaration order;
   GLR version ordering, merge decisions, tie-breaking, and repair ordering use
   stable keys. No hash-map iteration order, thread timing, or allocation
   address can affect the parse tree.

---

## 1. What “incremental” and “error recovery” mean here

### 1.1 Terms

| Term | Meaning in this plan |
|---|---|
| **source snapshot** | Immutable text plus the edit history needed to map old positions into it. |
| **edit** | A replacement of a byte range in the source snapshot. A transaction may contain several edits. |
| **token** | A lexeme with a stable `TokenId`, a kind, a byte range (stored as text anchors), the lexer state at its start and end, a lookahead extent, and flags (trivia, error, missing). |
| **token stream** | A persistent sequence of tokens covering every byte of the source, including trivia and error tokens. |
| **syntax tree** | A persistent concrete syntax tree (CST) whose leaves are tokens and whose internal nodes are productions. It covers every byte exactly once. |
| **fragile node** | A node whose internal parse was produced while more than one GLR interpretation was active, during a declared conflict, during dynamic lookahead, or during error recovery. It must not be reused as a whole. |
| **reuse certificate** | The set of recorded facts that justifies shifting an old subtree without reparsing it: start state, lexer state, right-context hash, lookahead extent, fragility, and change overlap. |
| **repair** | A token insertion, deletion, or structural recovery action that makes parsing continue. |
| **complete repair set** | All minimum-cost insert/delete sequences that allow the parser to reach the success condition, within the declared search budget. |

### 1.2 Invariants

The implementation is judged against these invariants. Every invariant has at
least one gate in §8.

| ID | Invariant |
|---|---|
| **L1** | Bootstrapping a lexer at any token boundary with the recorded start state reproduces the same suffix token stream as a fresh lex of that suffix. |
| **L2** | The token stream covers every source byte exactly once, except for explicitly flagged zero-width tokens, and token ranges are ordered and non-overlapping. |
| **L3** | Incremental lexing performs work proportional to the relexed window, not to the file length, whenever the state reconverges. |
| **L4** | Unchanged tokens keep their `TokenId`; changed tokens get new identities only when kind, lexeme, start state, or end state changes. |
| **L5** | Lexer errors are explicit tokens with spans and diagnostics; they never panic, loop forever, or silently drop input. |
| **P1** | The parse of revision `r` is structurally equal to a from-scratch parse of the same text with the same grammar, options, and deterministic tie-breaks, ignoring node and token IDs. |
| **P2** | An unchanged subtree keeps its `NodeId`; a changed subtree gets a new root identity, while unchanged descendants may keep theirs. |
| **P3** | A reused subtree is semantically valid in its new context; the certificate in §6.4 is sufficient, not merely a heuristic. |
| **P4** | Incremental work is bounded by the changed token region, the affected stack spine, and GLR work in fragile regions; an edit in a block comment may legitimately reparse to EOF. |
| **P5** | The tree contains no duplicate tokens and no dropped bytes; `ERROR` nodes and `MISSING` tokens make the loss explicit. |
| **P6** | In `complete` recovery mode, the returned repair set is exactly the set of minimum-cost insert/delete repairs under the stated budget, up to declared GLR pruning. |
| **P7** | Recovery is deterministic: the same text, grammar, options, and prior state produce the same repair sequence and the same tree. |
| **P8** | `ERROR` nodes and regions containing `MISSING` tokens are never reused as a whole; they are reparsed when touched. |
| **D1** | Grammar diagnostics are compile-time diagnostics with source spans and actionable suggestions, not runtime panics. |
| **D2** | The parser never fails on malformed input in normal mode; it returns a tree plus diagnostics. Only cancellation, budget exhaustion with no fallback, or an internal invariant violation is a `ComputeError`. |
| **R1** | State replacement, output publication, and any optional `Syntax` definitions commit atomically; a cancelled or failed parse publishes nothing. |
| **R2** | The parser and lexer use no `unsafe`, no global mutable state, and no locks in user-visible algorithms. |
| **R3** | Output equality is cheap and stable: an unchanged parse returns the previous `Arc`s, so the runtime's cutoff suppresses downstream work. |

### 1.3 What “perfect” can and cannot mean

* **No parser can avoid reparsing when the edit changes context arbitrarily far
  away.** Inserting `/*` can turn the rest of a file into a comment. The
  contract is optimality relative to the actual dependency: unchanged suffix
  tokens are reused only when the lexer state and the parse state prove them
  unchanged. When they do not, reparsing to EOF is correct, not a bug.
* **GLR is worst-case cubic, not linear.** Local ambiguity and declared
  conflicts are expected to be local, as measured for real languages by
  Tomita and Wagner. Version pruning and budgets bound the pathological cases.
* **Error recovery is heuristic unless the complete tier is used.** No
  grammar-neutral algorithm knows what the programmer intended. The complete
  tier guarantees a minimum edit-distance repair set, not the “right” repair.
* **“From-scratch equality” is relative to the grammar's declared
  disambiguation.** If two parses are genuinely ambiguous and no dynamic
  precedence is declared, the deterministic tie-break is part of the grammar
  semantics; a from-scratch parse uses the same tie-break.
* **Node identity is stable only while the node is semantically reused.**
  Replacing a subtree with an equal-looking subtree constructed from scratch is
  not required to preserve identity; the parser preserves it when the reuse
  certificate holds.

---

## 2. Evidence and design decisions

### 2.1 Sources actually read

The research was primary-source driven. Where a paper was paywalled, the plan
uses its abstract and the detailed treatment in CPCT+ or Wagner's thesis, and
says so. The table is not a claim that every mechanism transfers unchanged.

| Source | Mechanism used here | What is adopted / what is not |
|---|---|---|
| Tomita, *Efficient Parsing for Natural Language* (1986) [T86] | GLR, graph-structured stack, local ambiguity packing | Adopted as the conceptual base. Tomita's original algorithm is not used directly because of the nullable/cyclic termination problem. |
| Scott & Johnstone, *Right-nulled GLR parsers* (TOPLAS 2006) [SJ06] | RN parse tables, right-nullable reductions, prebuilt ε-trees | Adopted for correctness on nullable grammars. RN tables make the parser terminate and avoid Tomita's GSS search pathologies. |
| Scott, Johnstone & Economopoulos, *BRNGLR* (2007) [SJE07] | Binary right-nulled GLR; cubic worst case | Considered; RNGLR is simpler to implement and the reported differences are not decisive for an editor parser. BRNGLR remains a possible optimization. |
| Johnstone, Scott & Economopoulos, *Evaluating GLR parsing algorithms* (2006) [JSE06] | Comparative GLR behavior; edge traversal costs | Used to justify measuring GSS edge visits, not just wall time. |
| Economopoulos, Klint & Vinju, *Faster Scannerless GLR Parsing* (CC 2009) [EKV09] | SRNGLR; 16–33% speedups over SGLR; nullable statistics | Adopted the RN table idea; rejected scannerless parsing as the default because tokenization is part of the requested adapter boundary. |
| Jalili & Gallier, *Building Friendly Parsers* (POPL 1982) [JG82] | First incremental LR(1) parser with multiple edit sites; state matching by storing parse states in nodes | Historical ancestor of the reuse certificate. State matching is sufficient but not necessary; the plan adds lexer state, right context, and fragility. |
| Larchevêque, *Optimal Incremental Parsing* (TOPLAS 1995) [LAR95] | Two notions of incremental optimality for LALR(1); threaded trees; node reuse | Adopted LALR and the per-node-state discipline. Not adopted: the threaded-tree representation and its extra per-node fields; the GLR certificate is the replacement. |
| Heering, Klint & Rekers, *Incremental Generation of Lexical Scanners* (TOPLAS 1992) [HKR92] | Lazy/incremental construction of scanner DFAs | Distinguished from incremental lexing: this plan compiles the DFA once and makes the *runtime token stream* incremental using Wagner's algorithm. |
| Wagner & Graham, *Efficient and Flexible Incremental Parsing* (TOPLAS 1998) [WG98a] | Sentential-form parsing, state matching, right-edge breakdown, node reuse, optimality definition | Adopted the reuse discipline and the insight that state matching is sufficient but not necessary. The deterministic sentential-form parser itself is not used; the GLR variant is. |
| Wagner, *Practical Algorithms for Incremental Software Development Environments* (PhD thesis, 1998), ch. 7 [W98] | Incremental GLR (IGLR), LALR tables, dynamic lookahead encoded as a nondeterministic state, atomic nondeterministic regions | This is the direct algorithmic ancestor of `Parser`. LALR is chosen for reuse; nondeterministic regions are treated as fragile and rebuilt atomically. |
| Wagner & Graham, *General Incremental Lexical Analysis* (1999) [WG99] | Token-end state, lookback/lookahead dependencies, token reuse, error tokens, optimal relexing | This is the direct algorithmic ancestor of `Lexer`. Its state-at-token-end model is generalized to a pushdown state machine. |
| Sijm, *Incremental Scannerless Generalized LR Parsing* (SPLASH SRC 2020) [SIJ20] | State-matching test using the top parse state, irreusable nodes created during nondeterminism, atomic region rebuild, 25× speedup on Git histories | Adopted the fragile/irreusable concept and the empirical warning that nondeterminism reduces reuse. Rejected scannerless character-level parsing for this adapter. |
| Tree-sitter documentation and source: `parser.c`, `stack.c`, `subtree.h`, `error_costs.h`, grammar DSL docs [TS] | GSS version merging, error states, stack summaries, recover-to-state, missing tokens, error costs, token reuse conditions, fragile flags, external scanner state, lex modes, grammar fields/conflicts/precedence | Adopted heavily for engineering detail. Changed where noted below (RN tables, complete repair tier, general stateful lexer, reuse in multiple GLR heads). |
| Lezer system guide [LEZ] | Explicit GLR only at declared ambiguity, recovery by running several recovery tricks in parallel, incremental tree fragments, contextual tokenization, context hash for reuse | Adopted the explicit-conflict philosophy and the idea of speculative recovery branches. Rejected the JavaScript implementation and its LR-only table model. |
| Diekmann & Tratt, *Don't Panic! Better, Fewer, Syntax Errors for LR Parsers* (CPCT+, ECOOP 2020) [CPCT] | Complete set of minimum-cost insert/delete repairs, configuration merging, ranking by furthest parse, timeout | Adopted as the quality tier. Its deterministic-LR assumption is weakened to “per GLR head”; completeness is explicitly relative to the explored frontier. |
| Burke & Fisher (1987) [BF87]; Corchuelo et al. (2002) [COR02] | Minimum-distance repair, trial parses, single-repair search | Used through CPCT+'s explanation and comparison; not implemented directly. CPCT+ fixes the completeness bugs identified in Corchuelo et al. |
| Aho & Peterson (1972) [AP72] | Minimum-distance error correction by grammar augmentation | Historical foundation; rejected as the runtime mechanism because it changes the grammar and does not integrate with incremental reuse. |
| Kats, de Jonge, Nilsson-Nyman & Visser, *Providing Rapid Feedback in Generated Modular Language Environments* (OOPSLA 2009) [KAT09] | Error recovery for SGLR via automatically generated permissive productions and backtracking | Considered as an alternative recovery tier. Not the default because it mutates the grammar and is less predictable for a generic adapter; useful as a future opt-in “island grammar” mode. |
| Wagner, thesis ch. 8 [W98]; Cormack's LR substring parser [SUB89] | Non-correcting, history-sensitive isolation of edits | Adopted the *isolation* idea for deciding which error regions must be rebuilt; rejected the non-correcting output model because downstream consumers need a complete tree. |
| DeRemer & Pennello (1979) [DP79] | Efficient LALR(1) lookahead computation | Used for table generation. |
| Denny & Malloy, IELR(1) (2010) [IELR] | Minimal LR(1) tables for non-LR(1) grammars | Offered as an optional table mode; LALR remains the default because state merging improves incremental reuse. |

**The incremental-LR lineage, in order.** Jalili and Gallier introduced the
state-matching test: a subtree records the parser state in which it was
created, and can be shifted without inspection if the current state matches.
Larchevêque refined this to LALR(1) and defined optimality relative to the
threaded-tree representation. Wagner and Graham replaced state matching for
deterministic grammars with sentential-form parsing and right-edge breakdown,
which is more powerful but assumes a deterministic parser. Wagner's thesis then
combined state matching with Tomita-style GLR, producing IGLR: LALR tables,
state recorded per node, nondeterministic regions treated as an equivalence
class, and dynamic lookahead encoded in the state. Sijm later added the
“irreusable if created during nondeterminism” rule and measured its cost on
scannerless Java. Tree-sitter and Lezer supply the engineering patterns:
GSS version merging, error cost, `MISSING` tokens, explicit conflict
declarations, and conservative reuse.

This plan's contribution is the combination: an IGLR-style certificate that
also records the **lexer state and right-context hash**, applied to a lexer
with an explicit pushdown state; a grammar DSL that makes conflict and state
explicit; and a recovery tier that can return a *complete* minimum-cost repair
set when asked. No single cited system has all three.

### 2.2 What tree-sitter does, precisely

The following is a summary of the mechanisms in the tree-sitter runtime and
grammar documentation, not a claim that they are the only way to build a GLR
parser.

1. **Graph-structured stack with multiple versions.** Each stack version is a
   head into a shared graph of `StackNode`s. A node records its parse state,
   byte position, error cost, dynamic precedence, and links to predecessors.
   Links carry a subtree and may be packed when two derivations reach the same
   node. `ts_stack_merge` merges versions with the same state, position, error
   cost, and external scanner state.
2. **Breadth-first shift across versions.** The parse loop advances every active
   version one token, then condenses the versions: it removes worse versions,
   merges compatible ones, and keeps at most `MAX_VERSION_COUNT` (6, with a
   temporary overflow of 4). Version comparison uses error cost, progress since
   the last error, and dynamic precedence.
3. **Conflicts are explicit.** Tree-sitter's grammar DSL requires a `conflicts`
   declaration for an intended LR conflict. Precedence and associativity remove
   conflicts statically; dynamic precedence chooses among genuinely ambiguous
   results at runtime. An undeclared conflict is a grammar-generation error.
4. **Incremental reuse is conservative.** The parser traverses the old tree as
   part of the input stream. A node is reusable when it has no changes, is not
   an error/missing/fragile node, its external scanner state matches, its first
   leaf can be lexed in the current lex mode, and the table entry says the token
   is reusable. Crucially, `allow_node_reuse` is true only when there is exactly
   one stack version; reuse is disabled once GLR branches.
5. **Ambiguity makes nodes fragile.** A node created during a reduction that
   popped multiple paths, or while multiple versions were active, is marked
   fragile and gets no reusable parse state. Fragility propagates through
   ancestors.
6. **Error recovery is local and greedy.** On error, the parser performs
   possible reductions, tries inserting a token that permits a reduction of the
   lookahead, pushes an error discontinuity, records a stack summary, and then
   either recovers to an earlier state where the lookahead is valid or wraps the
   lookahead in an `ERROR` node and skips it. Nested errors are merged. Costs are
   500 per recovery, 110 per missing token, 100 per skipped tree, 30 per skipped
   line, and 1 per skipped character. It produces one tree, not a complete
   repair set.
7. **Lexer state is parser-adjacent.** Each parse state selects a lex mode; an
   external scanner receives the set of valid external tokens and can serialize
   its state into the tree. This is how tree-sitter handles indentation, heredocs,
   and similar context-sensitive tokens. Token reuse requires the external
   scanner state to match.
8. **Changed ranges are computed from the new tree.** The old and new trees are
   compared to produce ranges that changed, which editors use to invalidate
   highlights and other caches.

### 2.3 What this plan adopts and what it changes

| Area | Tree-sitter | This plan |
|---|---|---|
| Core algorithm | Tomita-style GLR | RNGLR tables and reductions for nullable grammars. |
| Tables | LR(1)-style generated tables with conflicts | LALR(1) by default (better state reuse), optional canonical LR(1)/IELR(1); conflicts are explicit. |
| Reuse | Only when one stack version; conservative fragile flag | Per-head reuse certificate; fragile nodes are never reused, but deterministic subtrees may be reused while several heads are active. Soundness is stated as a theorem, not assumed. |
| Ambiguity | Local packing; dynamic precedence picks a tree | Same packing and precedence, plus an optional packed-forest output mode. |
| Lexer | Generated DFA tokens plus hand-written external scanners in C | General pushdown lexer with declarative states, guards, value stack, and serializable state. Optional Rust “external rule” escape hatch later. |
| Incremental lexing | Token cache and parser-driven lexing | Explicit relex window, state equality at token boundaries, token identity reuse, and lookback-extent tracking. The parser consumes a token stream rather than driving the lexer, except for optional external tokens. |
| Error recovery | One fast greedy strategy | Fast online tier plus CPCT+-style complete repair tier; both share the same tree representation. |
| Grammar input | JavaScript `grammar.js` evaluated by Node | Rust-native `.grammar` DSL, compiled in-process or by a macro; no Node/C toolchain. |
| Tree identity | Subtree pointers; no cross-revision stable ID contract | `NodeId` is stable for reused nodes and is part of the public contract. |

### 2.4 Error recovery: which algorithm is “better”?

There is no single winner. The published evidence and the settings differ:

| Algorithm family | Quality | Latency / worst case | Completeness | Incremental / GLR fit | Decision |
|---|---|---|---|---|---|
| Panic mode | Poor; cascading errors | Very fast | None | Easy | Fallback only. |
| Burke–Fisher / Corchuelo et al. | Single minimum-cost repair | Search can be exponential; Corchuelo adds fixed repair limits | Single sequence, and the original CPCT paper identifies missed repairs | Deterministic LR | Not used directly. |
| **CPCT+** | Best published general quality in this family: complete set of minimum-cost repairs, ranked by how far parsing continues | Mean 13.6 ms per file on 200k Java files; 98.37% repaired under 0.5 s; but 6 unmatched brackets exhausted 32 GiB in the paper's stress test | Complete set, within timeout | Needs adaptation to GLR; complete search is too costly per keystroke | **Quality tier.** |
| Tree-sitter online recovery | Good for interactive editing; one plausible tree | Fast, integrated with GLR versions | None | Excellent | **Fast tier and default.** |
| Lezer recovery | Runs several recovery tricks as GLR branches; picks the best after a few tokens | Fast, bounded by GLR branching | None | Excellent | Influences the “repair branches” design. |
| SGLR error productions (Kats et al.) | Good when the grammar is designed for it | Backtracking cost; grammar becomes permissive | None | Good for scannerless/embedded languages | Future opt-in; not default. |
| History-sensitive non-correcting recovery (Wagner) | Excellent localization of the *edit* that caused the error | Very cheap | Not a repair; leaves invalid text unincorporated | Excellent | Used for *isolation*, not for producing a complete tree. |

The conclusion is deliberate:

* If the product requirement is **diagnostic quality**, CPCT+ is the strongest
  published general baseline, and its complete-set idea is worth adopting.
* If the product requirement is **interactive latency on every edit**, a greedy
  online GLR recovery is better, because complete search is unbounded and the
  CPCT+ paper itself relies on a timeout.
* The adapter therefore has a **quality knob** and a **latency knob**, and the
  default is the fast tier with a small complete-search window. This is more
  honest than claiming either algorithm is always better.

CPCT+ numbers quoted above are from its corpus experiment [CPCT, §6]; they are
not reproduced here and must not be treated as a promise for this
implementation.

### 2.5 Why not simply use an existing parser

| Alternative | Why it is not the adapter |
|---|---|
| tree-sitter | C/C++ runtime, C external scanners, JavaScript grammar build, no reactive unit identity, no stable cross-revision `NodeId` contract, and a single greedy recovery tree. It is an excellent source of engineering ideas, not a drop-in reactive component. |
| Lezer | JavaScript, LR with GLR only at declared ambiguity, grammar build in JS, no Rust runtime integration. Its explicit-conflict and context-hash ideas are adopted. |
| LALRPOP | Deterministic LR(1), no GLR ambiguity, no incremental reuse, no grammar-level error-repair search. |
| pest | PEG: ordered choice, no left recursion, no general ambiguity, no incremental parse state. |
| chumsky / nom | Combinators; no parse table, no GSS, no subtree reuse certificate, no minimum-cost repair search. |
| grmtools / CPCT+ implementation | CPCT+ is deterministic LR and not incremental; its repair search is useful as a reference, not as the parser core. |
| salsa/rowan | Incremental storage or tree representation, not a parser. |

---

## 3. Architecture and public API

### 3.1 Crates and modules

```
reactive-syntax-text        immutable text snapshots, text index, anchors, edit maps
reactive-syntax             grammar compiler, lexer, parser, recovery, syntax tree
reactive-syntax-macros      optional grammar! / lexer! compile-time front end
reactive                    facade re-exports (optional feature "syntax")
```

The core grammar compiler must not depend on the runtime, so it can be used by
the macro crate and by tests. The adapters depend on `reactive-runtime` and
`reactive-collections`. Suggested internal layout:

```
reactive-syntax/src/
  text/          TextSnapshot, TextAnchor, TextEdit, TextVersion
  grammar/       DSL parser, IR, validation, BNF lowering, table builder
  lexer/         LexerSpec, DFA compiler, Lexer, LexState, TokenStream
  parser/        ParserSpec, GLR runtime, GSS, SyntaxTree, recovery
  syntax_view/   optional per-node reactive units
  diagnostics/   spans, codes, suggestions
```

`reactive-syntax-text` may start inside `reactive-syntax`; it is split only if
compile-time or reuse pressure justifies it. The text model is independent of
grammar and parsing.

### 3.2 Public types

```rust
pub struct Grammar { /* Arc<CompiledGrammar> */ }

impl Grammar {
    pub fn compile(source: &str) -> Result<Self, GrammarError>;
    pub fn compile_file(path: &Path) -> Result<Self, GrammarError>;
    pub fn lexer(&self) -> LexerSpec;
    pub fn parser(&self) -> ParserSpec;
    pub fn kind_name(&self, kind: KindId) -> &str;
    pub fn field_name(&self, field: FieldId) -> &str;
    pub fn token_kind_name(&self, kind: TokenKindId) -> &str;
}

pub struct Lexer { /* spec + options */ }
pub struct Parser { /* grammar + lexer engine + options */ }

impl Lexer {
    pub fn new(spec: LexerSpec, options: LexerOptions) -> Self;
    pub fn lex(&self, document: &TextDocument) -> LexOutput;
    pub fn lex_incremental(
        &self,
        previous: &LexState,
        document: &TextDocument,
    ) -> LexOutput;
}

impl Parser {
    pub fn new(grammar: Grammar, options: ParserOptions) -> Self;
    pub fn parse(&self, document: &TextDocument) -> ParseOutput;
    pub fn parse_incremental(
        &self,
        previous: &ParserState,
        document: &TextDocument,
    ) -> ParseOutput;
}

pub struct LexState {
    pub tokens: Arc<TokenStream>,
    pub text: TextSnapshot,
    pub version: TextVersion,
}
pub struct ParserState {
    pub tree: Arc<SyntaxTree>,
    pub tokens: Arc<TokenStream>,
    pub text: TextSnapshot,
    pub version: TextVersion,
}
```

`Lexer` and `Parser` also implement the runtime traits directly:

```rust
impl GeneralComponent for Lexer {
    type State = LexState;
    type Output = LexOutput;
    // ID, NAME, VIEWS, initialize, update
}

impl GeneralComponent for Parser {
    type State = ParserState;
    type Output = ParseOutput;
    // ID, NAME, VIEWS, initialize, update
}
```

The direct implementation, rather than `TypedGeneralComponent`, is chosen so
that `Parser` owns its lexer engine and reads only `SourceFile`. There is one
general-component root per runtime today; making `Parser` the root avoids a
second root and avoids the runtime change that a separate general-component
dependency would require. `Lexer` can still be installed alone when the host
only needs tokens.

### 3.3 How the host uses it

```rust
use reactive::Runtime;
use reactive_syntax::{Grammar, Parser, ParserOptions, SourceFile};

let grammar = Grammar::compile(include_str!("json.grammar"))?;
let runtime = Runtime::new();
runtime.install_general(Parser::new(grammar, ParserOptions::default()))?;

// The host edits a source view, not the parser.
runtime.edit(|edit| {
    let mut file = edit.view::<SourceFile>();
    let next = file.document().replace(10..12, "{");
    file.set_document(next);
})?;

let parsed = runtime.read_outputs_general::<Parser>()?;
for diagnostic in parsed.diagnostics() {
    eprintln!("{}: {}", diagnostic.code, diagnostic.message);
}
let document = parsed.tree().root();
assert_eq!(parsed.tree().grammar().kind_name(document.kind()), "Document");
```

Token-only use:

```rust
use reactive_syntax::{Grammar, Lexer, LexerOptions};

let grammar = Grammar::compile(include_str!("json.grammar"))?;
let runtime = Runtime::new();
runtime.install_general(Lexer::new(grammar.lexer(), LexerOptions::default()))?;
let lexed = runtime.read_outputs_general::<Lexer>()?;
```

`SourceFile` is the sole input contract:

```rust
#[view(source)]
pub struct SourceFile {
    pub document: TextDocument,
}
```

`TextDocument` is an immutable snapshot plus a versioned edit log (§3.4). The
host does not pass edits directly to `Parser`; that would break the runtime's
revision and snapshot contracts.

### 3.4 Text snapshots and coordinates

Positions are the part of an incremental parser that is easiest to get subtly
wrong. This plan uses **versioned anchors plus an edit map**, not chunk IDs and
not mutable byte offsets, for everything that survives a revision.

* A `TextSnapshot` is an immutable index (backed by a piece table or rope for
  storage) that carries a `TextVersion` and a persistent chain of edit maps.
* A `TextAnchor` is `(TextVersion, byte offset in that version)`.
* A `TextRange` is a pair of anchors.
* `TextSnapshot::resolve(anchor) -> Option<usize>` maps an anchor forward to
  the current version's byte offset. An anchor maps if its offset was not
  deleted or replaced; if it was, it returns `None`.
* A `TextEdit` is `{ range: Range<usize>, replacement: Arc<str> }` in the
  coordinate space of the version being edited. Applying it creates a new
  snapshot whose edit map records `(old_range, new_range, delta)`.
* A node or token stores anchors. An edit before a node shifts its resolved
  offset but does not rewrite the node.
* A reused node is only reused when both endpoints resolve and no edit
  intersects its range; `resolve` is then guaranteed to equal the new text at
  the corresponding position.
* Edits are half-open. An anchor exactly at an edit boundary maps to the
  corresponding boundary (start to start, end to end), not to `None`; only an
  anchor strictly inside a replaced range is lost.

Versioned offsets avoid the brittle part of chunk-ID anchors: splitting a
chunk, merging pieces, or changing the storage representation cannot invalidate
a node that still covers unchanged text. The edit-map chain is persistent, so
a new snapshot can still resolve anchors from an older version that it
retains. Path compression caches the composed map; the chain is bounded by the
revisions a live output still references.

`TextDocument` carries:

```rust
pub struct TextDocument {
    snapshot: Arc<TextSnapshot>,
    version: TextVersion,
    history: Arc<TextHistory>, // bounded; retained only while pinned
}
pub struct TextEdit {
    pub range: Range<usize>,
    pub replacement: Arc<str>,
}
pub struct TextVersion(u64);
```

`TextHistory::edits_between(old, new)` returns an ordered edit batch, or
`None` if the old version has been pruned. In the latter case the parser has
the old `TextSnapshot` in its state and can compute a diff; this is a
correctness fallback, not the fast path. The runtime's snapshot pinning keeps
the old snapshot alive while a consumer holds a `ParseOutput`. The output tree
holds the `TextSnapshot` that its anchors resolve against, so a snapshot
remains readable after later edits.

Because anchors are versioned, the reuse certificate compares **resolved
offsets**, not anchor identity: `resolve(node.range.start) == cursor.offset`.
An anchor from an old version and an anchor from the current version can name
the same text position; that is not a reason to reject reuse.

**Runtime additions this plan needs (small):**

1. A source-view type capable of carrying `TextDocument` as one field. This is
   ordinary `#[view(source)]` plus a `ContentVtable` implementation for the
   text type; no runtime change if the field equality is identity-based.
2. `GeneralCx::check_cancelled()` (or a cancellation token handle) so a long
   parse can stop between tokens. Today `GeneralComponent` execution is only
   checked before claim and at commit. Without this, cancellation is delayed by
   the duration of one parse; the adapter can bound that with a budget, but the
   clean contract needs the check.
3. Optional: a way to install more than one general component, or to request a
   general component from another general component. This plan avoids the need
   by having `Parser` own its lexer, but the `Syntax` view bridge (§3.7) and
   future parser pipelines would benefit.

None of these require changing the node model, revision protocol, or
scheduling. They are accessors, not new semantics.

### 3.5 Output values, equality, and cutoff

```rust
pub struct LexOutput {
    pub tokens: Arc<TokenStream>,
    pub text: TextSnapshot,     // the coordinate space the anchors resolve against
    pub diagnostics: Arc<[Diagnostic]>,
    pub report: Arc<LexReport>, // excluded from equality
}

pub struct ParseOutput {
    pub tree: Arc<SyntaxTree>,
    pub tokens: Arc<TokenStream>,
    pub diagnostics: Arc<[Diagnostic]>,
    pub changed: Arc<[ChangedRange]>,
    pub report: Arc<ParseReport>, // excluded from equality
}
```

`PartialEq` compares `Arc` pointers for `tree` and `tokens`, and compares
`Diagnostic` values for diagnostics. `report` (timings, reuse counts, GLR
versions) is observational and must not trigger a change notification.
A representative report is:

```rust
pub struct ParseReport {
    pub relexed_chars: u64,
    pub reused_tokens: u64,
    pub reparsed_tokens: u64,
    pub reused_nodes: u64,
    pub glr_versions: u32,
    pub gss_edge_visits: u64,
    pub recovery_mode: RecoveryMode,
    pub recovery_cost: u32,
    pub recovery_complete: bool,
    pub recovery_truncated: bool,
}
```

The numbers are tested as invariants in §8.3; they are not a performance
promise.
When a parse produces no structural change, it returns the previous `Arc`s, so
the runtime's cutoff keeps the change version and observers do not fire.

The tree itself carries the `TextSnapshot` it was parsed against:

```rust
pub struct SyntaxTree {
    pub grammar: Arc<CompiledGrammar>,
    pub text: TextSnapshot,
    pub root: Arc<Node>,
    pub version: TreeVersion,
}
```

A snapshot of the parse therefore remains readable after later edits. Node
ranges are anchors; `node.range()` resolves against `tree.text` and returns a
current byte range. `node.range_in(&other_snapshot)` is available when the
node is reused in a newer tree.

### 3.6 One root, two adapters

The runtime currently permits one root component (generated or general). The
adapters respect that:

* `Parser` is one `GeneralComponent` and owns a lexer engine internally;
  `ParseOutput::tokens()` exposes the token stream.
* `Lexer` is independently installable when the host does not need a parse.
  It is not a child component of `Parser`.
* If the host needs both in one runtime, install `Parser` and read its tokens;
  this is the supported path.

This is not a loss of incrementality: the parser's internal lexer uses the
same `LexState` and incremental algorithm as the standalone `Lexer`. It is a
loss of independent observability of tokens, which the optional `Lexer` root
covers in a separate runtime.

### 3.7 Optional fine-grained `Syntax` view

The parser's output tree is a value. A consumer that reads the whole tree
depends on one output, so a change anywhere in the tree invalidates it. That is
coarser than the runtime's collection and tree families. The plan therefore
includes an optional bridge, not in the first implementation phase:

* A `Syntax` view has one logical unit per node identity:
  * membership (`UnitKind::Membership`), so a removed node is certified absent;
  * kind (`UnitKind::Value`, `KindId`);
  * span (`UnitKind::Value`, anchor range);
  * each field (`UnitKind::Value`, text/number/token payload);
  * each child slot (`UnitKind::Structure`, ordered stable node IDs);
  * parent (`UnitKind::Value`, optional node ID).
* The parser defines units only for nodes it creates or changes. Reused nodes
  keep their units, so downstream computations keyed by `NodeId` are reused.
* Definitions are staged through `GeneralCx` and commit atomically with the
  output tree. A consumer never sees a node without its tag or children.
* A `SyntaxRef` reads the narrowest unit for an accessor, mirroring
  `TreeRef`: reading a node's kind does not read its children; reading one
  child does not read its siblings.

This bridge is what makes the parser a first-class reactive producer rather
than a coarse black box. It is also the most expensive part to get right:
retracting removed nodes, preserving identity across reparses, and keeping the
unit count proportional to changed nodes. It is therefore a separate phase
with its own gates, after the parser's from-scratch and incremental contracts
are stable.

---

## 4. Grammar specification

### 4.1 Design principles

The grammar language is part of the user-facing API. It must be:

1. **Local.** A rule's meaning is visible in its production; no hidden
   precedence tables or magic capitalization rules.
2. **Explicit about conflict.** GLR is not allowed to happen accidentally.
   Every retained ambiguity is either resolved by declared precedence or named
   in a `conflict` declaration.
3. **Explicit about state.** Lexer modes and value-stack operations are
   visible. Indentation and XML matching are user-written guard/action logic,
   not built-ins.
4. **Diagnosable.** Every grammar error has a span, a code, and a suggestion.
5. **Tractable.** The DSL parser is hand-written recursive descent, independent
   of the parser adapter (no bootstrapping cycle), and reports multiple errors
   per compile when possible.
6. **Rust-native.** No Node.js, no C preprocessor, no generated C. The same
   grammar compiles in-process, in a macro, or in a test.

The DSL is line-oriented inside blocks but not whitespace-sensitive; comments
are `//` and `/* ... */`.

**Why this surface, and not another one?**

| Option | Verdict |
|---|---|
| External `.grammar` DSL (primary) | Readable for large grammars, diffable, reusable by tools, independent of Rust compile cycles, and able to carry spans for diagnostics. |
| `grammar!` macro (later) | Same DSL inside a Rust source file, with compile-time validation and generated `TokenKind`/`NodeKind` enums. Good for small grammars and type-safe consumers; not required for the core. |
| Rust builder API | Fine for dynamically generated or very small grammars, and useful in tests; unreadable for a real language. It lowers to the same IR. |
| Yacc/Bison `.y` | Familiar, but no lexer states/value stack, no explicit GLR conflict model, and error recovery is grammar-specific; rejected as the primary surface. |
| tree-sitter `grammar.js` | Powerful, but requires Node and a JavaScript evaluation step, and its external scanners are C; rejected for a Rust-native runtime. |
| Lezer `.grammar` | Close in spirit and an influence on the surface; it is JavaScript-oriented and lacks the pushdown lexer/value stack and Rust diagnostics. |

### 4.2 Complete JSON example

```text
grammar Json {
    start Document;

    lexer {
        initial root;

        state root {
            skip ws       = /[ \t\r\n]+/;
            token string  = /"([^"\\]|\\.)*"/;
            token number  = /-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?/;
            token ident   = /[A-Za-z_][A-Za-z0-9_]*/ @word;

            token "{" ;
            token "}" ;
            token "[" ;
            token "]" ;
            token ":" ;
            token "," ;
        }
    }

    parser {
        Document = Value;
        Value    = Object | Array | string | number | ident
                 | "true" | "false" | "null";
        Object   = "{" (Pair ("," Pair)*)? "}";
        Pair     = key: string ":" value: Value;
        Array    = "[" (Value ("," Value)*)? "]";
    }
}
```

Notes:

* `skip` rules emit trivia tokens. They are retained in the token stream and
  in the CST as trivia token children, but they are not parser terminals.
  Keeping them is what lets the tree cover every byte and lets an editor see
  the whitespace and comments it needs to display.
* `token "{"` declares an anonymous exact token. The parser may refer to it as
  `"{"`; the compiler resolves the literal to the unique exact token.
* `ident @word` marks the token as a word-like token. Literal keywords such as
  `"true"` are extracted contextually by the parser: in a state where `true` is
  a valid terminal and the token text is exactly `true`, the parser uses the
  keyword terminal; otherwise it uses `ident`. This gives contextual keywords
  without exploding the lexer's token set.
* `key:` and `value:` are field names. Field names are grammar-level IDs and
  become `FieldId`s in the tree; they do not change recognition.
* There are no semantic actions. The output is a concrete syntax tree; a
  separate AST mapping can be written as a component over the CST. Actions
  would have to run during ambiguous parses and would compromise
  incrementality, so they are rejected rather than silently skipped.

### 4.3 Lexer block reference

| Syntax | Meaning |
|---|---|
| `initial root;` | Initial mode. Exactly one initial mode. |
| `state name { ... }` | A mode with its own rules. |
| `skip NAME = PATTERN;` | Emit a trivia token: retained in the token stream/CST, never a parser terminal. |
| `token NAME = PATTERN;` | Emit a token of kind `NAME`. |
| `token "text" ;` | Emit an anonymous exact token. |
| `@priority(n)` | Lexical priority within the same match length; higher wins. Default 0; ties are declaration order (and are diagnosed if they are ambiguous and unannotated). |
| `@word` | Marks the token as word-like for parser keyword extraction. |
| `@extra` | Marks a normally declared token as trivia, so the parser ignores it as a terminal. |
| `when EXPR` | Guard evaluated after the longest match; the match is rejected if false. |
| `-> action, ...` | State/value-stack action after the token is emitted. |

`skip` and `@extra` both produce tokens with `TokenFlags::TRIVIA`. The parser
does not look them up in the table; it buffers them and attaches them to the
CST as trivia token children in source order. This is the same role as tree-sitter's
“extra” tokens, moved into the lexer so that byte coverage is a lexer
invariant rather than a parser side effect. A grammar may also declare
`extras { Token }` for a token that is not lexically marked, but the normal
form is to mark the token itself.

Rule patterns are a regular-language subset:

* literals and escapes;
* character classes and negated classes;
* `.` (any scalar except newline), `\d`, `\w`, `\s` and their Unicode-aware
  forms;
* grouping, alternation, `*`, `+`, `?`;
* named captures `(?P<name>...)`, referenced in guards/actions as `$name`;
* anchors are implicit: a rule always starts at the current lexer position.

No backreferences, lookaround, or recursive pattern calls in a lexer rule.
Those are the features that make a lexer non-regular and would destroy the
DFA-based incremental guarantees. A later opt-in external rule hook can cover
them, with an explicit serialization contract.

Guard expressions are a small, total expression language:

```text
column | depth | lexeme | $capture | top_value | eof | integer | string
       | (EXPR) | not EXPR | EXPR and EXPR | EXPR or EXPR
       | EXPR == EXPR | EXPR != EXPR | EXPR < EXPR | ...
       | EXPR + EXPR | EXPR - EXPR
```

`column` is the current zero-based column in Unicode scalar values.
`top_value` is the top of the value stack, or a typed “empty” value; comparing
it to any value is false. `depth` is the value-stack depth. `eof` is a boolean.
Guards must be pure and deterministic; they cannot call Rust.

Actions:

| Action | Effect |
|---|---|
| `stay` | Keep the current mode and value stack. |
| `switch(MODE)` | Replace the current mode. |
| `push(MODE)` | Push the current mode and enter `MODE`. |
| `pop` | Restore the previous mode. |
| `push_value(EXPR)` | Push a value onto the value stack. |
| `set_value(EXPR)` | Replace the top value; error at runtime if empty. |
| `pop_value` | Pop the value stack; error at runtime if empty. |
| `error("message")` | Emit the matched text as an error token with the message. |

Multiple actions apply left to right. Emission happens before actions, so the
token records the state *after* the action as its end state. This is the state
that must match the next token's start state and the state stored in the old
token stream.

Zero-width tokens are allowed (`token indent = "" when ...;`) but must make
progress: the action must change the mode stack or the value stack. The
compiler rejects a zero-width rule that can loop without changing the machine
state; the runtime also has a `max_zero_width_run` guard and reports a
structured diagnostic if a guard makes the static check incomplete.

### 4.4 Parser block reference

| Syntax | Meaning |
|---|---|
| `start Rule;` | Start symbol. |
| `Rule = ...;` | BNF/EBNF production. |
| `inline Rule;` | Remove `Rule` from the CST and substitute its productions. |
| `A = x: B ...;` | Field `x` on child `B`. Fields are optional and multiple children may share a field. |
| `@prec(Level)` | Static precedence annotation; `Level` is declared in `precedence`. |
| `@prec(Level, left/right/nonassoc)` | Precedence plus associativity for this production. |
| `@dynamic(n)` | Dynamic precedence contribution for ambiguity resolution. |
| `conflict A, B;` | Retain the LR conflict between rules `A` and `B` and let GLR explore it. Undeclared conflicts are errors. |
| `extras { Token, ... }` | Optional: marks named tokens as trivia when they are not already `skip`/`@extra`. Usually unnecessary. |
| `table = lalr / lr1 / ielr;` | Table construction mode; default `lalr`. |
| `recovery { ... }` | Recovery mode and budgets, e.g. `mode = balanced; max_versions = 8; nshifts = 3; ntry = 250; time_budget_ms = 50;`. Defaults are in §6.5. |
| `error` | Reserved node kind; productions may not declare it. |

Precedence declaration:

```text
precedence {
    Add   = left;   // declared loosest first
    Mul   = left;
    Unary = right;
}
```

Levels are ordered by declaration from loosest to tightest. A production with
`@prec(Mul)` that conflicts with one at `Add` resolves in favor of `Mul`; equal
levels use the declared associativity. A conflict that cannot be resolved this
way requires `conflict` or is a compile error.

A complete expression fragment shows the interaction:

```text
precedence {
    Add   = left;
    Mul   = left;
    Unary = right;
}

parser {
    Expr = Expr "+" Expr @prec(Add)
         | Expr "*" Expr @prec(Mul)
         | "-" Expr      @prec(Unary)
         | number
         | "(" Expr ")";
}
```

If a language has a genuinely ambiguous construct (for example, a C-style
cast versus a parenthesized expression), the grammar names the pair and lets
GLR explore it:

```text
conflict Expr, Cast;
```

The parser then produces both interpretations and resolves them by dynamic
precedence if one is declared; otherwise the stable rule order decides and the
choice is reported in the parse report.

EBNF sugar (`*`, `+`, `?`, groups, alternation) is lowered to fresh internal
nonterminals with deterministic names and no CST nodes. `inline` is the
explicit way to remove a named rule from the CST; there is no “uppercase means
named” convention.

### 4.5 A stateful lexer: XML with matching end tags

The point of this example is that **nothing about XML is built into `Lexer`**.
The lexer sees modes, captured text, a value stack, and guards. A language
designer can write the same shape for HTML, JSX, or a template language.

```text
grammar XmlLike {
    start Document;

    lexer {
        initial text;

        state text {
            skip ws        = /[ \t\r\n]+/;
            token Text     = /[^<]+/;

            token StartTag = /<(?P<name>[A-Za-z_:][A-Za-z0-9_.:-]*)/
                           -> push_value($name), switch(tag);

            token EndTag   = /<\/(?P<name>[A-Za-z_:][A-Za-z0-9_.:-]*)\s*>/
                           when $name == top_value
                           -> pop_value;

            token EndTagMismatch =
                /<\/(?P<name>[A-Za-z_:][A-Za-z0-9_.:-]*)\s*>/
                when $name != top_value
                -> error("end tag does not match the open element");

            token "<!--" -> push(comment);
            token "<![CDATA[" -> push(cdata);
        }

        state tag {
            skip ws      = /[ \t\r\n]+/;
            token Attr   = /[A-Za-z_:][A-Za-z0-9_.:-]*/;
            token "=" ;
            token Value  = /"[^"]*"|'[^']*'/;
            token "/>"   -> pop_value, switch(text);
            token ">"    -> switch(text);
        }

        state comment {
            token CommentText = /[^-]+/;
            token "-" ;
            token "-->" -> pop;
        }

        state cdata {
            token CdataText = /[^\]]+/;
            token "]" ;
            token "]]>" -> pop;
        }
    }

    parser {
        Document = Item*;
        Item     = Element | Text | Comment | Cdata;
        Element  = StartTag Attribute* ( "/>" | ">" Item* EndTag );
        Attribute = Attr "=" Value;
        Comment  = "<!--" (CommentText | "-")* "-->";
        Cdata    = "<![CDATA[" (CdataText | "]")* "]]>";
    }
}
```

This example exposes an important design choice: the lexer validates the tag
name match, but the parser still sees `StartTag` and `EndTag` as separate
terminals. The value stack is lexer state, not parser state. The alternative —
letting the parser decide which end tag is valid — would require parser-driven
lexing and would make incremental relexing depend on parser state. The chosen
split keeps the lexer independently incremental; the price is that a
context-free language whose tokenization genuinely depends on parse context
(such as JavaScript's regex/division ambiguity) is not handled by the default
mode. See §10.

### 4.6 A stateful lexer: indentation without an indentation feature

```text
grammar Indented {
    start Program;

    lexer {
        initial line_start;

        state line_start {
            // column and top_value are generic. Nothing here knows "indentation".
            skip CodeStart = "" when depth == 0
                           -> push_value(0), switch(code);
            skip SameLine  = "" when depth > 0 and column == top_value
                           -> switch(code);
            token Indent    = "" when depth > 0 and column > top_value
                            -> push_value(column), switch(code);
            token Dedent    = "" when depth > 0 and column < top_value
                            -> pop_value;
            token Newline   = /\r?\n/;
            skip Space      = /[ \t]+/;
        }

        state code {
            skip Space    = /[ \t]+/;
            token Ident   = /[A-Za-z_][A-Za-z0-9_]*/ @word;
            token Number  = /[0-9]+/;
            token Newline = /\r?\n/ -> switch(line_start);
            token ":" ;
            token "(" ;
            token ")" ;
            token "=" ;
            token "," ;
            skip Comment = /#[^\n]*/;
        }
    }

    parser {
        Program = Newline* Stmt*;
        Stmt    = Ident ":" (Newline (Indent Stmt+ Dedent)? | simple)?
                | simple;
        simple  = Ident "(" Ident ")"? ; // illustrative; a real grammar continues
    }
}
```

Again, `Indent` and `Dedent` are user-declared zero-width tokens with generic
guards and value-stack actions. The lexer does not know what a line is beyond
tracking `column`; blank-line and comment handling are expressed by rule order
and guards in the grammar.

### 4.7 Compile-time diagnostics

`Grammar::compile` returns `GrammarError` with a list of `Diagnostic`s. It
continues after errors where it safely can, so a user sees more than the first
mistake.

Examples of checks:

* **Lexical**
  * undefined state, unreachable state, duplicate token name;
  * two rules with equal priority and identical longest match;
  * a zero-width rule with no state/value-stack effect;
  * a guard referencing an undefined capture or an ill-typed comparison;
  * a `pop`/`pop_value` that can execute with an empty stack (reported as a
    reachability warning, not a hard error, when static analysis is
    inconclusive);
  * an `@extra` token also used as `skip`.
* **Syntactic**
  * undefined nonterminal or token;
  * duplicate rule, unused rule (warning), unreachable rule;
  * nullable cycle (`A =>+ A` with no token consumption);
  * left recursion is allowed, including indirect and hidden left recursion;
  * undeclared conflict, with a counterexample token sequence and suggestions:
    “declare `conflict A, B`”, “add `@prec`”, “factor the rule”, “mark an
    alternative `inline`”;
  * field name used inconsistently on different alternatives (warning);
  * `@dynamic` without a declared conflict (warning).
* **Recovery**
  * impossible budget combination, zero `nshifts`, `ntry` too large, etc.

Diagnostics carry `code`, `severity`, `span`, `message`, and `notes`. The DSL
compiler uses the same `TextSnapshot`/anchor machinery as the source parser,
so a diagnostic span survives concurrent edits in an editor host.

### 4.8 Grammar IR and generated kinds

The compiler lowers the DSL to a small IR:

```rust
pub struct GrammarIr {
    pub name: String,
    pub tokens: Vec<TokenDef>,       // declaration order = stable ID
    pub lexer_states: Vec<LexStateDef>,
    pub productions: Vec<Production>, // declaration order = stable ID
    pub start: NonterminalId,
    pub precedence: Vec<PrecedenceLevel>,
    pub conflicts: Vec<ConflictDecl>,
    pub extras: Vec<TokenId>,
    pub fields: Vec<FieldDef>,
    pub table_mode: TableMode,
    pub recovery: RecoveryOptions,
}
```

Stable IDs come from declaration order. The compiler never iterates a
`HashMap` when assigning an ID or an action. The compiled grammar exposes:

* `KindId` for node kinds (one per visible/inline-expanded rule or literal),
  `TokenKindId` for terminals, `FieldId` for named fields;
* name interning for diagnostics and serialization;
* a `GrammarFingerprint` (stable hash of the normalized IR), recorded in every
  tree. A tree from a different grammar is never reused.

The optional `grammar!` macro (phase 6) parses the same DSL at compile time,
emits the same `CompiledGrammar` as a static, and can additionally generate:

```rust
pub enum TokenKind { ... }
pub enum NodeKind { ... }
pub enum Field { ... }
```

The core API uses `KindId` so dynamic grammars work without codegen. The
generated enums are an ergonomics layer, not a second grammar semantics.

---

## 5. Lexer

### 5.1 Formal model

A lexer configuration at a token boundary is:

```
C = (mode, values, column)
```

where `mode ∈ Mode`, `values` is a persistent stack of `LexValue`
(`Text(Arc<str>)`, `Int(i64)`, `Bool(bool)`, `Unit`), and `column` is the
current zero-based column in Unicode scalar values. The current `position` is
not part of the state id: it is supplied to the transition separately, so the
same state stays valid when unchanged text shifts to a new byte offset.

```
step(C, snapshot, position) -> Outcome
```

is one of:

* `Token { token, next: C' }`;
* `Error { token, next: C' }`;
* `End { token? }` at EOF.

A token rule has a regular pattern `P`, a guard `G`, an action `A`, and a
priority. At a position, all rules in the current mode are matched with
maximal munch: the longest accepted prefix wins; among equal lengths, higher
priority wins; ties are declaration order, but an unannotated equal-priority
tie is a compile error if the alternatives produce different token kinds.
Guards are evaluated on the winning match. If the winning rule's guard is
false, the lexer backtracks to the next-longest accepted match; the DFA retains
accept states for this purpose. This is bounded by the pattern length, not by
input length, and the compiled DFA records the fallback chain.

The state after emission is `C'`. The token stores `C` and `C'` as interned
`LexStateId`s. This internal lexer configuration is distinct from the public
`LexState` (the adapter's retained token stream and text version). Interned
configurations compare by pointer or hash equality, so a token can cheaply
compare its recorded state with a current state. `column` is part of the id
when any guard in the grammar reads it; if an edit earlier on the same line
changes the column, the state comparison catches it and the token is
relexed. A grammar that never reads `column` omits it from the id, so unrelated edits do not invalidate tokens.
Absolute offsets are deliberately excluded either way, so edits on previous
lines do not invalidate the state.

The value-stack depth is capped by `LexerOptions::max_value_depth` (a
configurable resource limit). Exceeding it produces a structured lexer
diagnostic and an error token, never a panic or an unbounded allocation. Within
that cap, the stack provides the nested state that indentation and matching
delimiters need.

This model is a deterministic pushdown transducer, not a regular lexer. It is
still deterministic, which keeps relexing linear and makes the language of
token streams unambiguous. If a
grammar genuinely needs nondeterministic tokenization, that is a scannerless parser
problem, not a lexer feature; see §10.

### 5.2 Compilation

For each mode:

1. Parse patterns into a small regex AST.
2. Normalize alternation/quantifiers; compile to an NFA (Thompson or
   derivatives).
3. Determinize to a DFA with accept states carrying rule IDs and priority.
4. Add guard-failure fallback: from each accept state, remember the next-best
   accept on the path; the runtime can reject a guarded match and continue.
5. Compute first-character classes to reject impossible rules early.
6. Check zero-width cycles: build a graph of (mode, value-stack effect,
   zero-width) transitions and reject a cycle that consumes no input and
   changes only value payloads that do not affect guards. The runtime check is
   a backstop, not the primary proof.
7. Emit a compact transition table. State IDs are deterministic.

The compiler also validates that every token kind used by the parser is
producible in every lexer mode where it is expected. This is a reachability
analysis over modes and parser states; it catches missing token definitions
early.

### 5.3 Token stream

```rust
pub struct Token {
    pub id: TokenId,
    pub kind: TokenKindId,
    pub text: Arc<str>,
    pub range: TextRange,        // anchors
    pub start_state: LexStateId,
    pub end_state: LexStateId,
    pub lookahead_end: TextAnchor,
    pub flags: TokenFlags,       // TRIVIA | ERROR | MISSING | UNTERMINATED
    pub error: Option<Arc<str>>, // lexer error message
}
```

`TokenStream` is a persistent rope of token runs, not a flat `Vec`. Edits
split runs and replace the affected run; the suffix runs are shared. Iteration
is left-to-right with a cursor; random access by index is `O(log runs)`.
`TokenId` is allocated by the lexer and reused only when the token is
semantically unchanged.

Invariant: `token[i].end_state == token[i+1].start_state`, except across a
relexed boundary where the runtime explicitly re-establishes the chain.
`lookahead_end` is the furthest character the DFA examined while producing the
token; it can be beyond `range.end` because maximal munch reads ahead.

### 5.4 Incremental relexing

Lexer incrementality is materially simpler than parser incrementality because
tokenization is local to `(state, suffix)`. There is no parse stack to match
and no ambiguity to preserve; the only hard parts are the lookahead window and
the persistent state stack. That is why the lexer can be made optimal with a
small, directly testable algorithm while the parser needs the certificate in
§6.4.

Input: `old: TokenStream`, `old_text`, `new_text`, `edits: [TextEdit]`.
Output: `new: TokenStream`, `relexed: Range<TokenIndex>`, diagnostics.

```
relex(old, old_text, new_text, edits):
    // 1. Find the first token whose start or lookahead window is touched.
    first = first_token_whose(
        any edit e: t.range.intersects(e.range) ||
                     t.lookahead_end >= e.start
    )
    // 2. Move one more token left if its end state might depend on the edit.
    while first > 0 and any edit e: old[first-1].lookahead_end >= e.start:
        first -= 1

    // 3. Relex from old[first].range.start using old[first].start_state.
    state = old[first].start_state
    position = map_anchor(old[first].range.start, new_text)
    produced = []
    loop:
        if position == old boundary at index j and
           state == old[j].start_state and
           !edits.intersects(old_suffix(j)):
            // Reconverged: reuse old[j..]; its versioned anchors still resolve.
            produced.extend(old[j..])
            break
        outcome = step(state, position, new_text)
        produced.push(outcome.token)
        state = outcome.next
        position = outcome.next_position

    // 4. Rebuild the run structure around the replaced window.
    return TokenStream::splice(old, first, produced)
```

The reconvergence check is not only “same state”: it must also ensure that the
remaining text is unchanged. Comparing full suffixes is unnecessary; compare
the edit map: if no edit intersects the old suffix range from the boundary,
the suffix text is unchanged. The state equality is on the interned
`LexStateId`, so it is O(1).

Correctness depends on two facts:

* The suffix token stream from a boundary is a pure function of
  `(start_state, suffix text)`.
* The old start state at `old[j]` is still valid if no edit touches any token
  before `old[j]` whose lookahead reaches into the relexed region. The
  `lookahead_end` expansion in step 2 establishes that.

Token reuse: when a newly produced token has the same kind, lexeme, start
state, and end state as the old token at the corresponding position, the
lexer keeps the old `TokenId`. Its anchors remain valid when the lexeme is
unchanged; only the resolved byte positions change. This preserves identity
for downstream consumers.

Zero-width tokens need special care in the reconvergence loop: a zero-width
token can be produced at the same position multiple times (dedent runs). The
loop tracks `(position, state, value_stack)`; if it revisits the same
configuration without progress, it stops with a diagnostic. The old stream
may also have a run of zero-width tokens; matching the run as a whole is safer
than matching individual tokens.

### 5.5 Lexer error recovery

The default behavior is deliberately simple and uniform:

* If no rule matches at a position, consume one Unicode scalar value and emit
  an `ERROR` token with a message. If the next position also fails, merge
  adjacent error characters into one error token to keep the stream compact.
* An unterminated construct (a rule that reaches EOF with an open mode) is
  reported by the default EOF diagnostic. The lexer does not guess a closing
  delimiter.
* A guard can emit an error token with `error("...")`; the matched text is
  consumed and the mode/value stack is left unchanged unless the rule also
  specifies an action.
* The lexer never removes a state frame on error automatically. If a language
  wants to resynchronize, it declares a rule that pops to a stable mode; this
  is explicit policy in the grammar, not a hidden “indentation recovery”
  feature.
* Every error token has a non-empty range except for a final EOF error, which
  may be zero-width. The token stream still covers every byte.

Incremental behavior: an error token is a token like any other. If its text
and states are unchanged, it is reused and its diagnostic is stable. If an
edit touches it, the relex window replaces it. A parser sees the error token
as input it cannot shift; the parser's recovery layer wraps it in an `ERROR`
node. The lexer does not silently drop invalid text.

### 5.6 Complexity

Let `w` be the number of characters in the relex window, `l` the lookahead
overshoot, and `r` the number of replaced runs.

* Time: `O(w + l + r log R)`, where `R` is the number of token runs; this is
  linear in the affected text for a fixed DFA.
* Space: `O(new tokens + log R)` new persistent structure; old runs are shared.
* If the state does not reconverge, the window grows to EOF: `O(n)` worst
  case. This is correct and is the documented fallback for edits that change a
  state that affects the rest of the file.

This is the same asymptotic shape as Wagner's incremental lexer: work is
linear in the affected characters, not the file, when the lexer state
reconverges.

---

## 6. Parser

### 6.1 Grammar lowering and tables

#### 6.1.1 BNF lowering

EBNF is lowered to BNF with fresh nonterminals:

* `A B` becomes a production with ordered children;
* `A | B` becomes two productions;
* `A*`, `A+`, `A?` become fresh nonterminals with hidden CST nodes;
* groups become fresh hidden nonterminals;
* field annotations attach to the child produced by the annotated element.

The lowering preserves source order and records enough metadata to reconstruct
the original EBNF shape for diagnostics and tree fields. The lowered grammar
is the only input to table construction.

#### 6.1.2 LALR(1) by default

The parser builds the LR(0) automaton and computes LALR(1) lookaheads with the
DeRemer–Pennello algorithm [DP79]. LALR is chosen for three reasons:

1. **Incremental reuse.** Merged states with the same core make state matching
   succeed more often. Wagner explicitly notes that LALR improves reuse over
   canonical LR(1) for this reason [W98, §7.3.2].
2. **Table size.** LALR states are smaller by a large factor on real
   grammars; that matters for an editor adapter that compiles grammars in
   process.
3. **GLR correctness.** GLR explores the conflicts that LALR introduces; the
   accepted language is determined by the table, and the deterministic
   tie-break is part of the grammar contract. A user who needs canonical
   parses can select `table = lr1` or `table = ielr`.

The compiler reports the table size, state count, conflict count, nullable
nonterminals, and right-nullable productions. These are useful review
artifacts, not just diagnostics.

#### 6.1.3 Conflict policy

For each table cell with multiple actions:

1. Apply production precedence and associativity. A higher-precedence
   reduction wins over a shift/reduce conflict; equal precedence uses the
   declared associativity; `nonassoc` turns the conflict into an error entry.
2. If one action remains, emit it.
3. If multiple actions remain:
   * if the pair is named by a `conflict` declaration, keep all actions and
     let GLR explore them; `@dynamic` ranks the resulting trees at runtime
     rather than pretending the conflict can be resolved statically;
   * otherwise fail compilation with a counterexample token sequence.

The counterexample is produced by a small breadth-first search over the
automaton from the state, looking for a terminal sequence that reaches the
conflicting cell. The diagnostic names the two productions, the state, the
lookahead, and the suggested fixes. This is the main usability difference
between a GLR parser and a Yacc-style generator that prints an opaque conflict
dump.

#### 6.1.4 RN tables

Nullable suffixes are the reason Tomita's original GLR can fail to terminate
or can miss parses. RNGLR fixes this by adding reductions for productions
`A -> α β` where `β =>* ε` before the suffix's symbols have been reduced.
The reduction carries a prebuilt ε-tree for `β`; when the reduction fires, the
tree is inserted, and the GSS does not need the repeated searches that
Farshi's correction requires [FAR91, SJ06, EKV09].

Implementation:

* compute `nullable` and `right_nullable` for every symbol/production;
* for each production whose suffix after some prefix is right-nullable, add an
  RN reduction entry at the corresponding item;
* build the ε-tree for the suffix once, at table-generation time;
* assign each RN reduction a stable ID distinct from ordinary reductions;
* verify with a nullable/ambiguous grammar corpus, including `A -> B C`,
  `B -> ε`, `C -> ε`, and mutually recursive nullable rules.

The runtime does not need a special “RN mode” flag; RN reductions are ordinary
reduction entries with a prebuilt subtree and a marker for cost/debugging.

#### 6.1.5 Optional canonical LR(1)/IELR(1)

`table = lr1` builds canonical LR(1) states. It is useful for validating that
a grammar is LR(1) and for reducing spurious conflicts. It is not the default
because canonical state proliferation reduces reuse and increases table size.

`table = ielr` is a future mode that computes minimal LR(1) tables with
conflict resolution [IELR]. It is listed here so the table interface is
designed to accept it; it is not required for the first implementation.

### 6.2 GLR runtime

#### 6.2.1 GSS and versions

A `GssNode` is:

```rust
struct GssNode {
    state: StateId,
    position: TextAnchor,
    lex_state: LexStateId,
    error_cost: u32,
    dynamic_precedence: i32,
    links: SmallVec<GssLink>, // symbol, child node, subtree, flags
}
enum GssSubtree {
    Token(Arc<Token>),
    Node(Arc<Node>),
}
struct GssLink {
    symbol: SymbolId,
    previous: GssNodeId,
    tree: Option<GssSubtree>, // None for pending/uncertain edges
    flags: LinkFlags,
}
```

A stack version is a head `GssNodeId` plus its current input cursor. Nodes are
shared. A node key for deduplication is
`(state, position, error_cost, lex_state)`; links are deduplicated by
`(symbol, previous, tree-equivalence)` and packed when they represent
alternative derivations of the same span.

The tree selection rule for packed alternatives is:

1. lower `error_cost`;
2. higher cumulative `dynamic_precedence`;
3. earlier parse (leftmost production ID / deterministic tie-break);
4. stable rule order.

This is the same ordering family as tree-sitter's `select_tree`, made explicit
and tested.

#### 6.2.2 Parse loop

```
loop:
    for each active version:
        advance one token (or one old subtree)
    condense_versions()      // remove worse, merge compatible, cap count
    if accept_versions exist and no better version can beat them:
        break
finish()                     // select the best accepted tree
```

`advance`:

0. Buffer any leading `TRIVIA` tokens; they do not participate in the table
   lookup and are attached later.
1. Look up the table action(s) for the current state and effective terminal.
2. If one action: shift or reduce it.
3. If multiple actions and the conflict is declared: branch into multiple
   versions (or packed links for reduce/reduce).
4. If error: enter recovery (or mark the version failed).

`reduce(A -> α)`:

1. Pop `|α|` links from the GSS. Because the GSS is a DAG, this can yield
   multiple paths; each path is a candidate reduction.
2. For each path, build or reuse the parent node from the child trees.
3. Compute `goto(state, A)`, push the new link, and merge the resulting node
   with an existing equivalent node if possible.
4. Mark the new node fragile if the reduction was created during a conflict,
   during multi-version parsing, or while the version's error state was active.

RN reductions use the same path but insert the prebuilt ε-tree in the suffix
positions; they do not require the suffix to have been reduced.

#### 6.2.3 Version condensation and pruning

`condense_versions`:

* drop halted versions;
* for each pair, compare by error cost, progress since the last error, and
  dynamic precedence:
  * clearly worse → remove;
  * comparable and mergeable → merge links;
  * otherwise keep both;
* cap at `max_versions` (default 8; tree-sitter uses 6);
* if a version has not advanced for a configurable number of steps, choose
  the best and pause the rest; recovery resumes the best paused version.

The comparison is deterministic: all scores are integers, and ties are broken
by version creation order, which is itself deterministic. No wall-clock time is
part of the ranking.

#### 6.2.4 Ambiguity

Default mode `best` keeps packed alternatives only as long as they are needed
to choose the best tree, then drops the losers. This avoids printing an
exponential forest while preserving the correct chosen tree. Mode `forest`
retains a packed forest (the future semantic-disambiguation use case) and
returns `Ambiguity` nodes; it is off by default because it increases memory and
requires downstream consumers to understand packing.

A node created while alternatives are live is fragile. This is intentionally
conservative: even if the final parse selects one alternative, the node's
internal structure may have depended on dynamic lookahead, so it must not be
reused after a nearby edit. This is the same tradeoff Sijm measures: about a
third of Java nodes were marked irreusable, but only about 2% were actually
broken down in his experiments [SIJ20].

### 6.3 Node representation and reuse metadata

```rust
pub struct NodeId(pub u64);

pub struct Node {
    pub id: NodeId,
    pub kind: KindId,
    pub symbol: SymbolId,          // nonterminal symbol used by GOTO
    pub range: TextRange,          // anchors
    pub children: Arc<[Child]>,    // ordered; tokens and internal nodes
    pub fields: Arc<[FieldValue]>, // field id + child index / value
    pub meta: NodeMeta,
    pub error: Option<Arc<ErrorInfo>>,
}

pub enum Child {
    Node {
        field: Option<FieldId>,
        node: Arc<Node>,
    },
    Token {
        field: Option<FieldId>,
        token: Arc<Token>,
        effective_kind: TokenKindId, // keyword extraction can change the parse role
    },
}

pub struct NodeMeta {
    pub start_state: Option<StateId>,   // None for fragile nodes
    pub end_state: Option<StateId>,
    pub lex_state: LexStateId,
    pub right_context: u64,             // hash of k following non-trivia tokens
    pub lookahead_end: TextAnchor,
    pub error_cost: u32,
    pub dynamic_precedence: i32,
    pub flags: NodeFlags,               // FRAGILE | ERROR
}
```

`NodeFlags`:

* `FRAGILE` — never reuse as a whole; propagate to ancestors when a child is
  fragile.
* `ERROR` — an `ERROR` node produced by recovery.

Tokens carry their own flags (`TRIVIA`, `ERROR`, `MISSING`,
`UNTERMINATED`); a missing token is therefore a token leaf, not an internal
node. A token leaf also stores the **effective terminal** used by the parser:
for an ordinary token this is `token.kind`, and for a `@word` token specialized
into a keyword it is the keyword terminal. This lets the CST expose the parse
role without duplicating the lexeme or changing the lexer's token identity. Trivia is attached as a token child. This keeps the internal-node
metadata uniform and avoids a special “leaf node” case in the GLR and reuse
code.

`start_state`/`end_state` are `Option` only to make fragility explicit; a
fragile node deliberately loses its state certificate. `start_state` is the
parser state at the node's left edge, i.e. the state from which the node's
symbol is shifted; it is not the goto state after the reduction. `end_state`
is the state after shifting the node. This is the state-matching convention
used by Jalili–Gallier, Larchevêque, Wagner, tree-sitter, and Sijm.
`lex_state` is the lexer state at the node's first non-trivia token; leading
trivia is buffered before the reuse test, so the cursor reports the same
state. `right_context` is a stable hash of the next `k` **non-trivia**
tokens: kind, text hash for word-like tokens (keyword
extraction depends on the text, not just the kind), and lex state. Here
`k = max(1, options.lookahead)` (default 1; recovery may use 2–3). Trivia is
excluded because it does not affect parse actions; including it would make
whitespace edits invalidate otherwise reusable subtrees. `lookahead_end` is
the furthest anchor the lexer/parser examined for this node's right edge.
`error_cost` is the accumulated tree-sitter-style cost.

The tree is persistent: a node's children are `Arc`s; a changed parent is new,
unchanged children are shared. `NodeId` is allocated from a monotonic counter
in the parser state and is never reused for a different node.

Trivia handling is intentionally outside the table. A `TRIVIA` token is not a
terminal, so it does not participate in the LR/GLR action lookup. The parser
buffers trivia between real tokens; when it constructs a node, it attaches the
buffered trivia as `Child::Token` entries in source order. Extras immediately
before a real token are attached before that token; extras after the last
child of a reduction are carried as trailing extras and attached to the next
enclosing node, mirroring tree-sitter's trailing-extras handling. The result
is the same CST shape tree-sitter obtains with “extra” shift actions, without
inflating the parse table with an extra action in every state. An edit that
changes only trivia changes the tree's spans/trivia children but not the
recognized nonterminal structure; such a region may still be reparsed locally,
and the benchmark reports its cost separately.

### 6.4 Incremental parsing

#### 6.4.1 The reuse certificate

Define:

```
reusable(node, head, cursor, new_tokens, edits) =
    !node.meta.flags.contains(FRAGILE)
 && !node.meta.flags.contains(ERROR)
 && !node.children_contain_error_or_missing()
 && table.shiftable(head.state, node.symbol)
 && resolve(node.range.start) == cursor.offset
 && resolve(node.range.end).is_some()
 && !edits.overlaps(resolve_range(node.range))
 && !edits.overlaps(resolve_range(node.range.start..node.meta.lookahead_end))
 && node.meta.start_state == Some(head.state)
 && node.meta.end_state == Some(goto(head.state, node.symbol))
 && node.meta.lex_state == cursor.lex_state
 && node.meta.right_context == context_hash(cursor, k)
 && resolve(node.meta.lookahead_end) == Some(cursor.lookahead_end)
```

The `shiftable` and `start_state` checks are state matching. The
`lex_state` and `right_context` checks are the lexical and right-context half
that tree-sitter implements through `ts_parser__can_reuse_first_leaf`,
external scanner state, and byte-offset checks. The `lookahead_end` check is
the generalization needed for maximal-munch tokenization and GLR lookahead.
The `fragile` checks are the structural half.
`children_contain_error_or_missing` is a conservative backstop: fragility
propagation should already reject an ancestor of an `ERROR` node or a
`MISSING` token, but the certificate must not depend on that invariant being
perfectly maintained.

The `end_state` check is redundant when the table is consistent, but it is
kept because it catches a corrupted or stale node before it is shifted.

If the full certificate fails but the node's text is unchanged and its start
state matches, the parser **breaks the node down**: it replaces the node in
the input stream with its children. This exposes the largest reusable
prefix/suffix. This is the safe version of “right-edge breakdown”: the parser
never shifts a node whose right context is unknown. For a node whose only
problem is a changed right context, breaking down descends until it reaches a
child whose context is unchanged; the rest is reparsed. This is less clever
than Wagner's eager `right_breakdown` but simpler to prove and still local
to the changed region in the common case.

#### 6.4.2 Input as tokens plus old subtrees

The parser's input is a lazy stream of `InputItem`:

```rust
enum InputItem<'a> {
    Token(TokenRef),
    Subtree(&'a Node),
    Eof,
}
```

A cursor is a position in the new token stream plus a zipper into the old
tree. At a cursor position:

1. If the old tree has a node starting exactly at the cursor, try the reuse
   certificate for the current GLR head.
2. If it passes, yield `Subtree(node)`.
3. Otherwise, descend into the node's children (breakdown) and try the first
   child.
4. If there is no old node, yield the new token at the cursor.
5. If the old node starts after the cursor, the gap is new text; yield tokens
   until the cursor reaches it.
6. `Eof` only when both the new token stream and the old tree are exhausted.

Because a cursor has a zipper path, it is cheap to clone for each GLR head.
Different heads can reuse different subtrees; the input is not a single
materialized vector.

#### 6.4.3 Algorithm

```
parse_incremental(previous, document):
    edits = document.edits_since(previous.version)
    lexed = lexer.lex_incremental(previous.tokens, edits, document)
    if lexed is unchanged and previous.tree.text == document.snapshot:
        return previous output (same Arcs)

    // Build a fresh GSS. The old tree is the input stream.
    glr = Glr::new(spec, lexed.tokens, previous.tree, edits)
    glr.push_start()

    while glr.has_active_versions():
        if cancelled() or over_budget():
            return cancelled_or_fallback()
        glr.advance_all()
        if glr.error():
            glr.recover()
        glr.condense()

    let tree = glr.finish_best()
    let changed = diff_ranges(previous.tree, tree)
    return ParseOutput { tree, tokens: lexed.tokens, changed, ... }
```

The loop starts at the beginning of the file, not at the edit. That sounds
expensive, but the first old subtree that still has a valid certificate is
usually a large prefix, and shifting it costs `O(1)` plus the traversal down
the spine. The parser does not re-tokenize or re-reduce the prefix. This is the
same strategy as tree-sitter's `reusable_node` and Wagner's input stream.

#### 6.4.4 Why the certificate is sound

**Thesis.** Let `T` be an old subtree for symbol `A`, parsed from state `s` with
lexer state `l`, with unchanged text in `T` and an unchanged right context
`c` (the next `k` token kinds/states and the lookahead extent). Let `P` be a
new parser configuration with state `s` and lexer state `l` at `T`'s start.
Then shifting `T` produces the same configuration and the same tree as
reparsing `T`'s yield from `P`.

Proof sketch:

1. Every reduction inside `T` was determined by `s`, the tokens of `T`, and
   lookahead that never extends beyond `T`'s recorded `lookahead_end`. For a
   non-fragile node the table's effective lookahead is bounded by `k`, and the
   right-context hash covers those tokens; the `lookahead_end` check ensures
   the lexical extent beyond them is unchanged as well. This is the standard
   LR/GLR viable-prefix argument.
2. The lexer states at every token boundary inside `T` are a pure function of
   `(l, text)`. Since the text and `l` are unchanged, every token is identical
   and every lexer state is identical. This is invariant L1.
3. `T` is not fragile, so it was not produced while multiple interpretations or
   recovery were live; its internal parse is unique under the table and
   tie-break. This rules out dependence on a dynamic-lookahead branch that a
   fresh parse might take differently.
4. The start state `s` matches, and `A` is shiftable there; the `end_state`
   and `GOTO` check on the new head are the ordinary LR conditions. Therefore
   the stack after shifting `T` is the same as the stack after reparsing it.
5. Induct over the input stream: every reused subtree contributes the same
   configuration as its reparse, and every new token is parsed normally. The
   final tree is structurally equal to a from-scratch parse.

The failure modes are exactly the certificate's false cases: fragile nodes,
edited text, changed lexer state, changed right context, or a start-state
mismatch. The certificate is conservative, not complete; a more clever parser
could reuse more, but it would need a different proof.

#### 6.4.5 Fragile regions and convergence

A fragile region is a maximal subtree containing `FRAGILE` nodes. When an edit
touches a fragile region, the whole region is broken down and reparsed. This is
Wagner's “nondeterministic regions are reconstructed atomically” rule and
Sijm's irreusable-node rule. The region boundary is the nearest non-fragile
ancestor. The parser may reuse the non-fragile children outside the boundary.

This is the main reason a change inside a declared ambiguity can cost more than
a change in ordinary code. The cost is bounded by the size of the fragile
region, which is expected to be local; the benchmark in §8 measures it.

#### 6.4.6 Changed ranges

`diff_ranges(old, new)` walks the two trees by identity:

* reused subtree → no change;
* same node ID but changed fields/children → range of the node;
* new node → range of the new node;
* removed node → old range plus the new range of its replacement.

The output is a sorted, merged list of `ChangedRange { old, new }`. It is used
by editors and by tests. The invariant is that every edit is covered and no
node outside the changed ranges differs structurally. A test compares this
list with a brute-force diff of the old and new trees.

### 6.5 Error recovery

#### 6.5.1 Tree representation of errors

* `ERROR(kind = Error)` is an internal node whose children are the skipped
  tokens/subtrees. It has a range covering them and a diagnostic.
* `MISSING` is a zero-width token leaf inserted by recovery; its `range` is a
  single anchor pair, and its kind is the inserted terminal.
* `ERROR` nodes are always fragile. `MISSING` tokens are always inside a
  fragile error/recovery region, so their ancestors can never be reused.
* Adjacent `ERROR` nodes are merged; an `ERROR` node never overlaps another.
* Every input token is either in exactly one normal subtree or in exactly one
  `ERROR` subtree. `MISSING` tokens add no input coverage.

This is the tree-sitter representation, made explicit as an invariant.

#### 6.5.2 Fast tier (online)

When no version has an action for the lookahead:

1. **Potential reductions.** Perform reductions that do not depend on
   lookahead, to expose a state where the lookahead can shift. This is
   tree-sitter's `do_all_potential_reductions`.
2. **Missing token.** For each candidate terminal `t` that the current state can
   shift and that leads to a state with a reduce action for the lookahead,
   create a zero-width `MISSING(t)` and try the reduce. Choose the first
   candidate in stable terminal order if several work. Record `ERROR_COST_PER_MISSING`.
3. **Recover to state.** Record a stack summary when entering the error state.
   For each previous state `s` in the summary where the lookahead is valid,
   compute:
   `cost = depth * skipped_tree + skipped_chars * skipped_char_cost + skipped_lines * line_cost`.
   If that is better than the current best, create a new version that pops back
   to `s`, wraps the popped subtrees in `ERROR`, and shifts the lookahead.
4. **Skip token.** Wrap the lookahead in `ERROR`, push it, and continue in the
   error state. Merge with an existing trailing `ERROR` if present.
5. **Rank versions.** Compare error cost first, then progress since the last
   error, then dynamic precedence. Cap versions. Stop pursuing a version if an
   accepted tree with a lower error cost already exists.

Default costs follow tree-sitter's `error_costs.h`: 500 per recovery, 110 per
missing token, 100 per skipped tree, 30 per skipped line, 1 per skipped
character. They are configurable because the best values are grammar- and
language-dependent; the paper's criticism of magic constants applies here too.

#### 6.5.3 Quality tier (complete repair search)

The quality tier is a CPCT+-style search adapted to the GLR frontier. It is
used when `recovery.mode` is `balanced` or `complete`, and the current error
region is within budget.

A configuration is:

```
Config = (gss_head, token_index, repair_sequence, cost)
```

`gss_head` is a GSS node plus its cursor; `token_index` is the current token;
`repair_sequence` is a persistent parent-pointer chain (cactus), not a copied
list. Actions:

| Action | Preconditions | Cost | Effect |
|---|---|---|---|
| `Shift` | normal action exists | 0 | normal GLR advance |
| `Insert(t)` | `t` is valid in the current state; `t != EOF` | 1 | push a zero-width missing token, then apply reduce/goto as needed |
| `Delete` | lookahead is not EOF | 1 | wrap the lookahead in an error node and consume it |
| `Recover(s)` | a stack summary entry `s` admits the lookahead | `recover_cost` | pop to `s`, wrap popped trees in an error node; used only when no insert/delete sequence works, because it is not part of CPCT+'s completeness claim |

Search:

1. Seed from every failed head at cost 0.
2. Use a cost-bucketed queue (Dijkstra/BFS; costs are small integers). For each
   configuration, expand actions in a stable order.
3. Merge compatible configurations on the fly:
   * same GSS head/state and token index;
   * same number of trailing shifts;
   * both end in delete or neither does.
   This is CPCT+'s compatible-configuration merge, extended with the GSS head.
   The repair sequences are merged as a DAG, so the complete set is represented
   without copying sequences.
4. Success: accept, or `nshifts` (default 3) shifts after the last repair.
5. When the first success at cost `c` is found, finish exploring cost `c`,
   discard higher-cost configurations, and collect all successful
   configurations. That is the complete set at cost `c`.
6. Rank the successful configurations by how far they parse within `ntry`
   (default 250) tokens after the repair. Keep the top rank. Expand repair
   sequences from the DAG, remove trailing shifts, deduplicate, and return.
   If several sequences remain in the top rank, choose the lexicographically
   smallest stable sequence; the choice is recorded in the report.
7. Apply the chosen sequence: insert `MISSING` tokens for inserts, wrap deleted
   tokens in `ERROR`, and resume the normal GLR parse from the resulting
   configuration. Record the repair sequence in diagnostics.

Budget:

* `max_configs`, `max_repairs`, `max_repair_cost`, `time_budget_ms`;
* on exhaustion, return the best successful sequence found so far and mark
  `report.truncated = true`; if none, fall back to the fast tier.

Completeness is stated precisely:

> In `complete` mode, if the search does not hit a budget, does not prune a
> GLR version, and the grammar has no infinite ambiguity, the returned set is
> exactly the set of minimum-cost insert/delete repair sequences for the
> explored GLR frontier, using CPCT+'s success condition and cost model.

This is weaker than “complete for all possible GLR parses”, because pruning is
an explicit engineering choice. The report states which conditions held.

#### 6.5.4 Choosing between tiers

| Mode | Behavior | Intended use |
|---|---|---|
| `off` | No recovery; the first error produces an `ERROR`-root tree and a diagnostic. | Validation, tests, strict mode. |
| `fast` | Online tier only. | Keystroke latency, very large files, low error density. |
| `balanced` (default) | Online first; run complete search only if the error region is small (`repair_span` tokens and `max_configs`), otherwise keep the online result. | Editor default. |
| `complete` | Complete search; fall back to online on budget exhaustion. | Diagnostics, benchmarks, test oracles. |

The parser's default is `balanced`, not `complete`: CPCT+'s own stress test
shows the search can be unbounded on unmatched brackets, and an editor cannot
pay that on every keystroke.

The report is part of the output, not an opaque side effect:

```rust
pub enum RepairKind { Insert, Delete, Recover }
pub struct Repair {
    pub kind: RepairKind,
    pub token_kind: Option<TokenKindId>,
    pub range: TextRange,
    pub cost: u32,
}
pub struct RecoveryReport {
    pub mode: RecoveryMode,
    pub cost: u32,
    pub complete: bool,
    pub truncated: bool,
    pub repairs: Arc<[Repair]>,
}
```

### 6.6 Ambiguity and dynamic precedence

* A declared conflict creates multiple GLR versions or packed links.
* At accept, the best tree is chosen by error cost, then total dynamic
  precedence, then stable production order.
* Dynamic precedence is additive over the tree, as in tree-sitter.
* A `conflict` declaration may be scoped to rule pairs; an unlisted pair that
  conflicts is a compile error. This prevents “GLR everywhere” behaviour.
* A fragile region is created when a conflict is explored, even if it later
  collapses. This is conservative and is the reason reuse is safe.

### 6.7 Complexity

Let:

* `t` = number of new/changed tokens;
* `s` = number of edit sites;
* `h` = height of the old tree;
* `g` = GSS work in fragile regions;
* `r` = recovery search configurations.

Typical: `O(t + s·h + g + r)`; with a balanced tree `h = O(log n)`. This matches
Wagner's `O(t + s log N)` typical bound for IGLR and Sijm's observed speedups.
Worst case: `O(n^3)` for GLR on an ambiguous grammar, plus CPCT+'s search
space; budgets convert these into diagnostics rather than hangs.

---

## 7. Correctness

### 7.1 Lexer

**L1 (restart).** For a deterministic pushdown transducer, the suffix token
stream from a boundary depends only on `(state, suffix text)`. Proof: the
transition function is deterministic and reads only the current character,
the mode, the value stack, and the column; all of those are in `LexStateId`,
and the column is updated from the characters the transition consumes. Prior
text outside the current line is not observable by a guard, so it is not part
of the state. This is why `LexStateId` is a sufficient restart key.

**L2 (reconvergence).** If relexing reaches a token boundary `j` where the new
state equals the old `start_state` and no edit intersects the old suffix from
`j`, the old suffix tokens are valid for the new text. Proof: L1 plus the fact
that the suffix text is unchanged; offsets resolve through the edit map.

**L3 (coverage).** Every lexer step consumes at least one scalar or changes the
state machine in a way that cannot repeat indefinitely; the compiler's
zero-width checks and the runtime's progress guard establish exhaustion. Error
tokens consume one scalar by default, so they make progress.

### 7.2 Parser

**P1 (reuse).** The certificate theorem in §6.4.4. The proof relies on the
grammar contract, the deterministic tie-break, the lexer restart theorem, and
the absence of fragility.

**P2 (from-scratch equality).** Induct over the input stream. At each position,
the incremental parser either:
* shifts a certified old subtree, which by P1 yields the same configuration
  and tree as reparsing it; or
* breaks down the subtree and handles its children/tokens, eventually shifting
  new tokens or smaller certified subtrees.

The parser's actions on new tokens are exactly the table actions of a batch
parse. The final tree is therefore structurally equal to a batch tree, modulo
IDs and the chosen alternative under ambiguity (the tie-break is the same).

**P3 (termination).** RN reduction entries and GSS deduplication ensure every
reduction consumes a finite GSS prefix and every shift advances the input. The
version cap and budget ensure the GLR search is finite even for ambiguous
grammars. The grammar compiler rejects nullable cycles that would create
infinitely many zero-width derivations; grammars with finite but ambiguous
nullable structure are handled by RN tables and packing.

**P4 (recovery coverage).** Every error action either inserts a zero-width token,
consumes a token into an `ERROR` node, or pops a finite GSS prefix into an
`ERROR` node. Progress is therefore monotone in `(token index, stack depth)`.
At EOF, any remaining stack is wrapped in an `ERROR` node and accepted. No
input token is skipped without being represented.

**P5 (complete repairs).** The search explores configurations in nondecreasing
repair cost. Insert/delete costs are positive; shifts are zero but bounded by
`nshifts`/`ntry`. When the first success cost is found, all configurations of
lower cost have been exhausted and all configurations of equal cost are
finished before the set is returned. Merging preserves at least one
representative of every compatible repair class, so no minimum-cost sequence is
lost. This is the CPCT+ completeness argument, adapted to the GSS head and
documented with its budget caveats.

### 7.3 Determinism and concurrency

* All IDs and table orders come from declaration order or deterministic
  traversal order.
* Version comparison and repair ordering use integer scores and stable
  sequence numbers.
* No hash-map iteration affects output; hash maps are used only for
  membership/dedup where order is irrelevant.
* `Parser`/`Lexer` state is immutable between runs. Scratch GSS/repair
  structures are local to one `update`.
* The runtime guarantees single-flight per node, so two threads cannot run the
  same parser invocation concurrently. Different runtime instances (and other
  components in the same runtime) may run in parallel; they share only
  immutable `Arc<CompiledGrammar>` and text snapshots.
* Cancellation and budget exhaustion leave the previous state valid; the
  runtime's commit protocol discards the attempt.

### 7.4 Assumptions and counterexamples

The plan is honest about the cases where the guarantees do not apply:

1. **A grammar with an infinite nullable cycle** is rejected. If a future
   feature allows it, the parser must cap derivations and report a cycle; it
   cannot promise a finite tree.
2. **A grammar with unbounded genuine ambiguity** has an exponential number of
   trees; `best` mode chooses one by tie-break, `forest` mode may exhaust
   memory. Budgets turn this into a diagnostic.
3. **A lexer with a context-dependent state that is not captured in
   `LexState`** violates L1. The DSL only permits guard/action state; there are
   no hidden globals or Rust callbacks in the default mode.
4. **An external tokenization rule with side effects** would violate
   determinism and reuse. The optional escape hatch must be pure and
   serializable; this is a contract, not a convention.
5. **A parser-driven lexical decision** (JavaScript regex vs. division) is not
   representable in the default separated lexer/parser model. The parser sees
   one tokenization. This is a documented boundary, not a silent failure; see
   §10.
6. **Tree-sitter's `allow_node_reuse` restriction** is deliberately relaxed
   here, but only for nodes with a complete certificate and no fragility. The
   tests include a GLR grammar with a conflict before an unchanged subtree to
   validate that reuse does not change the chosen tree.

---

## 8. Verification and acceptance gates

### 8.1 Test oracles

* **Lexer oracle.** Relex the entire new text from scratch with a fresh lexer
  and compare kind, text, start/end state, flags, and spans (resolved to byte
  offsets). Compare IDs separately: unchanged tokens must keep IDs.
* **Parser oracle.** Parse the entire new text from scratch with a fresh parser
  and compare the tree structurally: kind, fields, child order, token kinds and
  text, error/missing structure, and diagnostics (modulo spans represented as
  anchors). Ignore `NodeId`/`TokenId` values.
* **Recovery oracle.** For small inputs and a bounded grammar, compute the
  complete minimum-cost repair set by brute force (enumerate insert/delete
  sequences up to cost `c`) and compare with `complete` mode.
* **Edit generator.** Random insertions, deletions, replacements, and batches;
  edits at token boundaries, inside tokens, inside strings/comments, inside
  error regions, and inside ambiguous regions. Fixed seeds for reproducibility.
* **Corpus.** JSON, an expression grammar with precedence, a declared
  ambiguity, XML-like states, indentation, a nullable grammar, and a small
  C-like grammar with comments/strings.

### 8.2 Gates

Names are concrete; a reviewer can add them to the workspace suite.

| ID | Gate | What it proves |
|---|---|---|
| L-01 | `lexer_restart_at_every_boundary_matches_fresh_lex` | L1 |
| L-02 | `lexer_tokens_cover_every_byte_once` | L2 |
| L-03 | `lexer_relexes_only_the_affected_window` | L3; counts relexed characters, not time |
| L-04 | `unchanged_tokens_keep_identity` | L4 |
| L-05 | `lexer_error_tokens_never_drop_input` | L5 |
| L-06 | `zero_width_indent_tokens_terminate` | zero-width progress |
| L-07 | `xml_like_end_tags_match_via_generic_stack_guards` | no XML built-ins |
| L-08 | `nested_comments_and_cdata_reconverge` | stateful relex |
| L-09 | `guarded_maximal_munch_matches_bruteforce` | guard fallback |
| P-01 | `grammar_with_undeclared_conflict_fails_with_counterexample` | conflict policy |
| P-02 | `declared_conflict_parses_both_interpretations` | GLR |
| P-03 | `rnglr_nullable_grammar_terminates_and_matches_earley` | RN correctness |
| P-04 | `lalr_and_lr1_tables_accept_the_same_language_corpus` | table modes |
| P-05 | `incremental_parse_matches_from_scratch_after_every_random_edit` | P1/P2 |
| P-06 | `unaffected_subtrees_keep_node_identity` | P2 |
| P-07 | `reuse_certificate_rejects_changed_right_context` | P3 |
| P-08 | `glr_reuse_does_not_change_the_chosen_tree` | P3 in multiple heads |
| P-09 | `edit_inside_comment_reparses_to_eof` | P4 fallback |
| P-10 | `changed_ranges_cover_edits_and_no_other_structure` | changed ranges |
| P-11 | `parser_returns_a_tree_for_arbitrary_malformed_input` | D2 |
| R-01 | `recovery_tree_covers_all_tokens_without_duplicates` | P5 |
| R-02 | `complete_recovery_returns_all_min_cost_repairs` | P6 |
| R-03 | `complete_recovery_matches_bruteforce_on_small_inputs` | P6 |
| R-04 | `recovery_is_deterministic_across_runs_and_worker_counts` | P7 |
| R-05 | `error_nodes_and_missing_tokens_are_never_reused` | P8 |
| R-06 | `edit_inside_error_region_recomputes_only_that_region` | incremental recovery |
| R-07 | `unmatched_brackets_hit_budget_not_a_hang` | CPCT+ stress case |
| G-01 | `grammar_diagnostics_have_spans_and_suggestions` | D1 |
| G-02 | `lexer_rule_conflicts_are_diagnosed` | D1 |
| G-03 | `nullable_cycles_are_rejected` | termination |
| G-04 | `field_and_kind_names_are_stable_across_compiles` | determinism |
| S-01 | `parse_snapshot_is_pinned_after_edits` | snapshot contract |
| S-02 | `cancelled_parse_publishes_nothing` | R1 |
| S-03 | `unchanged_parse_does_not_notify_observers` | R3 |
| S-04 | `parser_does_not_hold_runtime_locks_while_parsing` | R2 |
| S-05 | `syntax_view_node_units_change_independently` | optional phase |
| S-06 | `syntax_view_is_atomic_with_the_tree` | optional phase |
| B-01 | `incremental_parse_is_cheaper_than_batch_for_local_edits` | cost model |
| B-02 | `relex_work_is_independent_of_file_length` | cost model |
| B-03 | `glr_edge_visits_are_reported` | GLR cost observability |
| B-04 | `table_size_and_compile_time_are_reported` | grammar ergonomics |

### 8.3 Benchmark design

The benchmark matrix must separate tokenization, parsing, and recovery:

| Dimension | Values |
|---|---|
| File size | 10 KiB, 100 KiB, 1 MiB, 10 MiB |
| Edit kind | insert one char, delete one char, replace identifier, insert unbalanced bracket, edit inside string/comment, edit inside ambiguity |
| Edit position | start, middle, end, inside a fragile region |
| Mode | batch, incremental, incremental + fast recovery, incremental + complete recovery |
| Grammar | JSON, expression, XML-like, indentation, nullable, C-like |
| Metric | wall time, relexed chars, reused tokens/nodes, reparsed tokens, GLR versions, GSS edge visits, repairs, peak memory |

Report reuse counts next to time. A fast run that reparsed the whole file is
not evidence of incrementality. The acceptance target for a local edit in a
large file is:

* relexed characters ≤ a small multiple of the edited span plus one token;
* reparsed tokens ≤ the changed region plus `O(spine)` old subtrees;
* `NodeId` reuse for untouched siblings;
* no full-file relex unless the lexer state genuinely fails to reconverge.

### 8.4 Fuzzing

* **Lexer fuzz:** arbitrary UTF-8 edits and grammar constraints; no panic, no
  infinite loop, full coverage, incremental = from-scratch.
* **Parser fuzz:** arbitrary token streams and text; no panic, tree coverage,
  error nodes explicit, incremental = from-scratch, deterministic.
* **Grammar fuzz:** mutate grammar declarations; compiler returns a diagnostic,
  never panics; valid mutations preserve table invariants.
* **Cancellation fuzz:** cancel at random steps; no partial publication; next
  demand recomputes correctly.

---

## 9. Implementation phases

Each phase is independently shippable and has a gate before the next begins.
The parser and lexer are named `Parser` and `Lexer` from phase 1 onward.

### Phase 0 — Text and grammar IR (no parser)

* `TextSnapshot` index, versioned anchors, edit maps, `TextDocument`,
  `TextHistory`.
* DSL parser (`Grammar::compile`) and IR; diagnostics with spans.
* Validation: names, reachability, nullable cycles, lexer state references.
* Gate: G-01, G-04; text anchor unit tests.

### Phase 1 — Lexer compiler and runtime

* Regex/pattern AST, NFA, DFA, guarded accept fallback.
* Mode/value-stack runtime, zero-width progress checks, error tokens.
* `Lexer` and `LexState`; batch lexing.
* Gates: L-01, L-02, L-05, L-06, L-07, L-09, G-02.

### Phase 2 — Lexer incrementality

* Lookahead extents, relex window, reconvergence, token identity reuse.
* `Lexer::lex_incremental`; `LexOutput`.
* Gates: L-03, L-04, L-08, B-02; randomized lexer oracle.

### Phase 3 — Parser tables and batch GLR

* BNF lowering, LALR(1) tables, precedence/conflicts, RN reductions.
* GSS, versions, reductions, packing, dynamic precedence.
* `Parser::parse` (batch), `SyntaxTree`, node metadata.
* Gates: P-01..P-04, P-11, G-03, B-03, B-04.

### Phase 4 — Incremental parser

* Reuse certificate, input stream/tree zipper, fragile regions, breakdown.
* `Parser::parse_incremental`, changed ranges, stable IDs.
* Gates: P-05..P-10, B-01; randomized parser oracle.

### Phase 5 — Error recovery

* Fast tier: missing, skip, recover-to-state, cost ranking.
* Quality tier: cost-ordered repair search, config merging, ranking, budgets.
* Error tree invariants, diagnostics, `RecoveryReport`.
* Gates: R-01..R-07; corpus of invalid programs.

### Phase 6 — Reactive integration and optional Syntax view

* `Parser`/`Lexer` as `GeneralComponent`s; `SourceFile`; install/read/observe.
* Snapshot, cancellation, cutoff, atomic definitions.
* Optional `Syntax` view: per-node units, retraction, identity.
* `grammar!` macro (optional) generating static specs and typed enums.
* Gates: S-01..S-06; end-to-end examples.

### Phase 7 — Hardening and measurement

* Corpus, fuzzing, benchmark matrix, memory accounting.
* Table serialization/caching if needed.
* Documentation: grammar guide, examples, recovery guide, design notes.
* Gate: the full matrix in §8.3, with no unexplained regression.

Dependencies: phase 1 needs phase 0; phase 3 needs phase 1; phase 4 needs
phase 3; phase 5 can begin after phase 3 for batch error recovery and finish
after phase 4 for incremental recovery; phase 6 needs phase 4 (and phase 5 for
diagnostics).

---

## 10. Risks, boundaries, and open questions

### 10.1 Risks

| Risk | Mitigation |
|---|---|
| GLR worst-case blowup on ambiguous grammars | explicit conflicts only; version cap; error-cost pruning; budgets; fragile-region locality; benchmark with a genuinely ambiguous grammar. |
| LALR introduces spurious conflicts | `table = lr1`/`ielr`; conflict diagnostics; tests compare accepted languages. |
| Reuse certificate is subtly unsound | theorem plus randomized differential tests; conservative fragile flags; right-context and lookahead-end checks. |
| CPCT+ search is unbounded | time/config/repair budgets; fallback to fast tier; `truncated` flag; default balanced. |
| Stateful lexer guards become too expressive or hard to analyze | guards are a total expression language; no Rust callbacks in default mode; bounded value stack; static zero-width checks and reachability diagnostics. |
| Text anchors complex and memory-hungry | versioned offsets plus a compressed edit map; piece table/rope only for text storage; measure resolution cost and prune maps with snapshots. |
| Parser and lexer composition limited by one general root | Parser owns lexer; standalone Lexer runtime; future multi-general-component support. |
| `Syntax` view has too many units | phase 6, only changed nodes; measure unit counts; allow consumers to use the coarse tree until it lands. |
| Grammar compilation is slow for large languages | table compression; compile once; cache serialized tables; benchmark C-like/Java-like grammars. |
| Error diagnostics are generic and unhelpful | expected-token sets, repair sequences, spans, and grammar-provided names; keep language-specific messages out of the core. |

### 10.2 Explicit boundaries

* **No scannerless parsing.** The adapters are lexer + parser. A future
  scannerless mode would be a different parser algorithm, not a flag.
* **No parser-driven tokenization in the default mode.** JavaScript's
  regex/division ambiguity, Haskell layout as a parser feature, and similar
  cases need either a lexer state trick or an external rule. The lexer's
  pushdown state handles indentation and matching delimiters, which are the
  requested cases; it does not solve arbitrary context-sensitive lexing.
* **No semantic actions in productions.** The CST is the product. AST
  construction is a downstream component or a generated mapping, not part of
  the GLR parse.
* **No incremental grammar changes.** A grammar is fixed for a parser
  instance. Reloading a grammar invalidates the tree and requires a fresh
  parse; a future `Parser::set_grammar` can do this atomically.
* **No durable cross-process cache.** Table and tree serialization are future
  work; the first implementation compiles in process.
* **No complete repair guarantee under GLR pruning.** The report states
  whether the complete conditions held.
* **No claim that every language can be parsed efficiently.** Exponential
  ambiguity is a property of the grammar; budgets make it observable instead
  of hanging.

### 10.3 Open questions

1. Should `SyntaxTree` use `Arc<Node>` per node, or a compact arena with
   stable IDs? The plan starts with `Arc` for clarity; benchmark memory before
   choosing a packed representation.
2. Should `table = lalr` be the default for all grammars, or should the
   compiler try LALR and fall back to IELR when conflict count is high? The
   initial default is LALR; the report exposes the conflict count.
3. Is CPCT+'s `nshifts=3`/`ntry=250` appropriate for editor-scale recovery, or
   should the adapter expose language-specific profiles? Defaults are
   configurable; profiles are future work.
4. Should the lexer support a Rust external-rule hook before the optional
   `Syntax` view? It is useful for heredocs and unusual literals, but it needs
   a serialization contract; it is not on the critical path.
5. Should the optional `Syntax` view reuse the `Tree` family's unit layout, or
   define a dedicated layout with spans? The plan favors a dedicated layout
   because parse nodes need anchors and token leaves; the final decision is
   phase 6.

---

## 11. References

Primary sources are preferred. Links are to the publisher page or the
authoritative PDF used while drafting; source-code links point at the files
whose behavior is described.

* [T86] Masaru Tomita. *Efficient Parsing for Natural Language: A Fast
  Algorithm for Practical Systems.* Kluwer, 1986.
  <https://doi.org/10.1007/978-1-4757-1885-0>
* [FAR91] Rahman Nozohoor-Farshi. “GLR Parsing for ε-Grammers.” In
  *Generalized LR Parsing*, Kluwer, 1991.
  <https://doi.org/10.1007/978-1-4615-4034-2_5>
* [SJ06] Elizabeth Scott and Adrian Johnstone. “Right Nulled GLR Parsers.”
  *ACM TOPLAS* 28(4), 2006.
  <https://doi.org/10.1145/1146809.1146810>
* [SJE07] Elizabeth Scott, Adrian Johnstone, and Rob Economopoulos. “BRNGLR:
  a cubic Tomita-style GLR parsing algorithm.” *Acta Informatica* 44, 2007.
  <https://doi.org/10.1007/s00236-007-0054-z>
* [JSE06] Adrian Johnstone, Elizabeth Scott, and Giorgios Economopoulos.
  “Evaluating GLR parsing algorithms.” *Science of Computer Programming* 61(3),
  2006. <https://doi.org/10.1016/j.scico.2006.04.004>
* [EKV09] Giorgios Economopoulos, Paul Klint, and Jurgen Vinju. “Faster
  Scannerless GLR Parsing.” *Compiler Construction* (CC), 2009.
  <https://doi.org/10.1007/978-3-642-00722-4_10>
  PDF: <https://ir.cwi.nl/pub/15095/15095B.pdf>
* [WG98a] Tim A. Wagner and Susan L. Graham. “Efficient and Flexible
  Incremental Parsing.” *ACM TOPLAS* 20(2), 1998.
  <https://doi.org/10.1145/293677.293678>
  PDF: <https://harmonia.cs.berkeley.edu/papers/twagner-parsing.pdf>
* [W98] Tim A. Wagner. *Practical Algorithms for Incremental Software
  Development Environments.* PhD thesis, UC Berkeley, 1998. Especially ch. 6
  (incremental parsing), ch. 7 (incremental GLR), ch. 8 (history-sensitive
  error recovery).
  PDF: <https://digitalassets.lib.berkeley.edu/techreports/ucb/text/CSD-97-946.pdf>
* [WG99] Tim A. Wagner and Susan L. Graham. “General Incremental Lexical
  Analysis.” 1999 (TOPLAS submission version used here).
  PDF: <https://harmonia.cs.berkeley.edu/papers/twagner-lexing.pdf>
* [SIJ20] Maarten P. Sijm. “SPLASH: G: Incremental Scannerless Generalized LR
  Parsing.” ACM Student Research Competition Grand Finals, 2020.
  <https://doi.org/10.1145/3359061.3361085>
  PDF: <https://src.acm.org/binaries/content/assets/src/2020/maarten-p.-sijm.pdf>
* [CPCT] Lukas Diekmann and Laurence Tratt. “Don't Panic! Better, Fewer,
  Syntax Errors for LR Parsers.” *ECOOP* 2020.
  <https://doi.org/10.4230/LIPIcs.ECOOP.2020.6>
  arXiv: <https://arxiv.org/abs/1804.07133>
* [BF87] Michael G. Burke and Gerald A. Fisher. “A practical method for LR and
  LL syntactic error diagnosis and recovery.” *ACM TOPLAS* 9(2), 1987.
  <https://doi.org/10.1145/22719.22720>
* [COR02] Rafael Corchuelo, José A. Pérez, Antonio Ruiz, and Miguel Toro.
  “Repairing syntax errors in LR parsers.” *ACM TOPLAS* 24(6), 2002.
  <https://doi.org/10.1145/586088.586092>
* [AP72] Alfred V. Aho and Thomas G. Peterson. “A Minimum Distance
  Error-Correcting Parser for Context-Free Languages.” *SIAM Journal on
  Computing* 1(4), 1972. <https://doi.org/10.1137/0201022>
* [KAT09] Lennart C. L. Kats, Maartje de Jonge, Emma Nilsson-Nyman, and Eelco
  Visser. “Providing Rapid Feedback in Generated Modular Language
  Environments: Adding Error Recovery to Scannerless Generalized-LR Parsing.”
  *OOPSLA* 2009. <https://doi.org/10.1145/1640089.1640122>
* [SUB89] G. V. Cormack. “An LR substring parser for noncorrecting syntax
  error recovery.” *PLDI* 1989.
  <https://doi.org/10.1145/73141.74832>
* [DP79] Frank DeRemer and Thomas Pennello. “Efficient computation of LALR(1)
  look-ahead sets.” *ACM SIGPLAN Notices* 14(8), 1979.
  <https://doi.org/10.1145/800229.806968>
* [IELR] Joel E. Denny and Brian A. Malloy. “The IELR(1) algorithm for
  generating minimal LR(1) parser tables for non-LR(1) grammars with conflict
  resolution.” *Science of Computer Programming* 75(11), 2010.
  <https://doi.org/10.1016/j.scico.2009.08.001>
* [TS] Tree-sitter documentation and runtime source:
  * implementation notes: <https://tree-sitter.github.io/tree-sitter/5-implementation.html>
  * grammar DSL: <https://github.com/tree-sitter/tree-sitter/blob/master/docs/src/creating-parsers/2-the-grammar-dsl.md>
  * external scanners: <https://github.com/tree-sitter/tree-sitter/blob/master/docs/src/creating-parsers/4-external-scanners.md>
  * parser/GSS/recovery: <https://github.com/tree-sitter/tree-sitter/blob/master/lib/src/parser.c>
  * stack versions/merging: <https://github.com/tree-sitter/tree-sitter/blob/master/lib/src/stack.c>
  * subtree flags/reuse: <https://github.com/tree-sitter/tree-sitter/blob/master/lib/src/subtree.h>
  * error costs: <https://github.com/tree-sitter/tree-sitter/blob/master/lib/src/error_costs.h>
* [LEZ] Marijn Haverbeke. *Lezer System Guide.*
  <https://lezer.codemirror.net/docs/guide/>
* [JG82] Fahimeh Jalili and Jean H. Gallier. “Building friendly parsers.”
  *POPL* 1982. <https://doi.org/10.1145/582153.582175>
* [LAR95] J.-M. Larchevêque. “Optimal incremental parsing.” *ACM TOPLAS*
  17(1), 1995. <https://doi.org/10.1145/200994.200996>
* [HKR92] Jan Heering, Paul Klint, and Jan Rekers. “Incremental generation of
  lexical scanners.” *ACM TOPLAS* 14(4), 1992.
  <https://doi.org/10.1145/133233.133240>

---

## Appendix A — DSL grammar (informative)

This is the accepted surface syntax; the compiler may accept supersets, but
the implementation must reject an ambiguous construct rather than guess.

```ebnf
Grammar      ::= "grammar" Ident? "{" Start? Lexer? Parser? Inline* Extras? Precedence? Conflict* Table? Recovery? "}"
Start        ::= "start" Ident ";"
Lexer        ::= "lexer" "{" Initial? LexState* "}"
Initial      ::= "initial" Ident ";"
LexState     ::= "state" Ident "{" LexRule* "}"
LexRule      ::= ("skip" | "token") TokenName? "=" Pattern Attr* Guard? Action? ";"
               | "token" String Attr* Guard? Action? ";"
TokenName    ::= Ident | String
Attr         ::= "@word" | "@extra" | "@priority" "(" Integer ")"
Pattern      ::= Alt
Alt          ::= Seq ("|" Seq)*
Seq          ::= Rep+
Rep          ::= Atom ("*" | "+" | "?")?
Atom         ::= String | Regex | "(" Alt ")" | "." | Class
Guard        ::= "when" Expr
Action       ::= "->" ActionList
ActionList   ::= Action ("," Action)*
Action       ::= "stay" | "switch" "(" Ident ")"
               | "push" "(" Ident ")"
               | "pop"
               | "push_value" "(" Expr ")"
               | "set_value" "(" Expr ")"
               | "pop_value"
               | "error" "(" String ")"
Expr         ::= Or
Or           ::= And ("or" And)*
And          ::= Cmp ("and" Cmp)*
Cmp          ::= Add (("==" | "!=" | "<" | "<=" | ">" | ">=") Add)?
Add          ::= Mul (("+" | "-") Mul)*
Mul          ::= Unary (("*" | "/") Unary)*
Unary        ::= "not" Unary | Primary
Primary      ::= Ident | "$" Ident | Integer | String | "(" Expr ")"
Parser       ::= "parser" "{" Rule* "}"
Rule         ::= Ident "=" Alternatives ";"
Inline       ::= "inline" Ident ";"
Extras       ::= "extras" "{" (Ident ("," Ident)*)? "}"
Alternatives ::= Sequence ("|" Sequence)*
Sequence     ::= Element*
Element      ::= Primary Suffix?
Primary      ::= Ident | String | Field | Prec | Group
Field        ::= Ident ":" Primary
Prec         ::= "@prec" "(" Ident ("," ("left" | "right" | "nonassoc"))? ")"
               | "@dynamic" "(" Integer ")"
Group        ::= "(" Alternatives ")"
Suffix       ::= "*" | "+" | "?"
Precedence   ::= "precedence" "{" PrecDecl* "}"
PrecDecl     ::= Ident "=" ("left" | "right" | "nonassoc") ";"
Conflict     ::= "conflict" Ident "," Ident ";"
Table        ::= "table" "=" ("lalr" | "lr1" | "ielr") ";"
Recovery     ::= "recovery" "{" RecoverySetting* "}"
RecoverySetting ::= Ident "=" (Ident | Integer) ";"
```

The parser for this grammar is hand-written recursive descent. It is not
self-hosted: bootstrapping a parser generator with its own DSL would add a
dependency cycle and obscure diagnostics for the first version.

## Appendix B — Pseudocode

### B.1 Token reconvergence

```text
find_relex_window(old_tokens, edits):
    lo = 0
    hi = old_tokens.len()
    for edit in sorted(edits):
        lo = min(lo, first_token_with_end_after(edit.start))
        hi = max(hi, last_token_with_start_before(edit.end))
    while lo > 0 and old_tokens[lo - 1].lookahead_end >= edits[0].start:
        lo -= 1
    return (lo, hi)

relex(old_tokens, lo, documents):
    state = old_tokens[lo].start_state
    pos = old_tokens[lo].range.start
    out = []
    while True:
        if at_old_boundary(j) and no_edit_in(old_suffix(j))
           and state == old_tokens[j].start_state:
            out.extend(old_tokens[j..])   // anchors stay valid; positions resolve later
            return splice(old_tokens, lo, out)
        step = lex_one(state, pos, documents.current)
        if step is empty and no_progress():
            fail("lexer made no progress")
        out.push(reuse_or_new(step.token, old_tokens, j))
        state = step.next_state
        pos = step.next_position
```

### B.2 Incremental parse

```text
parse_incremental(old, document):
    lexed = relex(old.tokens, document.edits)
    if lexed == old.tokens and document.snapshot == old.text:
        return old.output

    input = Input::new(lexed.tokens, old.tree, document.edits)
    glr = Glr::new(grammar, input)

    while glr.active():
        glr.step_all()
        glr.condense()
        if glr.error():
            glr.recover()

    tree = glr.finish()
    return ParseOutput::new(tree, lexed, diff(old.tree, tree))
```

### B.3 Complete repair search

```text
complete_repair(failed_heads, tokens):
    frontier = CostBuckets::new()
    for head in failed_heads:
        frontier.push(0, Config { head, token: head.cursor, repairs: empty })

    successes = []
    first_success_cost = infinity
    while let Some((cost, batch)) = frontier.pop_lowest():
        if cost > first_success_cost: break
        for config in batch:
            if success(config):
                first_success_cost = cost
                successes.push(config)
                continue
            for action in expand(config):
                next = apply(action, config)
                if next.cost <= first_success_cost:
                    frontier.push(next.cost, next)
        merge_compatible(frontier)

    ranked = rank_by_furthest(successes, ntry)
    return expand_repairs(ranked, remove_trailing_shifts)
```

## Appendix C — Worked expectations

### C.1 JSON edit

Text:

```json
{"a": 1, "b": [2, 3]}
```

Edit `3` to `30` inside the array. The lexer relexes `3` to `30` (one token,
same state), reuses the surrounding tokens, and reconverges at `]`. The parser
reuses the unchanged prefix up to `Pair("a")` and the unchanged suffix after
the array; it reparses the number, rebuilds the array and the enclosing
`Pair("b")`/`Object`/`Document` ancestors, and splices the reused suffix.
`Pair("a")` keeps its `NodeId`; the changed ancestors get new IDs. The changed
range is the number token and the ancestors whose child structure changed, not
the whole file.

### C.2 Unbalanced bracket

Text:

```json
{"a": 1, "b": [2, 3}
```

The parser reaches `}` where `]` is expected. Fast tier: it tries inserting
`]` as a `MISSING` token; the following `}` then parses. The tree has an array
node with a missing `]`; the diagnostic reports the insertion. Complete tier:
it searches insert/delete sequences and reports the minimum-cost set, which may
include deleting `}` or inserting `]`; it ranks by how far parsing continues.
The chosen tree still covers every input token exactly once.

### C.3 XML mismatch

Text:

```xml
<a><b></a>
```

The lexer's value stack holds `a`, then `b`. The end tag `</a>` fails the
`$name == top_value` guard, so the `EndTagMismatch` rule emits an error token.
The parser receives the error token and wraps it in an `ERROR` node; the
recovery tier can delete it and continue, producing a diagnostic that the
end tag does not match the innermost open element. The lexer never guesses
that `</a>` closes `<a>` by scanning backward; the stack comparison is the
whole mechanism.
