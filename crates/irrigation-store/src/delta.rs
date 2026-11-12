//! Change contracts for the built-in View families.
//!
//! Each delta is an ordered edit script. Composition concatenates scripts;
//! normalization is done by the runtime at the transaction boundary by applying
//! the script and comparing final values, so an edit followed by its reversal
//! produces no observable change.

use irrigation_core::change::Delta;

use crate::persistent::{PMap, PSeq, PSet};

/// The units a field delta actually moved.
///
/// A delta application reports the keys it changed or removed, not a whole
/// content comparison. `content_changed` is true when at least one payload or
/// membership entry moved; structure-only maintenance is reported separately by
/// the family that owns it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AppliedDelta<K> {
    /// Keys whose payload was inserted or changed.
    pub changed: Vec<K>,
    /// Keys removed from the field.
    pub removed: Vec<K>,
    /// Whether any payload/membership content moved.
    pub content_changed: bool,
}

impl<K> AppliedDelta<K> {
    /// An application that changed nothing.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            changed: Vec::new(),
            removed: Vec::new(),
            content_changed: false,
        }
    }

    /// Whether no key moved.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.removed.is_empty()
    }

    /// Iterates changed units in application order, then removals.
    pub fn units(&self) -> impl Iterator<Item = UnitChange<&K>> {
        self.changed
            .iter()
            .map(UnitChange::Changed)
            .chain(self.removed.iter().map(UnitChange::Removed))
    }

    /// Consumes the report into owned unit changes.
    pub fn into_units(self) -> impl Iterator<Item = UnitChange<K>> {
        self.changed
            .into_iter()
            .map(UnitChange::Changed)
            .chain(self.removed.into_iter().map(UnitChange::Removed))
    }
}

/// One moved unit reported by a field delta application.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnitChange<K> {
    /// The key's payload was inserted or changed.
    Changed(K),
    /// The key was removed.
    Removed(K),
}

/// A typed edit script that can be applied to a field target.
///
/// This is the public `FieldDelta` name for the existing [`Delta`] contract;
/// family stores name their concrete delta types as associated types.
pub trait FieldDelta: Delta {}

impl<T: Delta> FieldDelta for T {}

/// A persistent value that can apply one typed delta and report what moved.
///
/// This is the first kernel shape of the plan’s `FieldStore`: the delta is a
/// typed edit script, the result names the keys that changed or disappeared,
/// and no whole-value comparison is needed to decide whether content moved.
pub trait DeltaStore: Clone {
    /// Identity of one changed or removed entry.
    type Key;
    /// The typed edit script.
    type Delta;

    /// Applies the delta, returning the new store and the exact key changes.
    fn apply_delta(&self, delta: &Self::Delta) -> (Self, AppliedDelta<Self::Key>);
}

/// The field-store contract: a persistent field value that applies one typed
/// delta and reports the exact moved keys.
///
/// `DeltaStore` is the implementation trait; `FieldStore` is the public kernel
/// name used by the plan. The blanket implementation keeps one method surface.
pub trait FieldStore: DeltaStore {}

impl<T: DeltaStore> FieldStore for T {}

impl<K, V> DeltaStore for PMap<K, V>
where
    K: Ord + Clone + Send + Sync + 'static,
    V: Clone + PartialEq + Send + Sync + 'static,
{
    type Key = K;
    type Delta = MapDelta<K, V>;

    fn apply_delta(&self, delta: &Self::Delta) -> (Self, AppliedDelta<K>) {
        let mut next = self.clone();
        let mut applied = AppliedDelta::empty();
        for edit in delta.edits() {
            match edit {
                MapEdit::Insert { key, value } | MapEdit::Update { key, value } => {
                    let (storage, previous) = next.insert(key.clone(), value.clone());
                    next = storage;
                    if previous.as_ref() != Some(value) {
                        applied.changed.push(key.clone());
                        applied.content_changed = true;
                    }
                }
                MapEdit::Remove { key } => {
                    let (storage, previous) = next.remove(key);
                    next = storage;
                    if previous.is_some() {
                        applied.removed.push(key.clone());
                        applied.content_changed = true;
                    }
                }
            }
        }
        (next, applied)
    }
}

impl<T> DeltaStore for PSet<T>
where
    T: Ord + Clone + Send + Sync + 'static,
{
    type Key = T;
    type Delta = SetDelta<T>;

    fn apply_delta(&self, delta: &Self::Delta) -> (Self, AppliedDelta<T>) {
        let mut next = self.clone();
        let mut applied = AppliedDelta::empty();
        for edit in delta.edits() {
            match edit {
                SetEdit::Insert { value } => {
                    let (storage, inserted) = next.insert(value.clone());
                    next = storage;
                    if inserted {
                        applied.changed.push(value.clone());
                        applied.content_changed = true;
                    }
                }
                SetEdit::Remove { value } => {
                    let (storage, removed) = next.remove(value);
                    next = storage;
                    if removed {
                        applied.removed.push(value.clone());
                        applied.content_changed = true;
                    }
                }
            }
        }
        (next, applied)
    }
}

/// One structural edit to a sequence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListEdit<T> {
    /// Insert a value at a position.
    Insert {
        /// Destination position.
        index: usize,
        /// Inserted value.
        value: T,
    },
    /// Remove the value at a position.
    Remove {
        /// Position to remove.
        index: usize,
    },
    /// Replace the value at a position.
    Replace {
        /// Position to replace.
        index: usize,
        /// New value.
        value: T,
    },
    /// Move a value between positions, preserving its identity.
    Move {
        /// Source position.
        from: usize,
        /// Destination position.
        to: usize,
    },
}

/// An ordered script of sequence edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListDelta<T> {
    edits: Vec<ListEdit<T>>,
}

impl<T> Default for ListDelta<T> {
    fn default() -> Self {
        Self { edits: Vec::new() }
    }
}

impl<T> ListDelta<T> {
    /// The empty script.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a script from edits.
    #[must_use]
    pub fn from_edits(edits: Vec<ListEdit<T>>) -> Self {
        Self { edits }
    }

    /// The edits in application order.
    #[must_use]
    pub fn edits(&self) -> &[ListEdit<T>] {
        &self.edits
    }

    /// Appends an edit.
    pub fn push(&mut self, edit: ListEdit<T>) {
        self.edits.push(edit);
    }
}

impl<T: Clone + Send + Sync + 'static> Delta for ListDelta<T> {
    type Target = PSeq<T>;

    fn empty() -> Self {
        Self::new()
    }

    fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }

    fn apply(&self, target: &Self::Target) -> Self::Target {
        let mut seq = target.clone();
        for edit in &self.edits {
            seq = match edit {
                ListEdit::Insert { index, value } => seq.insert(*index, value.clone()),
                ListEdit::Remove { index } => seq.remove(*index).0,
                ListEdit::Replace { index, value } => seq.set(*index, value.clone()),
                ListEdit::Move { from, to } => seq.move_element(*from, *to),
            };
        }
        seq
    }

    fn compose(&self, later: &Self) -> Self {
        let mut edits = self.edits.clone();
        edits.extend(later.edits.iter().cloned());
        Self { edits }
    }
}

/// One positional edit to a reduced sequence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReducedEdit<T> {
    /// Replace one item.
    Set {
        /// Position.
        index: usize,
        /// New item.
        item: T,
    },
    /// Insert one item.
    Insert {
        /// Position.
        index: usize,
        /// New item.
        item: T,
    },
    /// Remove one item.
    Remove {
        /// Position.
        index: usize,
    },
}

/// An ordered script of positional edits to a reduced sequence.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ReducedDelta<T> {
    edits: Vec<ReducedEdit<T>>,
}

impl<T> ReducedDelta<T> {
    /// Builds a script from edits.
    #[must_use]
    pub fn from_edits(edits: Vec<ReducedEdit<T>>) -> Self {
        Self { edits }
    }

    /// The edits in application order.
    #[must_use]
    pub fn edits(&self) -> &[ReducedEdit<T>] {
        &self.edits
    }

    /// Appends an edit.
    pub fn push(&mut self, edit: ReducedEdit<T>) {
        self.edits.push(edit);
    }
}

/// One keyed edit to a derived list occurrence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyedListEdit<T> {
    /// Set one occurrence payload.
    Set {
        /// Occurrence identity.
        id: u64,
        /// New payload.
        value: T,
    },
    /// Remove one occurrence.
    Remove {
        /// Occurrence identity.
        id: u64,
    },
}

/// An ordered script of keyed list edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyedListDelta<T> {
    edits: Vec<KeyedListEdit<T>>,
}

impl<T> Default for KeyedListDelta<T> {
    fn default() -> Self {
        Self { edits: Vec::new() }
    }
}

impl<T> KeyedListDelta<T> {
    /// The empty script.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a script from edits.
    #[must_use]
    pub fn from_edits(edits: Vec<KeyedListEdit<T>>) -> Self {
        Self { edits }
    }

    /// The edits in application order.
    #[must_use]
    pub fn edits(&self) -> &[KeyedListEdit<T>] {
        &self.edits
    }

    /// Appends an edit.
    pub fn push(&mut self, edit: KeyedListEdit<T>) {
        self.edits.push(edit);
    }
}

/// One edit to a map.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MapEdit<K, V> {
    /// Insert a key with a value.
    Insert {
        /// Key to insert.
        key: K,
        /// Associated value.
        value: V,
    },
    /// Remove a key.
    Remove {
        /// Key to remove.
        key: K,
    },
    /// Change the value of an existing key.
    Update {
        /// Existing key.
        key: K,
        /// Replacement value.
        value: V,
    },
}

/// An ordered script of map edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapDelta<K, V> {
    edits: Vec<MapEdit<K, V>>,
}

impl<K, V> Default for MapDelta<K, V> {
    fn default() -> Self {
        Self { edits: Vec::new() }
    }
}

impl<K, V> MapDelta<K, V> {
    /// The empty script.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a script from edits.
    #[must_use]
    pub fn from_edits(edits: Vec<MapEdit<K, V>>) -> Self {
        Self { edits }
    }

    /// The edits in application order.
    #[must_use]
    pub fn edits(&self) -> &[MapEdit<K, V>] {
        &self.edits
    }

    /// Appends an edit.
    pub fn push(&mut self, edit: MapEdit<K, V>) {
        self.edits.push(edit);
    }
}

impl<K, V> Delta for MapDelta<K, V>
where
    K: Ord + Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    type Target = PMap<K, V>;

    fn empty() -> Self {
        Self::new()
    }

    fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }

    fn apply(&self, target: &Self::Target) -> Self::Target {
        let mut map = target.clone();
        for edit in &self.edits {
            map = match edit {
                MapEdit::Insert { key, value } | MapEdit::Update { key, value } => {
                    map.insert(key.clone(), value.clone()).0
                }
                MapEdit::Remove { key } => map.remove(key).0,
            };
        }
        map
    }

    fn compose(&self, later: &Self) -> Self {
        let mut edits = self.edits.clone();
        edits.extend(later.edits.iter().cloned());
        Self { edits }
    }
}

/// One edit to a set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetEdit<T> {
    /// Insert a member.
    Insert {
        /// Member to insert.
        value: T,
    },
    /// Remove a member.
    Remove {
        /// Member to remove.
        value: T,
    },
}

/// An ordered script of set edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetDelta<T> {
    edits: Vec<SetEdit<T>>,
}

impl<T> Default for SetDelta<T> {
    fn default() -> Self {
        Self { edits: Vec::new() }
    }
}

impl<T> SetDelta<T> {
    /// The empty script.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a script from edits.
    #[must_use]
    pub fn from_edits(edits: Vec<SetEdit<T>>) -> Self {
        Self { edits }
    }

    /// The edits in application order.
    #[must_use]
    pub fn edits(&self) -> &[SetEdit<T>] {
        &self.edits
    }

    /// Appends an edit.
    pub fn push(&mut self, edit: SetEdit<T>) {
        self.edits.push(edit);
    }
}

impl<T> Delta for SetDelta<T>
where
    T: Ord + Clone + Send + Sync + 'static,
{
    type Target = PSet<T>;

    fn empty() -> Self {
        Self::new()
    }

    fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }

    fn apply(&self, target: &Self::Target) -> Self::Target {
        let mut set = target.clone();
        for edit in &self.edits {
            set = match edit {
                SetEdit::Insert { value } => set.insert(value.clone()).0,
                SetEdit::Remove { value } => set.remove(value).0,
            };
        }
        set
    }

    fn compose(&self, later: &Self) -> Self {
        let mut edits = self.edits.clone();
        edits.extend(later.edits.iter().cloned());
        Self { edits }
    }
}

#[cfg(test)]
#[path = "../tests/unit/delta/applied_tests.rs"]
mod applied_tests;

#[cfg(test)]
#[path = "../tests/unit/delta/tests.rs"]
mod tests;
