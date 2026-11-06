//! Lexer component: turns source text into the `Tokens` View.
//!
//! The lexer is incremental already (it relexes the edit window and reuses the
//! suffix), so this component only has to publish a delta: the new order, the
//! payloads of the tokens that changed, and retractions for the tokens that
//! disappeared.

use std::collections::HashSet;
use std::sync::Arc;

use reactive::{
    ChangeBatch, ComponentId, DependencyUpdates, FieldId, GeneralComponent, GeneralCx, ListDelta,
    ListEdit, ListOutputExt, Result, UnitKey, UnitKind, ViewDescriptor, ViewSchema, any_key,
};

use crate::lexer::{LexOutput, LexState, Lexer, Token, TokenFlags, TokenId};
use crate::text::TextDocument;
use crate::views::{DiagnosticId, SourceFile, Text, TokenDelta, TokenPayload, Tokens};

impl Tokens {
    /// Token stream coordinate space.
    #[must_use]
    pub fn text_unit() -> UnitKey {
        UnitKey::field(Self::view_id(), FieldId(0), UnitKind::Value)
    }

    /// Ordered token identities.
    #[must_use]
    pub fn order_unit() -> UnitKey {
        UnitKey::field(Self::view_id(), FieldId(1), UnitKind::Structure)
    }

    /// One ordered entry.
    #[must_use]
    pub fn order_entry(id: u64) -> UnitKey {
        UnitKey::entity(Self::view_id(), FieldId(1), UnitKind::Value, any_key(id))
    }

    /// The latest delta.
    #[must_use]
    pub fn delta_unit() -> UnitKey {
        UnitKey::field(Self::view_id(), FieldId(2), UnitKind::Value)
    }

    /// One diagnostic.
    #[must_use]
    pub fn diagnostic_unit(id: DiagnosticId) -> UnitKey {
        UnitKey::entity(Self::view_id(), FieldId(3), UnitKind::Value, any_key(id))
    }

    /// One diagnostic membership.
    #[must_use]
    pub fn diagnostic_membership(id: DiagnosticId) -> UnitKey {
        UnitKey::entity(
            Self::view_id(),
            FieldId(3),
            UnitKind::Membership,
            any_key(id),
        )
    }

    /// One token's metadata.
    #[must_use]
    pub fn payload_unit(id: TokenId) -> UnitKey {
        UnitKey::entity(Self::view_id(), FieldId(5), UnitKind::Value, any_key(id))
    }

    /// One changed-token membership.
    #[must_use]
    pub fn changed_membership(id: TokenId) -> UnitKey {
        UnitKey::entity(
            Self::view_id(),
            FieldId(6),
            UnitKind::Membership,
            any_key(id),
        )
    }

    /// One trivia membership.
    #[must_use]
    pub fn trivia_membership(id: TokenId) -> UnitKey {
        UnitKey::entity(
            Self::view_id(),
            FieldId(7),
            UnitKind::Membership,
            any_key(id),
        )
    }

    /// One lexical-error membership.
    #[must_use]
    pub fn error_membership(id: TokenId) -> UnitKey {
        UnitKey::entity(
            Self::view_id(),
            FieldId(8),
            UnitKind::Membership,
            any_key(id),
        )
    }
}

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
        cx.read_typed(Text::document_unit())
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
        cx.list_define_keyed(
            <Tokens as ViewSchema>::view_id(),
            FieldId(1),
            lexed.tokens.iter().map(|token| (token.id.0, token.id)),
        );
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
        let mut output = cx.output();
        output.field(
            <Tokens as ViewSchema>::view_id(),
            FieldId(0),
            document.clone(),
        );
        output.field(<Tokens as ViewSchema>::view_id(), FieldId(2), delta.clone());

        // Payloads: only the tokens this revision rebuilt, or all of them on the
        // first parse.
        for token in lexed.tokens.iter() {
            if !full && !changed.contains(&token.id) {
                continue;
            }
            output.entry(
                <Tokens as ViewSchema>::view_id(),
                FieldId(5),
                token.id,
                Arc::new(TokenPayload {
                    kind: token.kind,
                    text: Arc::clone(&token.text),
                    range: token.range,
                    flags: token.flags,
                    error: token.error.clone(),
                }),
            );
            output.membership(
                <Tokens as ViewSchema>::view_id(),
                FieldId(5),
                token.id,
                true,
            );
            output.membership(
                <Tokens as ViewSchema>::view_id(),
                FieldId(6),
                token.id,
                true,
            );
            output.membership(
                <Tokens as ViewSchema>::view_id(),
                FieldId(7),
                token.id,
                token.flags.contains(TokenFlags::TRIVIA),
            );
            output.membership(
                <Tokens as ViewSchema>::view_id(),
                FieldId(8),
                token.id,
                token.flags.contains(TokenFlags::ERROR),
            );
        }

        // Diagnostics: publish current ones, retract vanished ones.
        for diagnostic in &lexed.diagnostics {
            let id = DiagnosticId::from_diagnostic(diagnostic);
            output.entry(
                <Tokens as ViewSchema>::view_id(),
                FieldId(3),
                id.clone(),
                diagnostic.clone(),
            );
            output.membership(
                <Tokens as ViewSchema>::view_id(),
                FieldId(3),
                id.clone(),
                true,
            );
        }
        if !full {
            let current_diagnostics: HashSet<DiagnosticId> = lexed
                .diagnostics
                .iter()
                .map(DiagnosticId::from_diagnostic)
                .collect();
            for id in previous_diagnostics {
                if !current_diagnostics.contains(id) {
                    output.retract(Tokens::diagnostic_unit(id.clone()));
                    output.retract(Tokens::diagnostic_membership(id.clone()));
                }
            }
            for id in previous_changed {
                if !changed.contains(id) {
                    output.membership(<Tokens as ViewSchema>::view_id(), FieldId(6), *id, false);
                }
            }
            for id in previous_ids {
                if !current.contains(id) {
                    output.retract(Tokens::payload_unit(*id));
                    output.retract(Tokens::changed_membership(*id));
                    output.retract(Tokens::trivia_membership(*id));
                    output.retract(Tokens::error_membership(*id));
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
        for _ in old_middle.clone() {
            edits.push(ListEdit::Remove { index: prefix });
        }
        for (offset, token) in new[new_middle.clone()].iter().enumerate() {
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
    const NAME: &'static str = "reactive-syntax::LexerComponent";
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
                lex: LexState::new(Arc::clone(&lexed.tokens), Arc::clone(&lexed.text)),
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
                lex: LexState::new(Arc::clone(&lexed.tokens), Arc::clone(&lexed.text)),
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
