//! Lexer component: turns source text into the `Tokens` View.
//!
//! The lexer is incremental already (it relexes the edit window and reuses the
//! suffix), so this component only has to publish a delta: the new order, the
//! payloads of the tokens that changed, and retractions for the tokens that
//! disappeared.

use std::collections::HashSet;
use std::sync::Arc;

use irrigation::{
    ChangeBatch, ComponentId, DependencyUpdates, GeneralComponent, GeneralCx, ListDelta, ListEdit,
    Result, ViewDescriptor, ViewSchema,
};

use crate::lexer::{LexOutput, LexState, Lexer, Token, TokenFlags, TokenId};
use crate::text::TextDocument;
use crate::views::{DiagnosticId, SourceFile, Text, TokenDelta, TokenPayload, Tokens};

/// Retained lexer state.
#[derive(Clone, Debug)]
pub struct LexerComponentState {
    lex: LexState,
    ids: HashSet<TokenId>,
    changed: HashSet<TokenId>,
    diagnostic_ids: HashSet<DiagnosticId>,
    version: u64,
}

/// Lexer component: reads source text, owns the `Tokens` View.
#[derive(Clone, Debug)]
pub struct LexerComponent {
    lexer: Lexer,
}

impl LexerComponent {
    /// Builds a lexer component from a lexer specification.
    #[must_use]
    pub fn new(spec: crate::LexerSpec) -> Self {
        Self {
            lexer: Lexer::new(spec),
        }
    }

    fn read_source(cx: &mut GeneralCx) -> Result<TextDocument> {
        Text::DOCUMENT.read_in(cx)
    }

    /// Publishes one lexer output as a delta against `previous`.
    #[allow(clippy::too_many_arguments)]
    fn publish(
        cx: &mut GeneralCx,
        document: &TextDocument,
        lexed: &LexOutput,
        previous_ids: &HashSet<TokenId>,
        previous_diagnostics: &HashSet<DiagnosticId>,
        previous_changed: &HashSet<TokenId>,
        delta: &TokenDelta,
        full: bool,
    ) {
        cx.write(Tokens::ORDER)
            .define_keyed(lexed.tokens.iter().map(|token| (token.id.0, token.id)));
        let changed = delta.changed_set();
        let current: HashSet<TokenId> = lexed.tokens.iter().map(|token| token.id).collect();
        if current.len() != lexed.tokens.len() {
            let mut seen = HashSet::new();
            let duplicates: Vec<u64> = lexed
                .tokens
                .iter()
                .map(|token| token.id.0)
                .filter(|id| !seen.insert(*id))
                .collect();
            eprintln!("[lex] duplicate token ids: {duplicates:?}");
        }
        cx.write(Tokens::TEXT).set(document.clone());
        cx.write(Tokens::DELTA).set(delta.clone());

        // Payloads: only the tokens this revision rebuilt, or all of them on the
        // first parse.
        for token in lexed.tokens.iter() {
            if !full && !changed.contains(&token.id) {
                continue;
            }
            cx.write(Tokens::PAYLOADS)
                .entry(token.id)
                .set(Arc::new(TokenPayload {
                    kind: token.kind,
                    text: Arc::clone(&token.text),
                    range: token.range,
                    flags: token.flags,
                    error: token.error.clone(),
                }));
            cx.write(Tokens::CHANGED).member(token.id).present(true);
            cx.write(Tokens::TRIVIA)
                .member(token.id)
                .present(token.flags.contains(TokenFlags::TRIVIA));
            cx.write(Tokens::ERRORS)
                .member(token.id)
                .present(token.flags.contains(TokenFlags::ERROR));
        }
        // Diagnostics: publish current ones, retract vanished ones.
        for diagnostic in &lexed.diagnostics {
            let id = DiagnosticId::from_diagnostic(diagnostic);
            cx.write(Tokens::DIAGNOSTICS)
                .entry(id)
                .set(diagnostic.clone());
        }
        if !full {
            let current_diagnostics: HashSet<DiagnosticId> = lexed
                .diagnostics
                .iter()
                .map(DiagnosticId::from_diagnostic)
                .collect();
            for id in previous_diagnostics {
                if !current_diagnostics.contains(id) {
                    cx.write(Tokens::DIAGNOSTICS).entry(id.clone()).retract();
                }
            }
            for id in previous_changed {
                if !changed.contains(id) {
                    cx.write(Tokens::CHANGED).member(*id).present(false);
                }
            }
            for id in previous_ids {
                if !current.contains(id) {
                    cx.write(Tokens::PAYLOADS).entry(*id).retract();
                    cx.write(Tokens::CHANGED).member(*id).present(false);
                    cx.write(Tokens::TRIVIA).member(*id).present(false);
                    cx.write(Tokens::ERRORS).member(*id).present(false);
                }
            }
        }
    }

    /// Builds the token delta of one revision from the identity diff.
    ///
    /// The lexer reconciles identities, so a token that still exists keeps its
    /// id. The delta therefore only names the tokens whose *payload* changed and
    /// the identities that appeared or disappeared; a consumer can rebuild the
    /// order by applying the id script, which is empty when nothing moved.
    fn delta(
        old: &[Arc<Token>],
        new: &[Arc<Token>],
        lexed: &LexOutput,
        from: u64,
        to: u64,
    ) -> TokenDelta {
        let mut prefix = 0usize;
        while prefix < old.len() && prefix < new.len() && old[prefix].id == new[prefix].id {
            prefix += 1;
        }
        let mut suffix = 0usize;
        while suffix < old.len().saturating_sub(prefix)
            && suffix < new.len().saturating_sub(prefix)
            && old[old.len() - 1 - suffix].id == new[new.len() - 1 - suffix].id
        {
            suffix += 1;
        }
        let old_middle = prefix..old.len() - suffix;
        let new_middle = prefix..new.len() - suffix;
        let mut edits = Vec::with_capacity(old_middle.len() + new_middle.len());
        for _ in old_middle {
            edits.push(ListEdit::Remove { index: prefix });
        }
        for (offset, token) in new[new_middle].iter().enumerate() {
            edits.push(ListEdit::Insert {
                index: prefix + offset,
                value: token.id,
            });
        }
        let old_by_id: std::collections::HashMap<TokenId, &Arc<Token>> =
            old.iter().map(|token| (token.id, token)).collect();
        let mut changed = Vec::new();
        let mut trivia_only = true;
        for token in new {
            let same = old_by_id.get(&token.id).is_some_and(|old| {
                old.kind == token.kind
                    && old.text == token.text
                    && old.flags == token.flags
                    && old.error == token.error
            });
            if same {
                continue;
            }
            changed.push(token.id);
            trivia_only &= token.flags.contains(TokenFlags::TRIVIA);
        }
        let present: std::collections::HashSet<TokenId> =
            new.iter().map(|token| token.id).collect();
        for token in old {
            if !present.contains(&token.id) {
                changed.push(token.id);
                trivia_only &= token.flags.contains(TokenFlags::TRIVIA);
            }
        }
        TokenDelta {
            from,
            to,
            order: ListDelta::from_edits(edits),
            changed,
            diagnostics: lexed.diagnostics.clone(),
            trivia_only,
            bytes_relexed: lexed.relexed_chars,
            tokens_relexed: lexed.relexed_tokens.len(),
        }
    }
}

impl GeneralComponent for LexerComponent {
    type State = LexerComponentState;
    type Output = ();

    const ID: ComponentId = ComponentId(0x00B2_0001);
    const NAME: &'static str = "irrigation-syntax::LexerComponent";
    const VIEWS: &'static [&'static ViewDescriptor] = &[<SourceFile as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static ViewDescriptor] = &[<Tokens as ViewSchema>::DESCRIPTOR];

    fn dependency_updates(&self) -> DependencyUpdates {
        DependencyUpdates::Incremental
    }

    fn initialize(&self, cx: &mut GeneralCx) -> Result<(Self::State, Self::Output)> {
        let document = Self::read_source(cx)?;
        let lexed = self.lexer.lex(&document);
        let version = document.snapshot().version().0;
        let delta = Self::delta(&[], lexed.tokens.as_slice(), &lexed, version, version);
        Self::publish(
            cx,
            &document,
            &lexed,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &delta,
            true,
        );
        Ok((
            LexerComponentState {
                lex: LexState::from_output(&lexed),
                ids: lexed.tokens.iter().map(|token| token.id).collect(),
                changed: delta.changed_set(),
                diagnostic_ids: lexed
                    .diagnostics
                    .iter()
                    .map(DiagnosticId::from_diagnostic)
                    .collect(),
                version,
            },
            (),
        ))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        state: &Self::State,
        _changes: &ChangeBatch,
    ) -> Result<(Self::State, Self::Output)> {
        let document = Self::read_source(cx)?;
        let lexed = self.lexer.lex_incremental(&state.lex, &document);
        let version = document.snapshot().version().0;
        let delta = Self::delta(
            state.lex.tokens.as_slice(),
            lexed.tokens.as_slice(),
            &lexed,
            state.version,
            version,
        );
        Self::publish(
            cx,
            &document,
            &lexed,
            &state.ids,
            &state.diagnostic_ids,
            &state.changed,
            &delta,
            false,
        );
        let mut ids = state.ids.clone();
        for id in &delta.changed {
            ids.insert(*id);
        }
        let present: HashSet<TokenId> = lexed.tokens.iter().map(|token| token.id).collect();
        ids.retain(|id| present.contains(id));
        Ok((
            LexerComponentState {
                lex: LexState::from_output(&lexed),
                ids,
                changed: delta.changed_set(),
                diagnostic_ids: lexed
                    .diagnostics
                    .iter()
                    .map(DiagnosticId::from_diagnostic)
                    .collect(),
                version,
            },
            (),
        ))
    }
}
