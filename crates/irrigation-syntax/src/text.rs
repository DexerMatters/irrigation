//! Immutable text snapshots with versioned anchors and an edit map.
//!
//! A [`TextSnapshot`] owns the current text and a parent link. Every edit
//! batch records the edits that transform the parent into the snapshot.
//! Anchors are `(version, byte offset)` pairs, so an edit before a node shifts
//! its resolved offset without rewriting the node. An anchor strictly inside a
//! replaced range does not resolve; an anchor at a boundary resolves to the
//! corresponding new boundary.

use std::fmt;
use std::ops::Range;
use std::sync::Arc;

/// Monotonic version of a text snapshot.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TextVersion(pub u64);

impl fmt::Debug for TextVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}", self.0)
    }
}

/// A stable position in a particular text version.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TextAnchor {
    /// The version the offset belongs to.
    pub version: TextVersion,
    /// Byte offset in that version.
    pub offset: usize,
}

impl fmt::Debug for TextAnchor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@v{}", self.offset, self.version.0)
    }
}

impl TextAnchor {
    /// Builds an anchor.
    #[must_use]
    pub const fn new(version: TextVersion, offset: usize) -> Self {
        Self { version, offset }
    }
}

/// A range between two versioned anchors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextRange {
    /// Start anchor (inclusive).
    pub start: TextAnchor,
    /// End anchor (exclusive).
    pub end: TextAnchor,
}

impl TextRange {
    /// Builds a range.
    #[must_use]
    pub const fn new(start: TextAnchor, end: TextAnchor) -> Self {
        Self { start, end }
    }

    /// Builds an empty range at one anchor.
    #[must_use]
    pub const fn empty(anchor: TextAnchor) -> Self {
        Self {
            start: anchor,
            end: anchor,
        }
    }

    /// Returns `true` when the range is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// Resolves both anchors against a snapshot.
    #[must_use]
    pub fn resolve(&self, snapshot: &TextSnapshot) -> Option<Range<usize>> {
        let start = snapshot.resolve(self.start)?;
        let end = snapshot.resolve(self.end)?;
        (start <= end).then_some(start..end)
    }
}

/// One replacement in the coordinate space of the snapshot being edited.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    /// Replaced byte range.
    pub range: Range<usize>,
    /// Replacement text.
    pub replacement: Arc<str>,
}

impl TextEdit {
    /// Builds an edit.
    #[must_use]
    pub fn new(range: Range<usize>, replacement: impl Into<Arc<str>>) -> Self {
        Self {
            range,
            replacement: replacement.into(),
        }
    }

    /// Builds an insertion at `offset`.
    #[must_use]
    pub fn insert(offset: usize, text: impl Into<Arc<str>>) -> Self {
        Self::new(offset..offset, text)
    }

    /// Builds a deletion.
    #[must_use]
    pub fn delete(range: Range<usize>) -> Self {
        Self::new(range, "")
    }

    /// The old length.
    #[must_use]
    pub fn old_len(&self) -> usize {
        self.range.end.saturating_sub(self.range.start)
    }

    /// The new length.
    #[must_use]
    pub fn new_len(&self) -> usize {
        self.replacement.len()
    }
}

/// Failure while applying a set of edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextEditError {
    /// An edit range exceeds the current text.
    OutOfBounds {
        /// The invalid edit.
        edit: TextEdit,
        /// The current text length.
        len: usize,
    },
    /// Two edits overlap.
    Overlap {
        /// The earlier edit.
        left: TextEdit,
        /// The later edit.
        right: TextEdit,
    },
}

impl fmt::Display for TextEditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfBounds { edit, len } => {
                write!(f, "edit {:?} is outside text of length {len}", edit.range)
            }
            Self::Overlap { left, right } => {
                write!(f, "edits {:?} and {:?} overlap", left.range, right.range)
            }
        }
    }
}

impl std::error::Error for TextEditError {}

/// Immutable text plus its edit chain.
#[derive(Clone)]
pub struct TextSnapshot {
    version: TextVersion,
    text: Arc<str>,
    parent: Option<Arc<TextSnapshot>>,
    edits: Arc<[TextEdit]>,
}

impl fmt::Debug for TextSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The text is the value a reader wants; version and edit ancestry are
        // bookkeeping, not the document.
        f.write_str(&self.text)
    }
}

impl PartialEq for TextSnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.version == other.version && Arc::ptr_eq(&self.text, &other.text)
    }
}

impl Eq for TextSnapshot {}

impl TextSnapshot {
    /// Creates the initial snapshot at version zero.
    #[must_use]
    pub fn new(text: impl Into<Arc<str>>) -> Self {
        Self {
            version: TextVersion(0),
            text: text.into(),
            parent: None,
            edits: Arc::from(Vec::new()),
        }
    }

    /// The snapshot version.
    #[must_use]
    pub fn version(&self) -> TextVersion {
        self.version
    }

    /// The current text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Byte length.
    #[must_use]
    pub fn len(&self) -> usize {
        self.text.len()
    }

    /// Returns `true` for empty text.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The edits that produced this snapshot from its parent.
    #[must_use]
    pub fn edits(&self) -> &[TextEdit] {
        &self.edits
    }

    /// The parent snapshot, if any.
    #[must_use]
    pub fn parent(&self) -> Option<&Arc<TextSnapshot>> {
        self.parent.as_ref()
    }

    /// Builds an anchor at `offset` in this version.
    #[must_use]
    pub fn anchor(&self, offset: usize) -> Option<TextAnchor> {
        (offset <= self.len()).then_some(TextAnchor::new(self.version, offset))
    }

    /// Builds a range in this version, rejecting out-of-bounds offsets.
    #[must_use]
    pub fn range(&self, range: Range<usize>) -> Option<TextRange> {
        if range.start > range.end || range.end > self.len() {
            return None;
        }
        Some(TextRange::new(
            TextAnchor::new(self.version, range.start),
            TextAnchor::new(self.version, range.end),
        ))
    }

    /// Resolves an anchor into this version's byte offset.
    #[must_use]
    pub fn resolve(&self, anchor: TextAnchor) -> Option<usize> {
        if anchor.version == self.version {
            return (anchor.offset <= self.len()).then_some(anchor.offset);
        }
        if anchor.version > self.version {
            return None;
        }

        let mut chain: Vec<&TextSnapshot> = Vec::new();
        let mut cursor = self;
        loop {
            if cursor.version == anchor.version {
                break;
            }
            let parent = cursor.parent.as_deref()?;
            chain.push(cursor);
            cursor = parent;
        }

        let mut offset = anchor.offset;
        if offset > cursor.len() {
            return None;
        }
        for snapshot in chain.iter().rev() {
            offset = snapshot.apply_edits_to_offset(offset)?;
        }
        (offset <= self.len()).then_some(offset)
    }

    /// Resolves a range into this version's byte range.
    #[must_use]
    pub fn resolve_range(&self, range: TextRange) -> Option<Range<usize>> {
        range.resolve(self)
    }

    /// Finds the snapshot for `version` in this snapshot's ancestry.
    #[must_use]
    pub fn find_version(&self, version: TextVersion) -> Option<Arc<TextSnapshot>> {
        let mut cursor = self;
        loop {
            if cursor.version == version {
                return Some(Arc::new(cursor.clone()));
            }
            cursor = cursor.parent.as_deref()?;
        }
    }

    /// Returns the edits between an ancestor version and this snapshot.
    ///
    /// A direct parent-child relationship returns the exact edit batch. A
    /// longer chain returns a single coarse replacement computed from the
    /// common prefix and suffix. The coarse fallback is correct but can relex
    /// a larger window; it is the documented fallback when exact composition
    /// would cost more than the relex.
    #[must_use]
    pub fn edits_since(&self, version: TextVersion) -> Option<Vec<TextEdit>> {
        if version == self.version {
            return Some(Vec::new());
        }
        if let Some(parent) = &self.parent {
            if parent.version == version {
                return Some(self.edits.to_vec());
            }
        }

        let old = self.find_version(version)?;
        let old_text = old.text();
        let new_text = self.text();
        let old_bytes = old_text.as_bytes();
        let new_bytes = new_text.as_bytes();
        let mut prefix = 0;
        while prefix < old_bytes.len()
            && prefix < new_bytes.len()
            && old_bytes[prefix] == new_bytes[prefix]
        {
            prefix += 1;
        }
        let mut suffix = 0;
        while suffix < old_bytes.len().saturating_sub(prefix)
            && suffix < new_bytes.len().saturating_sub(prefix)
            && old_bytes[old_bytes.len() - 1 - suffix] == new_bytes[new_bytes.len() - 1 - suffix]
        {
            suffix += 1;
        }
        let prefix = floor_boundary(old_text, prefix).min(floor_boundary(new_text, prefix));
        let old_end = ceil_boundary(old_text, old_bytes.len() - suffix).max(prefix);
        let new_end = ceil_boundary(new_text, new_bytes.len() - suffix).max(prefix);
        Some(vec![TextEdit::new(
            prefix..old_end,
            Arc::<str>::from(&new_text[prefix..new_end]),
        )])
    }

    fn apply_edits_to_offset(&self, offset: usize) -> Option<usize> {
        let mut delta: isize = 0;
        for edit in self.edits.iter() {
            let start = edit.range.start;
            let end = edit.range.end;
            if offset < start {
                return usize::try_from(offset as isize + delta).ok();
            }
            if offset == start {
                return usize::try_from(start as isize + delta).ok();
            }
            if offset < end {
                return None;
            }
            if offset == end {
                return usize::try_from(start as isize + delta + edit.new_len() as isize).ok();
            }
            delta += edit.new_len() as isize - edit.old_len() as isize;
        }
        usize::try_from(offset as isize + delta).ok()
    }
}

/// A text document: the current snapshot plus the ability to apply edits.
#[derive(Clone, PartialEq, Eq)]
pub struct TextDocument {
    snapshot: Arc<TextSnapshot>,
}

impl fmt::Debug for TextDocument {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.text())
    }
}

impl Default for TextDocument {
    fn default() -> Self {
        Self::new("")
    }
}

impl TextDocument {
    /// Creates a document from text.
    #[must_use]
    pub fn new(text: impl Into<Arc<str>>) -> Self {
        Self {
            snapshot: Arc::new(TextSnapshot::new(text)),
        }
    }

    /// The current snapshot.
    #[must_use]
    pub fn snapshot(&self) -> &Arc<TextSnapshot> {
        &self.snapshot
    }

    /// The current text.
    #[must_use]
    pub fn text(&self) -> &str {
        self.snapshot.text()
    }

    /// Applies a batch of non-overlapping edits in the current coordinate
    /// space. Edits are sorted; applying them in reverse order preserves the
    /// caller's original offsets.
    ///
    /// # Errors
    ///
    /// Fails when an edit is out of bounds or two edits overlap.
    pub fn edit(&self, edits: impl IntoIterator<Item = TextEdit>) -> Result<Self, TextEditError> {
        let mut edits: Vec<TextEdit> = edits.into_iter().collect();
        edits.sort_by_key(|edit| (edit.range.start, edit.range.end));
        for edit in &edits {
            if edit.range.start > edit.range.end || edit.range.end > self.snapshot.len() {
                return Err(TextEditError::OutOfBounds {
                    edit: edit.clone(),
                    len: self.snapshot.len(),
                });
            }
        }
        for pair in edits.windows(2) {
            if pair[0].range.end > pair[1].range.start {
                return Err(TextEditError::Overlap {
                    left: pair[0].clone(),
                    right: pair[1].clone(),
                });
            }
        }
        if edits.is_empty() {
            return Ok(self.clone());
        }

        let mut text = String::from(self.snapshot.text());
        for edit in edits.iter().rev() {
            text.replace_range(edit.range.clone(), &edit.replacement);
        }
        let version = TextVersion(self.snapshot.version.0.wrapping_add(1));
        Ok(Self {
            snapshot: Arc::new(TextSnapshot {
                version,
                text: Arc::from(text),
                parent: Some(Arc::clone(&self.snapshot)),
                edits: edits.into(),
            }),
        })
    }

    /// Convenience for a single replacement.
    ///
    /// # Errors
    ///
    /// Fails when the range is out of bounds.
    pub fn replace(
        &self,
        range: Range<usize>,
        replacement: impl Into<Arc<str>>,
    ) -> Result<Self, TextEditError> {
        self.edit([TextEdit::new(range, replacement)])
    }

    /// Applies a whole new text as the single edit that separates the two.
    ///
    /// An editor sends its entire buffer on every keystroke. `TextDocument::new`
    /// would hand the pipeline a document with no ancestry, and no pipeline can
    /// be local about a document it has never seen; this commits the one
    /// contiguous replacement that turns the committed text into the new text, so
    /// a keystroke stays a keystroke. `None` means the text is unchanged.
    #[must_use]
    pub fn replace_with(&self, text: &str) -> Option<Self> {
        let old = self.text();
        if old == text {
            return None;
        }
        let (old_bytes, new_bytes) = (old.as_bytes(), text.as_bytes());
        let mut prefix = 0;
        while prefix < old_bytes.len()
            && prefix < new_bytes.len()
            && old_bytes[prefix] == new_bytes[prefix]
        {
            prefix += 1;
        }
        let mut suffix = 0;
        while suffix < old_bytes.len() - prefix
            && suffix < new_bytes.len() - prefix
            && old_bytes[old_bytes.len() - 1 - suffix] == new_bytes[new_bytes.len() - 1 - suffix]
        {
            suffix += 1;
        }
        // A byte-wise diff can land inside a UTF-8 sequence; the boundaries move
        // outward until they are characters again.
        let mut start = prefix;
        while start > 0 && !old.is_char_boundary(start) {
            start -= 1;
        }
        let mut end = old_bytes.len() - suffix;
        while end < old_bytes.len() && !old.is_char_boundary(end) {
            end += 1;
        }
        let mut stop = new_bytes.len() - suffix;
        while stop < new_bytes.len() && !text.is_char_boundary(stop) {
            stop += 1;
        }
        self.replace(start..end, &text[start..stop]).ok()
    }

    /// Returns exact edits for a direct parent or a coarse replacement for a
    /// longer ancestry; `None` when `version` is not an ancestor.
    #[must_use]
    pub fn edits_since(&self, version: TextVersion) -> Option<Vec<TextEdit>> {
        self.snapshot.edits_since(version)
    }

    /// The line and column (both zero-based) of a byte offset.
    #[must_use]
    pub fn line_column(&self, offset: usize) -> Option<(usize, usize)> {
        if offset > self.snapshot.len() {
            return None;
        }
        let prefix = &self.snapshot.text()[..offset];
        let line = prefix.bytes().filter(|b| *b == b'\n').count();
        let column = prefix.rfind('\n').map_or_else(
            || prefix.chars().count(),
            |i| prefix[i + 1..].chars().count(),
        );
        Some((line, column))
    }
}

fn floor_boundary(text: &str, mut offset: usize) -> usize {
    offset = offset.min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

fn ceil_boundary(text: &str, mut offset: usize) -> usize {
    offset = offset.min(text.len());
    while offset < text.len() && !text.is_char_boundary(offset) {
        offset += 1;
    }
    offset
}

#[cfg(test)]
#[path = "../tests/unit/text/tests.rs"]
mod tests;
