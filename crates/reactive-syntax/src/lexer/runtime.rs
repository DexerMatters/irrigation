//! Lexer runtime, including the incremental relex/reconvergence algorithm.

use std::sync::Arc;

use crate::diagnostic::Diagnostic;
use crate::text::{TextDocument, TextEdit, TextRange, TextSnapshot};

use super::guard::EvalContext;
use super::spec::{CompiledAction, LexerSpec, Rule, RuleKind};
use super::{
    LexConfig, LexOutput, LexState, LexerOptions, Token, TokenFlags, TokenId, TokenKindId,
    TokenStream,
};

/// A lexer compiled from a [`LexerSpec`].
#[derive(Clone, Debug)]
pub struct Lexer {
    spec: Arc<LexerSpec>,
    options: LexerOptions,
}

/// Whether a token stream spells the document exactly.
///
/// The re-sync heuristic is checked, not trusted: a wrong boundary silently
/// drops or duplicates a token, and every later layer would inherit it. The
/// check compares lexemes against the text, not anchors: an anchor that sits on
/// an insertion point deliberately resolves to the text before the insertion.
fn covers_document(output: &LexOutput, text: &str) -> bool {
    let mut offset = 0usize;
    for token in output.tokens.iter() {
        let end = offset + token.text.len();
        if end > text.len() || &text[offset..end] != token.text.as_ref() {
            return false;
        }
        offset = end;
    }
    offset == text.len()
}

/// Keeps token identities where the lexeme is unchanged.
pub(crate) fn reconcile_tokens(new: &TokenStream, old: &TokenStream) -> TokenStream {
    let mut prefix = 0usize;
    while prefix < new.len()
        && prefix < old.len()
        && new.as_slice()[prefix].kind == old.as_slice()[prefix].kind
        && new.as_slice()[prefix].text == old.as_slice()[prefix].text
    {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < new.len().saturating_sub(prefix)
        && suffix < old.len().saturating_sub(prefix)
        && new.as_slice()[new.len() - 1 - suffix].kind
            == old.as_slice()[old.len() - 1 - suffix].kind
        && new.as_slice()[new.len() - 1 - suffix].text
            == old.as_slice()[old.len() - 1 - suffix].text
    {
        suffix += 1;
    }
    let next_fresh = old
        .iter()
        .map(|token| token.id.0)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    let mut fresh = 0u64;
    let mut tokens = Vec::with_capacity(new.len());
    for (index, token) in new.iter().enumerate() {
        let mut token = (**token).clone();
        token.id = if index < prefix {
            old.as_slice()[index].id
        } else if index >= new.len().saturating_sub(suffix) {
            old.as_slice()[old.len() - (new.len() - index)].id
        } else {
            let id = TokenId(next_fresh.saturating_add(fresh));
            fresh = fresh.saturating_add(1);
            id
        };
        tokens.push(Arc::new(token));
    }
    TokenStream::new(tokens)
}

impl Lexer {
    /// Builds a lexer with default options.
    #[must_use]
    pub fn new(spec: LexerSpec) -> Self {
        Self::with_options(spec, LexerOptions::default())
    }

    /// Builds a lexer with explicit options.
    #[must_use]
    pub fn with_options(spec: LexerSpec, options: LexerOptions) -> Self {
        Self {
            spec: Arc::new(spec),
            options,
        }
    }

    /// The compiled specification.
    #[must_use]
    pub fn spec(&self) -> &LexerSpec {
        &self.spec
    }

    /// Lexes a document from scratch.
    #[must_use]
    pub fn lex(&self, document: &TextDocument) -> LexOutput {
        self.lex_snapshot(Arc::clone(document.snapshot()))
    }

    /// Lexes a document incrementally from retained state.
    ///
    /// The fast path relexes from the first affected token and reconverges with
    /// the unchanged suffix. If an edit removes the start anchor of the first
    /// affected token or the edit history is unavailable, it falls back to a
    /// from-scratch lex.
    #[must_use]
    pub fn lex_incremental(&self, previous: &LexState, document: &TextDocument) -> LexOutput {
        let current = Arc::clone(document.snapshot());
        if previous.text.version() == current.version() {
            if previous.text.text() == current.text() {
                return LexOutput {
                    tokens: Arc::clone(&previous.tokens),
                    text: Arc::clone(&previous.text),
                    diagnostics: Vec::new(),
                    relexed_chars: 0,
                    relexed_tokens: 0..0,
                    replaced_tokens: 0..0,
                };
            }
            return self.relex_all(current, &previous.tokens);
        }
        let Some(edits) = current.edits_since(previous.text.version()) else {
            return self.relex_all(current, &previous.tokens);
        };
        if edits.is_empty() {
            return LexOutput {
                tokens: Arc::clone(&previous.tokens),
                text: Arc::clone(&previous.text),
                diagnostics: Vec::new(),
                relexed_chars: 0,
                relexed_tokens: 0..0,
                replaced_tokens: 0..0,
            };
        }
        if let Some(output) = self.try_incremental(previous, &current, &edits) {
            if covers_document(&output, current.text()) {
                return output;
            }

            if std::env::var_os("REACTIVE_SYNTAX_TRACE").is_some() {
                eprintln!("[syntax] incremental lex did not cover the document");
            }
        }
        self.relex_all(current, &previous.tokens)
    }

    /// Relexes the whole document, keeping token identities that survive.
    ///
    /// Reusing identities is what lets the parser keep its AST: a token that
    /// still exists at the same relation to unchanged neighbours must not look
    /// like a new token, or every splice candidate would belong to the wrong
    /// subtree.
    fn relex_all(&self, current: Arc<TextSnapshot>, previous: &TokenStream) -> LexOutput {
        let mut output = self.lex_snapshot(current);
        let tokens = reconcile_tokens(&output.tokens, previous);
        let len = tokens.len();
        output.tokens = Arc::new(tokens);
        output.relexed_tokens = 0..len;
        output.replaced_tokens = 0..previous.len();
        output
    }

    fn try_incremental(
        &self,
        previous: &LexState,
        current: &Arc<TextSnapshot>,
        edits: &[TextEdit],
    ) -> Option<LexOutput> {
        let deny = |reason: &str| -> Option<LexOutput> {
            if std::env::var_os("LEX_RESYNC_DEBUG").is_some() {
                eprintln!("[resync] fall back: {reason}");
            }
            None
        };
        let old_tokens = previous.tokens.as_slice();
        // Where an old token starts in the new document: its own offset plus the
        // net length of the edits that end before it.
        let new_offset = |old_offset: usize| -> isize {
            let shift: isize = edits
                .iter()
                .filter(|edit| edit.range.end <= old_offset)
                .map(|edit| edit.new_len() as isize - edit.old_len() as isize)
                .sum();
            old_offset as isize + shift
        };
        if old_tokens.is_empty() {
            return deny("empty token stream");
        }

        let mut first = old_tokens.len();
        for edit in edits {
            let mut candidate = old_tokens.len();
            for (index, token) in old_tokens.iter().enumerate() {
                let Some(end) = previous.text.resolve(token.range.end) else {
                    return deny("token end resolve");
                };
                if end >= edit.range.start {
                    candidate = index;
                    break;
                }
            }
            if candidate == old_tokens.len() {
                candidate = old_tokens.len().saturating_sub(1);
            } else {
                let Some(start) = previous.text.resolve(old_tokens[candidate].range.start) else {
                    return deny("candidate start resolve");
                };
                if start > edit.range.start && candidate > 0 {
                    candidate -= 1;
                }
            }
            while candidate > 0 {
                let Some(lookahead) = previous
                    .text
                    .resolve(old_tokens[candidate - 1].lookahead_end)
                else {
                    return deny("lookahead resolve");
                };
                if lookahead >= edit.range.start {
                    candidate -= 1;
                } else {
                    break;
                }
            }
            first = first.min(candidate);
        }
        if first >= old_tokens.len() {
            first = old_tokens.len().saturating_sub(1);
        }

        // A token whose start was deleted cannot seed a relex.
        let start_anchor = old_tokens[first].range.start;
        let Some(start_offset) = current.resolve(start_anchor) else {
            return deny("relex start offset");
        };
        let mut state = old_tokens[first].start_state.clone();
        let mut boundary = first;
        let mut produced: Vec<Arc<Token>> = old_tokens[..first].to_vec();
        let mut next_id = old_tokens
            .iter()
            .map(|token| token.id.0)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let mut diagnostics = Vec::new();
        let mut position = start_offset;
        let text = current.text();
        let mut zero_width_run = 0usize;

        while position < text.len() {
            let (choice, guard_error) = self.best_match(&state, text, position);
            let Some(choice) = choice else {
                let (token, diagnostic, next_position) = self.error_token(
                    current,
                    text,
                    position,
                    &state,
                    TokenId(next_id),
                    guard_error.map_or_else(
                        || "no lexer rule matched".to_string(),
                        |error| format!("lexical guard failed: {error}"),
                    ),
                );
                next_id = next_id.saturating_add(1);
                produced.push(Arc::new(token));
                diagnostics.push(diagnostic);
                position = next_position;
                zero_width_run = 0;
                if position >= text.len() {
                    break;
                }
                continue;
            };

            let rule = &self.spec.rules(state.mode)[choice.rule];
            let start_pos = position;
            let end_pos = choice.matched.end;
            let lexeme = &text[start_pos..end_pos];
            let captures = relative_captures(&choice.matched.captures, start_pos);
            let mut end_state = LexConfig {
                mode: state.mode,
                mode_stack: state.mode_stack.clone(),
                values: state.values.clone(),
                column: self.column_after(state.column, lexeme),
            };
            let mut error_message: Option<Arc<str>> = rule.error_message.clone();
            for action in &rule.actions {
                match action {
                    CompiledAction::Stay => {}
                    CompiledAction::Switch(mode) => end_state.mode = *mode,
                    CompiledAction::Push(mode) => {
                        end_state.mode_stack.push(end_state.mode);
                        end_state.mode = *mode;
                    }
                    CompiledAction::Pop => {
                        if let Some(mode) = end_state.mode_stack.pop() {
                            end_state.mode = mode;
                        } else {
                            diagnostics.push(diagnostic(
                                current,
                                start_pos..end_pos,
                                "lex.mode.pop",
                                "mode stack underflow",
                            ));
                        }
                    }
                    CompiledAction::PushValue(expr) => {
                        let cx =
                            eval_context(&state, lexeme, rule, &captures, start_pos == text.len());
                        match expr.eval(&cx) {
                            Ok(value) if end_state.values.len() < self.options.max_value_depth => {
                                end_state.values.push(value);
                            }
                            Ok(_) => diagnostics.push(diagnostic(
                                current,
                                start_pos..end_pos,
                                "lex.value.depth",
                                "value-stack depth limit exceeded",
                            )),
                            Err(error) => diagnostics.push(diagnostic(
                                current,
                                start_pos..end_pos,
                                "lex.guard",
                                format!("action expression failed: {error}"),
                            )),
                        }
                    }
                    CompiledAction::SetValue(expr) => {
                        let cx =
                            eval_context(&state, lexeme, rule, &captures, start_pos == text.len());
                        match expr.eval(&cx) {
                            Ok(value) if !end_state.values.is_empty() => {
                                *end_state.values.last_mut().expect("checked non-empty") = value;
                            }
                            Ok(_) => diagnostics.push(diagnostic(
                                current,
                                start_pos..end_pos,
                                "lex.value.underflow",
                                "`set_value` on an empty value stack",
                            )),
                            Err(error) => diagnostics.push(diagnostic(
                                current,
                                start_pos..end_pos,
                                "lex.guard",
                                format!("action expression failed: {error}"),
                            )),
                        }
                    }
                    CompiledAction::PopValue => {
                        if end_state.values.pop().is_none() {
                            diagnostics.push(diagnostic(
                                current,
                                start_pos..end_pos,
                                "lex.value.underflow",
                                "`pop_value` on an empty value stack",
                            ));
                        }
                    }
                    CompiledAction::Error(message) => {
                        error_message = Some(Arc::clone(message));
                    }
                }
            }

            if rule.error {
                diagnostics.push(
                    diagnostic(
                        current,
                        start_pos..end_pos,
                        "lex.error",
                        rule.error_message
                            .as_deref()
                            .unwrap_or("lexical error token"),
                    )
                    .with_token_kind(choice_token_kind(rule)),
                );
            }
            if end_pos == start_pos && end_state == state {
                // A zero-width rule that does not change the machine state
                // cannot make progress. Emit an error token for one scalar
                // rather than looping forever.
                let (token, diagnostic, next_position) = self.error_token(
                    current,
                    text,
                    start_pos,
                    &state,
                    TokenId(next_id),
                    "zero-width lexer rule made no progress".to_string(),
                );
                next_id = next_id.saturating_add(1);
                produced.push(Arc::new(token));
                diagnostics.push(diagnostic);
                position = next_position;
                zero_width_run = 0;
                if position >= text.len() {
                    break;
                }
                continue;
            }
            if end_pos == start_pos {
                zero_width_run += 1;
                if zero_width_run > self.options.max_zero_width_run {
                    let (token, diagnostic, next_position) = self.error_token(
                        current,
                        text,
                        start_pos,
                        &state,
                        TokenId(next_id),
                        "too many consecutive zero-width tokens".to_string(),
                    );
                    next_id = next_id.saturating_add(1);
                    produced.push(Arc::new(token));
                    diagnostics.push(diagnostic);
                    position = next_position;
                    zero_width_run = 0;
                    continue;
                }
            } else {
                zero_width_run = 0;
            }

            let Some(range) = current.range(start_pos..end_pos) else {
                return deny("token range anchor");
            };
            let Some(lookahead_end) = current.anchor(choice.matched.examined) else {
                return deny("lookahead anchor");
            };
            let reuse = old_tokens.get(boundary).and_then(|old| {
                (previous
                    .text
                    .resolve(old.range.start)
                    .is_some_and(|base| new_offset(base) == start_pos as isize)
                    && old.start_state == state
                    && old.kind == choice_token_kind(rule)
                    && old.text.as_ref() == lexeme
                    && old.end_state == end_state)
                    .then_some(old.id)
            });
            let reused_boundary = reuse.is_some();
            let id = reuse.unwrap_or_else(|| {
                let id = TokenId(next_id);
                next_id = next_id.saturating_add(1);
                id
            });
            let token = Arc::new(Token {
                id,
                kind: choice_token_kind(rule),
                text: Arc::from(lexeme),
                range,
                start_state: state.clone(),
                end_state: end_state.clone(),
                lookahead_end,
                flags: token_flags(rule, error_message.is_some()),
                error: error_message,
            });
            produced.push(Arc::clone(&token));
            position = end_pos;
            state = end_state;

            // Walk the old stream in step with the new one. Positions are
            // compared in *old* coordinates: a token's own offset plus the net
            // length of the edits that end before it is where that token starts
            // in the new document. Anchors cannot answer this (an anchor at an
            // insertion point stays before the inserted text), so the arithmetic
            // is explicit.
            if boundary < old_tokens.len() && reused_boundary {
                boundary += 1;
            }
            // The boundary is only as good as the arithmetic, so every step is
            // checked against the text: a candidate whose lexeme is not where it
            // claims to be is stale and is skipped rather than trusted.
            let mut resync = None;
            while boundary < old_tokens.len() {
                let candidate = &old_tokens[boundary];
                // `offset` belongs to the version the anchor was created in, not
                // to the previous document; resolve first, then apply the edits.
                let Some(old_offset) = previous.text.resolve(candidate.range.start) else {
                    boundary += 1;
                    continue;
                };
                let at = new_offset(old_offset);
                if at < position as isize {
                    boundary += 1;
                    continue;
                }
                if at > position as isize {
                    break;
                }
                let old_end = old_offset + candidate.text.len();
                // The token is unchanged unless an edit overlaps it: an edit
                // that ends before it has already been folded into `new_offset`,
                // and an edit that starts after it cannot reach it.
                let aligned = state == candidate.start_state
                    && edits
                        .iter()
                        .all(|edit| edit.range.end <= old_offset || edit.range.start >= old_end)
                    && text[position..].starts_with(candidate.text.as_ref());

                if aligned {
                    resync = Some(boundary);
                    break;
                }
                boundary += 1;
            }

            if let Some(boundary) = resync {
                let produced_len = produced.len();
                let replaced = first..boundary;
                produced.extend(old_tokens[boundary..].iter().cloned());
                let tokens = TokenStream::new(produced);
                if std::env::var_os("REACTIVE_SYNTAX_TRACE").is_some() {
                    eprintln!(
                        "[syntax] relex range={}..{} chars={} tokens={} (suffix reused)",
                        start_offset,
                        position,
                        position.saturating_sub(start_offset),
                        tokens.len()
                    );
                }
                return Some(LexOutput {
                    tokens: Arc::new(tokens),
                    text: Arc::clone(current),
                    diagnostics,
                    relexed_chars: position.saturating_sub(start_offset),
                    relexed_tokens: first..produced_len,
                    replaced_tokens: replaced,
                });
            }
        }

        let produced_len = produced.len();
        let replaced_tokens = first..old_tokens.len();
        // Relexing to the end still keeps every lexeme that did not change; the
        // identities are reconciled so a consumer's tree survives the edit.
        let tokens = reconcile_tokens(&TokenStream::new(produced), &previous.tokens);
        if std::env::var_os("REACTIVE_SYNTAX_TRACE").is_some() {
            eprintln!(
                "[syntax] relex range={}..{} chars={} tokens={}",
                start_offset,
                position,
                position.saturating_sub(start_offset),
                tokens.len()
            );
        }
        Some(LexOutput {
            tokens: Arc::new(tokens),
            text: Arc::clone(current),
            diagnostics,
            relexed_chars: position.saturating_sub(start_offset),
            relexed_tokens: first..produced_len,
            replaced_tokens,
        })
    }

    fn lex_snapshot(&self, snapshot: Arc<TextSnapshot>) -> LexOutput {
        let text = snapshot.text();
        let text_len = text.len();
        let mut state = LexConfig::new(self.spec.initial, Vec::new(), 0);
        let mut position = 0usize;
        let mut tokens = Vec::new();
        let mut diagnostics = Vec::new();
        let mut next_id = 0u64;
        let mut zero_width_run = 0usize;

        while position < text.len() {
            let (choice, guard_error) = self.best_match(&state, text, position);
            let Some(choice) = choice else {
                let (token, diagnostic, next_position) = self.error_token(
                    &snapshot,
                    text,
                    position,
                    &state,
                    TokenId(next_id),
                    guard_error.map_or_else(
                        || "no lexer rule matched".to_string(),
                        |error| format!("lexical guard failed: {error}"),
                    ),
                );
                next_id = next_id.saturating_add(1);
                tokens.push(Arc::new(token));
                diagnostics.push(diagnostic);
                position = next_position;
                zero_width_run = 0;
                continue;
            };

            let rule = &self.spec.rules(state.mode)[choice.rule];
            let start_pos = position;
            let end_pos = choice.matched.end;
            let lexeme = &text[start_pos..end_pos];
            let captures = relative_captures(&choice.matched.captures, start_pos);
            let mut end_state = LexConfig {
                mode: state.mode,
                mode_stack: state.mode_stack.clone(),
                values: state.values.clone(),
                column: self.column_after(state.column, lexeme),
            };
            let mut error_message: Option<Arc<str>> = rule.error_message.clone();
            for action in &rule.actions {
                match action {
                    CompiledAction::Stay => {}
                    CompiledAction::Switch(mode) => end_state.mode = *mode,
                    CompiledAction::Push(mode) => {
                        end_state.mode_stack.push(end_state.mode);
                        end_state.mode = *mode;
                    }
                    CompiledAction::Pop => {
                        if let Some(mode) = end_state.mode_stack.pop() {
                            end_state.mode = mode;
                        } else {
                            diagnostics.push(diagnostic(
                                &snapshot,
                                start_pos..end_pos,
                                "lex.mode.pop",
                                "mode stack underflow",
                            ));
                        }
                    }
                    CompiledAction::PushValue(expr) => {
                        let cx =
                            eval_context(&state, lexeme, rule, &captures, start_pos == text.len());
                        match expr.eval(&cx) {
                            Ok(value) if end_state.values.len() < self.options.max_value_depth => {
                                end_state.values.push(value);
                            }
                            Ok(_) => diagnostics.push(diagnostic(
                                &snapshot,
                                start_pos..end_pos,
                                "lex.value.depth",
                                "value-stack depth limit exceeded",
                            )),
                            Err(error) => diagnostics.push(diagnostic(
                                &snapshot,
                                start_pos..end_pos,
                                "lex.guard",
                                format!("action expression failed: {error}"),
                            )),
                        }
                    }
                    CompiledAction::SetValue(expr) => {
                        let cx =
                            eval_context(&state, lexeme, rule, &captures, start_pos == text.len());
                        match expr.eval(&cx) {
                            Ok(value) if !end_state.values.is_empty() => {
                                *end_state.values.last_mut().expect("checked non-empty") = value;
                            }
                            Ok(_) => diagnostics.push(diagnostic(
                                &snapshot,
                                start_pos..end_pos,
                                "lex.value.underflow",
                                "`set_value` on an empty value stack",
                            )),
                            Err(error) => diagnostics.push(diagnostic(
                                &snapshot,
                                start_pos..end_pos,
                                "lex.guard",
                                format!("action expression failed: {error}"),
                            )),
                        }
                    }
                    CompiledAction::PopValue => {
                        if end_state.values.pop().is_none() {
                            diagnostics.push(diagnostic(
                                &snapshot,
                                start_pos..end_pos,
                                "lex.value.underflow",
                                "`pop_value` on an empty value stack",
                            ));
                        }
                    }
                    CompiledAction::Error(message) => {
                        error_message = Some(Arc::clone(message));
                    }
                }
            }

            if rule.error {
                diagnostics.push(
                    diagnostic(
                        &snapshot,
                        start_pos..end_pos,
                        "lex.error",
                        rule.error_message
                            .as_deref()
                            .unwrap_or("lexical error token"),
                    )
                    .with_token_kind(choice_token_kind(rule)),
                );
            }
            if end_pos == start_pos && end_state == state {
                let (token, diagnostic, next_position) = self.error_token(
                    &snapshot,
                    text,
                    start_pos,
                    &state,
                    TokenId(next_id),
                    "zero-width lexer rule made no progress".to_string(),
                );
                next_id = next_id.saturating_add(1);
                tokens.push(Arc::new(token));
                diagnostics.push(diagnostic);
                position = next_position;
                zero_width_run = 0;
                continue;
            }
            if end_pos == start_pos {
                zero_width_run += 1;
                if zero_width_run > self.options.max_zero_width_run {
                    let (token, diagnostic, next_position) = self.error_token(
                        &snapshot,
                        text,
                        start_pos,
                        &state,
                        TokenId(next_id),
                        "too many consecutive zero-width tokens".to_string(),
                    );
                    next_id = next_id.saturating_add(1);
                    tokens.push(Arc::new(token));
                    diagnostics.push(diagnostic);
                    position = next_position;
                    zero_width_run = 0;
                    continue;
                }
            } else {
                zero_width_run = 0;
            }

            let Some(range) = snapshot.range(start_pos..end_pos) else {
                break;
            };
            let Some(lookahead_end) = snapshot.anchor(choice.matched.examined) else {
                break;
            };
            tokens.push(Arc::new(Token {
                id: TokenId(next_id),
                kind: choice_token_kind(rule),
                text: Arc::from(lexeme),
                range,
                start_state: state.clone(),
                end_state: end_state.clone(),
                lookahead_end,
                flags: token_flags(rule, error_message.is_some()),
                error: error_message,
            }));
            next_id = next_id.saturating_add(1);
            position = end_pos;
            state = end_state;
        }

        if std::env::var_os("REACTIVE_SYNTAX_TRACE").is_some() {
            eprintln!(
                "[syntax] relex full range=0..{} chars={} tokens={}",
                text_len,
                text_len,
                tokens.len()
            );
        }
        let relexed_tokens = 0..tokens.len();
        LexOutput {
            tokens: Arc::new(TokenStream::new(tokens)),
            text: snapshot,
            diagnostics,
            relexed_chars: text_len,
            relexed_tokens,
            replaced_tokens: 0..0,
        }
    }

    fn best_match(
        &self,
        state: &LexConfig,
        text: &str,
        position: usize,
    ) -> (Option<Choice>, Option<super::ExprError>) {
        let mut best: Option<Choice> = None;
        let mut first_guard_error = None;
        let rules = self.spec.rules(state.mode);
        for (index, rule) in rules.iter().enumerate() {
            if let Some(byte) = text.as_bytes().get(position) {
                if !rule.pattern.can_start_with_byte(*byte) {
                    continue;
                }
            }
            let matches = rule.pattern.find_at(text, position);
            for matched in matches {
                let captures = relative_captures(&matched.captures, position);
                if let Some(guard) = &rule.guard {
                    let cx = eval_context(
                        state,
                        &text[position..matched.end],
                        rule,
                        &captures,
                        position == text.len(),
                    );
                    match guard.eval_bool(&cx) {
                        Ok(true) => {}
                        Ok(false) => continue,
                        Err(error) => {
                            first_guard_error.get_or_insert(error);
                            continue;
                        }
                    }
                }
                let candidate = Choice {
                    rule: index,
                    matched,
                };
                let better = match &best {
                    None => true,
                    Some(current) => {
                        let candidate_rule = &rules[candidate.rule];
                        let current_rule = &rules[current.rule];
                        candidate.matched.end > current.matched.end
                            || (candidate.matched.end == current.matched.end
                                && (candidate_rule.priority > current_rule.priority
                                    || (candidate_rule.priority == current_rule.priority
                                        && candidate.rule < current.rule)))
                    }
                };
                if better {
                    best = Some(candidate);
                }
                break;
            }
        }
        (best, first_guard_error)
    }

    fn column_after(&self, column: u32, lexeme: &str) -> u32 {
        if self.spec.uses_column {
            advance_column(column, lexeme)
        } else {
            0
        }
    }

    fn error_token(
        &self,
        snapshot: &TextSnapshot,
        text: &str,
        position: usize,
        state: &LexConfig,
        id: TokenId,
        message: String,
    ) -> (Token, Diagnostic, usize) {
        let end = text[position..]
            .chars()
            .next()
            .map_or(position, |c| position + c.len_utf8());
        let lexeme = &text[position..end];
        let end_state = LexConfig {
            mode: state.mode,
            mode_stack: state.mode_stack.clone(),
            values: state.values.clone(),
            column: self.column_after(state.column, lexeme),
        };
        let range = snapshot
            .range(position..end)
            .unwrap_or_else(|| TextRange::empty(snapshot.anchor(position).expect("position")));
        let lookahead_end = snapshot.anchor(end).unwrap_or(range.end);
        let token = Token {
            id,
            kind: self.spec.error_kind,
            text: Arc::from(lexeme),
            range,
            start_state: state.clone(),
            end_state,
            lookahead_end,
            flags: TokenFlags::ERROR,
            error: Some(Arc::from(message.clone())),
        };
        let diagnostic = diagnostic(snapshot, position..end, "lex.error", message)
            .with_token_kind(self.spec.error_kind);
        (token, diagnostic, end)
    }
}

fn diagnostic(
    snapshot: &TextSnapshot,
    range: std::ops::Range<usize>,
    code: &str,
    message: impl Into<String>,
) -> Diagnostic {
    let range = snapshot
        .range(range.clone())
        .or_else(|| snapshot.range(range.start..range.start))
        .unwrap_or_else(|| {
            TextRange::empty(
                snapshot
                    .anchor(0)
                    .unwrap_or(crate::text::TextAnchor::new(snapshot.version(), 0)),
            )
        });
    Diagnostic::error(code, message, range)
}

struct Choice {
    rule: usize,
    matched: super::PatternMatch,
}

fn eval_context<'a>(
    state: &'a LexConfig,
    lexeme: &'a str,
    rule: &'a Rule,
    captures: &'a [Option<(usize, usize)>],
    eof: bool,
) -> EvalContext<'a> {
    EvalContext {
        column: state.column,
        text: lexeme,
        lexeme,
        capture_names: rule.pattern.capture_names(),
        capture_ranges: captures,
        values: &state.values,
        eof,
    }
}

fn relative_captures(
    captures: &[Option<(usize, usize)>],
    start: usize,
) -> Vec<Option<(usize, usize)>> {
    captures
        .iter()
        .map(|range| range.map(|(from, to)| (from.saturating_sub(start), to.saturating_sub(start))))
        .collect()
}

fn choice_token_kind(rule: &Rule) -> TokenKindId {
    match rule.kind {
        RuleKind::Token(kind) | RuleKind::Skip(kind) => kind,
    }
}

fn token_flags(rule: &Rule, has_error: bool) -> TokenFlags {
    let mut flags = if rule.extra || matches!(rule.kind, RuleKind::Skip(_)) {
        TokenFlags::TRIVIA
    } else {
        TokenFlags::default()
    };
    if has_error || rule.error {
        flags |= TokenFlags::ERROR;
    }
    flags
}

fn advance_column(column: u32, lexeme: &str) -> u32 {
    let mut column = column;
    for c in lexeme.chars() {
        if c == '\n' {
            column = 0;
        } else {
            column = column.saturating_add(1);
        }
    }
    column
}
