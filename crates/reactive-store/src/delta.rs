//! Change contracts for the built-in View families.
//!
//! Each delta is an ordered edit script. Composition concatenates scripts;
//! normalization is done by the runtime at the transaction boundary by applying
//! the script and comparing final values, so an edit followed by its reversal
//! produces no observable change.

use reactive_core::change::Delta;

use crate::persistent::{PMap, PSeq, PSet};

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
mod tests {
    use super::*;

    #[test]
    fn list_delta_composition_matches_sequential_application() {
        let base: PSeq<u32> = [1, 2, 3].into_iter().collect();
        let first = ListDelta::from_edits(vec![
            ListEdit::Insert { index: 0, value: 0 },
            ListEdit::Replace { index: 2, value: 20 },
        ]);
        let second = ListDelta::from_edits(vec![ListEdit::Move { from: 0, to: 3 }]);

        let sequential = second.apply(&first.apply(&base));
        let composed = first.compose(&second).apply(&base);
        assert_eq!(sequential, composed);
        assert_eq!(sequential.to_vec(), vec![1, 20, 3, 0]);
    }

    #[test]
    fn empty_delta_is_identity() {
        let base: PMap<u32, u32> = [(1, 1)].into_iter().collect();
        let delta: MapDelta<u32, u32> = MapDelta::new();
        assert!(delta.is_empty());
        assert_eq!(delta.apply(&base), base);
    }

    #[test]
    fn set_delta_insert_then_remove_is_observably_empty() {
        let base: PSet<u32> = [1, 2].into_iter().collect();
        let delta = SetDelta::from_edits(vec![
            SetEdit::Insert { value: 9 },
            SetEdit::Remove { value: 9 },
        ]);
        assert_eq!(delta.apply(&base), base);
    }
}
