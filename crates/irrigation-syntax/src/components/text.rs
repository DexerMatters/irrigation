//! Text component: turns one source document into the `Text` View.

use std::sync::Arc;

use irrigation::{
    ChangeBatch, ComponentId, DependencyUpdates, GeneralComponent, GeneralCx, Result,
    ViewDescriptor, ViewSchema,
};

use crate::text::TextDocument;
use crate::views::{SourceFile, Text};

/// Reads `SourceFile`, owns `Text`.
#[derive(Clone, Debug)]
pub struct TextComponent;

impl TextComponent {
    fn read_source(cx: &mut GeneralCx) -> Result<TextDocument> {
        SourceFile::DOCUMENT.read_in(cx)
    }

    fn publish(cx: &mut GeneralCx, document: &TextDocument, from: usize, to: usize) {
        let version = document.snapshot().version().0;
        let _ = (from, to);
        cx.write(Text::DOCUMENT).set(document.clone());
        cx.write(Text::VERSION).set(version);
        for (index, edit) in document.snapshot().edits().iter().enumerate() {
            cx.write(Text::EDITS).entry(index as u64).set(edit.clone());
        }
    }
}

impl GeneralComponent for TextComponent {
    type State = TextDocument;
    type Output = ();

    const ID: ComponentId = ComponentId(0x00B2_0000);
    const NAME: &'static str = "irrigation-syntax::TextComponent";
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
        cx.write(Text::DOCUMENT).set(document.clone());
        cx.write(Text::VERSION).set(to_version.0);
        for (index, edit) in edits.iter().enumerate() {
            cx.write(Text::EDITS).entry(index as u64).set(edit.clone());
        }
        for index in edits.len()..state.snapshot().edits().len() {
            cx.write(Text::EDITS).entry(index as u64).present(false);
        }
        Ok((document, ()))
    }
}
