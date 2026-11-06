//! Text component: turns one source document into the `Text` View.

use std::sync::Arc;

use reactive::{
    ChangeBatch, ComponentId, DependencyUpdates, FieldId, GeneralComponent, GeneralCx, Result,
    UnitKey, UnitKind, ViewDescriptor, ViewSchema,
};

use crate::text::TextDocument;
use crate::views::{SourceFile, Text};

/// Reads `SourceFile`, owns `Text`.
#[derive(Clone, Debug)]
pub struct TextComponent;

impl TextComponent {
    fn read_source(cx: &mut GeneralCx) -> Result<TextDocument> {
        cx.read_typed(UnitKey::field(
            <SourceFile as ViewSchema>::view_id(),
            FieldId(0),
            UnitKind::Value,
        ))
    }

    fn publish(cx: &mut GeneralCx, document: &TextDocument, from: usize, to: usize) {
        let version = document.snapshot().version().0;
        let mut output = cx.output();
        output.field(Text::view_id(), FieldId(0), document.clone());
        output.field(Text::view_id(), FieldId(1), version);
        for (index, edit) in document.snapshot().edits().iter().enumerate() {
            let _ = (from, to);
            output.entry(Text::view_id(), FieldId(2), index as u64, edit.clone());
            output.membership(Text::view_id(), FieldId(2), index as u64, true);
        }
    }
}

impl GeneralComponent for TextComponent {
    type State = TextDocument;
    type Output = ();

    const ID: ComponentId = ComponentId(0x00B2_0000);
    const NAME: &'static str = "reactive-syntax::TextComponent";
    const VIEWS: &'static [&'static ViewDescriptor] = &[<SourceFile as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static ViewDescriptor] = &[<Text as ViewSchema>::DESCRIPTOR];

    fn dependency_updates(&self) -> DependencyUpdates {
        DependencyUpdates::Incremental
    }

    fn initialize(&self, cx: &mut GeneralCx) -> Result<(Self::State, Self::Output)> {
        let document = Self::read_source(cx)?;
        Self::publish(cx, &document, 0, 0);
        Ok((document, ()))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        state: &Self::State,
        _changes: &ChangeBatch,
    ) -> Result<(Self::State, Self::Output)> {
        let document = Self::read_source(cx)?;
        let from_version = state.snapshot().version();
        let to_version = document.snapshot().version();
        let edits = if from_version == to_version {
            if state.text() == document.text() {
                Vec::new()
            } else {
                vec![crate::TextEdit::new(
                    0..state.text().len(),
                    Arc::<str>::from(document.text()),
                )]
            }
        } else {
            document
                .snapshot()
                .edits_since(from_version)
                .unwrap_or_else(|| {
                    vec![crate::TextEdit::new(
                        0..state.text().len(),
                        Arc::<str>::from(document.text()),
                    )]
                })
        };
        let mut output = cx.output();
        output.field(Text::view_id(), FieldId(0), document.clone());
        output.field(Text::view_id(), FieldId(1), to_version.0);
        for (index, edit) in edits.iter().enumerate() {
            output.entry(Text::view_id(), FieldId(2), index as u64, edit.clone());
            output.membership(Text::view_id(), FieldId(2), index as u64, true);
        }
        for index in edits.len()..state.snapshot().edits().len() {
            output.membership(Text::view_id(), FieldId(2), index as u64, false);
        }
        Ok((document, ()))
    }
}
