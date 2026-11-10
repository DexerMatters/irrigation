//! Persistent ordered collections built on path-copying AVL trees.
//!
//! All operations are `O(log n)` and share structure with the previous value.
//! The trees are not exposed directly; [`PMap`], [`PSeq`], and [`PSet`] are the
//! public shapes.

use std::sync::Arc;

type Link<T> = Option<Arc<T>>;

/// A persistent ordered map.
///
/// Keys must be totally ordered; iteration is deterministic and sorted. This is
/// the representation behind map Views and ordered indexes.
#[derive(Clone)]
pub struct PMap<K, V> {
    root: Option<Arc<Entry<K, V>>>,
    len: usize,
}

struct Entry<K, V> {
    key: K,
    value: V,
    left: Option<Arc<Entry<K, V>>>,
    right: Option<Arc<Entry<K, V>>>,
    height: u16,
    size: usize,
}

impl<K, V> Entry<K, V> {
    fn new(
        key: K,
        value: V,
        left: Option<Arc<Entry<K, V>>>,
        right: Option<Arc<Entry<K, V>>>,
    ) -> Self {
        let height = 1 + entry_height(&left).max(entry_height(&right));
        let size = 1 + entry_size(&left) + entry_size(&right);
        Self {
            key,
            value,
            left,
            right,
            height,
            size,
        }
    }
}

fn entry_height<K, V>(link: &Option<Arc<Entry<K, V>>>) -> u16 {
    link.as_ref().map_or(0, |entry| entry.height)
}

fn entry_size<K, V>(link: &Option<Arc<Entry<K, V>>>) -> usize {
    link.as_ref().map_or(0, |entry| entry.size)
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "`Link` is the option-typed child pointer every caller expects"
)]
fn entry_wrap<K, V>(
    key: K,
    value: V,
    left: Option<Arc<Entry<K, V>>>,
    right: Option<Arc<Entry<K, V>>>,
) -> Option<Arc<Entry<K, V>>> {
    Some(Arc::new(Entry::new(key, value, left, right)))
}

fn entry_rotate_right<K: Clone, V: Clone>(entry: Arc<Entry<K, V>>) -> Arc<Entry<K, V>> {
    let left = entry.left.as_ref().expect("rotate requires left child");
    Arc::new(Entry::new(
        left.key.clone(),
        left.value.clone(),
        left.left.clone(),
        entry_wrap(
            entry.key.clone(),
            entry.value.clone(),
            left.right.clone(),
            entry.right.clone(),
        ),
    ))
}

fn entry_rotate_left<K: Clone, V: Clone>(entry: Arc<Entry<K, V>>) -> Arc<Entry<K, V>> {
    let right = entry.right.as_ref().expect("rotate requires right child");
    Arc::new(Entry::new(
        right.key.clone(),
        right.value.clone(),
        entry_wrap(
            entry.key.clone(),
            entry.value.clone(),
            entry.left.clone(),
            right.left.clone(),
        ),
        right.right.clone(),
    ))
}

fn entry_rebalance<K: Clone, V: Clone>(entry: Arc<Entry<K, V>>) -> Arc<Entry<K, V>> {
    let balance = i32::from(entry_height(&entry.left)) - i32::from(entry_height(&entry.right));
    if balance > 1 {
        let left = entry.left.as_ref().expect("left child");
        if entry_height(&left.left) < entry_height(&left.right) {
            let rotated = Arc::new(Entry::new(
                entry.key.clone(),
                entry.value.clone(),
                Some(entry_rotate_left(Arc::clone(left))),
                entry.right.clone(),
            ));
            return entry_rotate_right(rotated);
        }
        entry_rotate_right(entry)
    } else if balance < -1 {
        let right = entry.right.as_ref().expect("right child");
        if entry_height(&right.right) < entry_height(&right.left) {
            let rotated = Arc::new(Entry::new(
                entry.key.clone(),
                entry.value.clone(),
                entry.left.clone(),
                Some(entry_rotate_right(Arc::clone(right))),
            ));
            return entry_rotate_left(rotated);
        }
        entry_rotate_left(entry)
    } else {
        entry
    }
}

fn entry_insert<K: Ord + Clone, V: Clone>(
    link: Option<Arc<Entry<K, V>>>,
    key: K,
    value: V,
) -> (Option<Arc<Entry<K, V>>>, bool) {
    match link {
        None => (entry_wrap(key, value, None, None), true),
        Some(entry) => match key.cmp(&entry.key) {
            std::cmp::Ordering::Less => {
                let (left, inserted) = entry_insert(entry.left.clone(), key, value);
                let rebuilt = Arc::new(Entry::new(
                    entry.key.clone(),
                    entry.value.clone(),
                    left,
                    entry.right.clone(),
                ));
                (Some(entry_rebalance(rebuilt)), inserted)
            }
            std::cmp::Ordering::Greater => {
                let (right, inserted) = entry_insert(entry.right.clone(), key, value);
                let rebuilt = Arc::new(Entry::new(
                    entry.key.clone(),
                    entry.value.clone(),
                    entry.left.clone(),
                    right,
                ));
                (Some(entry_rebalance(rebuilt)), inserted)
            }
            std::cmp::Ordering::Equal => {
                let rebuilt = Arc::new(Entry::new(
                    key,
                    value,
                    entry.left.clone(),
                    entry.right.clone(),
                ));
                (Some(rebuilt), false)
            }
        },
    }
}

type EntryLink<K, V> = Option<Arc<Entry<K, V>>>;

fn entry_remove_min<K: Ord + Clone, V: Clone>(
    entry: Arc<Entry<K, V>>,
) -> (EntryLink<K, V>, Arc<Entry<K, V>>) {
    match entry.left.clone() {
        None => (entry.right.clone(), entry),
        Some(left) => {
            let (new_left, min) = entry_remove_min(left);
            let rebuilt = Arc::new(Entry::new(
                entry.key.clone(),
                entry.value.clone(),
                new_left,
                entry.right.clone(),
            ));
            (Some(entry_rebalance(rebuilt)), min)
        }
    }
}

fn entry_remove<K: Ord + Clone, V: Clone>(
    link: Option<Arc<Entry<K, V>>>,
    key: &K,
) -> (Option<Arc<Entry<K, V>>>, Option<V>) {
    match link {
        None => (None, None),
        Some(entry) => match key.cmp(&entry.key) {
            std::cmp::Ordering::Less => {
                let (left, removed) = entry_remove(entry.left.clone(), key);
                let rebuilt = Arc::new(Entry::new(
                    entry.key.clone(),
                    entry.value.clone(),
                    left,
                    entry.right.clone(),
                ));
                (Some(entry_rebalance(rebuilt)), removed)
            }
            std::cmp::Ordering::Greater => {
                let (right, removed) = entry_remove(entry.right.clone(), key);
                let rebuilt = Arc::new(Entry::new(
                    entry.key.clone(),
                    entry.value.clone(),
                    entry.left.clone(),
                    right,
                ));
                (Some(entry_rebalance(rebuilt)), removed)
            }
            std::cmp::Ordering::Equal => {
                let removed = entry.value.clone();
                match (entry.left.clone(), entry.right.clone()) {
                    (None, right) => (right, Some(removed)),
                    (left, None) => (left, Some(removed)),
                    (left, Some(right)) => {
                        let (new_right, successor) = entry_remove_min(right);
                        let rebuilt = Arc::new(Entry::new(
                            successor.key.clone(),
                            successor.value.clone(),
                            left,
                            new_right,
                        ));
                        (Some(entry_rebalance(rebuilt)), Some(removed))
                    }
                }
            }
        },
    }
}

impl<K: Ord, V> Default for PMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Ord, V> PMap<K, V> {
    /// The empty map.
    #[must_use]
    pub fn new() -> Self {
        Self { root: None, len: 0 }
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` when the map has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Looks up a key.
    #[must_use]
    pub fn get(&self, key: &K) -> Option<&V> {
        let mut current = self.root.as_deref();
        while let Some(entry) = current {
            match key.cmp(&entry.key) {
                std::cmp::Ordering::Less => current = entry.left.as_deref(),
                std::cmp::Ordering::Greater => current = entry.right.as_deref(),
                std::cmp::Ordering::Equal => return Some(&entry.value),
            }
        }
        None
    }

    /// Returns `true` when the key is present.
    #[must_use]
    pub fn contains_key(&self, key: &K) -> bool {
        self.get(key).is_some()
    }

    /// Iterates in key order.
    pub fn iter(&self) -> Iter<'_, K, V> {
        let mut stack = Vec::new();
        let mut current = self.root.as_deref();
        while let Some(entry) = current {
            stack.push(entry);
            current = entry.left.as_deref();
        }
        Iter { stack }
    }

    /// First key and value in order.
    #[must_use]
    pub fn first_key_value(&self) -> Option<(&K, &V)> {
        let mut current = self.root.as_deref()?;
        while let Some(left) = current.left.as_deref() {
            current = left;
        }
        Some((&current.key, &current.value))
    }
}

impl<K: Ord + Clone, V: Clone> PMap<K, V> {
    /// Inserts or replaces a value, returning the previous value.
    #[must_use]
    pub fn insert(&self, key: K, value: V) -> (Self, Option<V>) {
        let previous = self.get(&key).cloned();
        let (root, _) = entry_insert(self.root.clone(), key, value);
        (
            Self {
                root,
                len: if previous.is_some() {
                    self.len
                } else {
                    self.len + 1
                },
            },
            previous,
        )
    }

    /// Removes a key, returning the new map and the removed value.
    #[must_use]
    pub fn remove(&self, key: &K) -> (Self, Option<V>) {
        let (root, removed) = entry_remove(self.root.clone(), key);
        (
            Self {
                root,
                len: if removed.is_some() {
                    self.len - 1
                } else {
                    self.len
                },
            },
            removed,
        )
    }

    /// Returns a map with `key` mapped to `value`, reusing the receiver when
    /// the value is already equal.
    #[must_use]
    pub fn insert_if_changed(&self, key: K, value: V) -> (Self, bool)
    where
        V: PartialEq,
    {
        match self.get(&key) {
            Some(existing) if *existing == value => (self.clone(), false),
            _ => {
                let (map, _) = self.insert(key, value);
                (map, true)
            }
        }
    }
}

impl<K: Ord + Clone, V: Clone> FromIterator<(K, V)> for PMap<K, V> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let mut map = Self::new();
        for (key, value) in iter {
            let (next, _) = map.insert(key, value);
            map = next;
        }
        map
    }
}

impl<K: Ord + Clone, V: Clone> IntoIterator for PMap<K, V> {
    type Item = (K, V);
    type IntoIter = IntoIter<K, V>;

    fn into_iter(self) -> Self::IntoIter {
        IntoIter {
            inner: self
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<Vec<_>>()
                .into_iter(),
        }
    }
}

/// Iterator over a [`PMap`] in key order.
pub struct Iter<'a, K, V> {
    stack: Vec<&'a Entry<K, V>>,
}

impl<K, V> std::fmt::Debug for Iter<'_, K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Iter").finish_non_exhaustive()
    }
}

impl<'a, K, V> Iterator for Iter<'a, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        let entry = self.stack.pop()?;
        let mut current = entry.right.as_deref();
        while let Some(node) = current {
            self.stack.push(node);
            current = node.left.as_deref();
        }
        Some((&entry.key, &entry.value))
    }
}

/// Owning iterator over a [`PMap`].
pub struct IntoIter<K, V> {
    inner: std::vec::IntoIter<(K, V)>,
}

impl<K, V> std::fmt::Debug for IntoIter<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IntoIter").finish_non_exhaustive()
    }
}

impl<K, V> Iterator for IntoIter<K, V> {
    type Item = (K, V);

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next()
    }
}

impl<K: std::fmt::Debug + Ord, V: std::fmt::Debug> std::fmt::Debug for PMap<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl<K: Ord, V: PartialEq> PartialEq for PMap<K, V> {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len
            && self
                .iter()
                .zip(other.iter())
                .all(|((ka, va), (kb, vb))| ka == kb && va == vb)
    }
}

impl<K: Ord + Eq, V: Eq> Eq for PMap<K, V> {}

// ---------------------------------------------------------------------------
// Persistent sequence
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct SeqNode<T> {
    value: T,
    left: Link<SeqNode<T>>,
    right: Link<SeqNode<T>>,
    height: u16,
    size: usize,
}

fn seq_height<T>(link: &Link<SeqNode<T>>) -> u16 {
    link.as_ref().map_or(0, |node| node.height)
}

fn seq_size<T>(link: &Link<SeqNode<T>>) -> usize {
    link.as_ref().map_or(0, |node| node.size)
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "`Link` is the option-typed child pointer every caller expects"
)]
fn seq_wrap<T>(value: T, left: Link<SeqNode<T>>, right: Link<SeqNode<T>>) -> Link<SeqNode<T>> {
    Some(Arc::new(SeqNode::new(value, left, right)))
}

impl<T> SeqNode<T> {
    fn new(value: T, left: Link<SeqNode<T>>, right: Link<SeqNode<T>>) -> Self {
        let height = 1 + seq_height(&left).max(seq_height(&right));
        let size = 1 + seq_size(&left) + seq_size(&right);
        Self {
            value,
            left,
            right,
            height,
            size,
        }
    }
}

fn seq_rotate_right<T: Clone>(node: Arc<SeqNode<T>>) -> Arc<SeqNode<T>> {
    let left = node.left.as_ref().expect("left child");
    Arc::new(SeqNode::new(
        left.value.clone(),
        left.left.clone(),
        seq_wrap(node.value.clone(), left.right.clone(), node.right.clone()),
    ))
}

fn seq_rotate_left<T: Clone>(node: Arc<SeqNode<T>>) -> Arc<SeqNode<T>> {
    let right = node.right.as_ref().expect("right child");
    Arc::new(SeqNode::new(
        right.value.clone(),
        seq_wrap(node.value.clone(), node.left.clone(), right.left.clone()),
        right.right.clone(),
    ))
}

fn seq_rebalance<T: Clone>(node: Arc<SeqNode<T>>) -> Arc<SeqNode<T>> {
    let balance = i32::from(seq_height(&node.left)) - i32::from(seq_height(&node.right));
    if balance > 1 {
        let left = node.left.as_ref().expect("left child");
        if seq_height(&left.left) < seq_height(&left.right) {
            let rotated = Arc::new(SeqNode::new(
                node.value.clone(),
                Some(seq_rotate_left(Arc::clone(left))),
                node.right.clone(),
            ));
            return seq_rotate_right(rotated);
        }
        seq_rotate_right(node)
    } else if balance < -1 {
        let right = node.right.as_ref().expect("right child");
        if seq_height(&right.right) < seq_height(&right.left) {
            let rotated = Arc::new(SeqNode::new(
                node.value.clone(),
                node.left.clone(),
                Some(seq_rotate_right(Arc::clone(right))),
            ));
            return seq_rotate_left(rotated);
        }
        seq_rotate_left(node)
    } else {
        node
    }
}

fn seq_insert_at<T: Clone>(link: Link<SeqNode<T>>, index: usize, value: T) -> Link<SeqNode<T>> {
    match link {
        None => seq_wrap(value, None, None),
        Some(node) => {
            let left_size = seq_size(&node.left);
            if index <= left_size {
                let left = seq_insert_at(node.left.clone(), index, value);
                let rebuilt = Arc::new(SeqNode::new(node.value.clone(), left, node.right.clone()));
                Some(seq_rebalance(rebuilt))
            } else {
                let right = seq_insert_at(node.right.clone(), index - left_size - 1, value);
                let rebuilt = Arc::new(SeqNode::new(node.value.clone(), node.left.clone(), right));
                Some(seq_rebalance(rebuilt))
            }
        }
    }
}

fn seq_remove_at<T: Clone>(link: Link<SeqNode<T>>, index: usize) -> (Link<SeqNode<T>>, Option<T>) {
    match link {
        None => (None, None),
        Some(node) => {
            let left_size = seq_size(&node.left);
            match index.cmp(&left_size) {
                std::cmp::Ordering::Less => {
                    let (left, removed) = seq_remove_at(node.left.clone(), index);
                    let rebuilt =
                        Arc::new(SeqNode::new(node.value.clone(), left, node.right.clone()));
                    (Some(seq_rebalance(rebuilt)), removed)
                }
                std::cmp::Ordering::Greater => {
                    let (right, removed) = seq_remove_at(node.right.clone(), index - left_size - 1);
                    let rebuilt =
                        Arc::new(SeqNode::new(node.value.clone(), node.left.clone(), right));
                    (Some(seq_rebalance(rebuilt)), removed)
                }
                std::cmp::Ordering::Equal => {
                    let removed = node.value.clone();
                    match (node.left.clone(), node.right.clone()) {
                        (None, right) => (right, Some(removed)),
                        (left, None) => (left, Some(removed)),
                        (left, Some(right)) => {
                            let (new_right, successor) = seq_remove_first(right);
                            let rebuilt =
                                Arc::new(SeqNode::new(successor.value.clone(), left, new_right));
                            (Some(seq_rebalance(rebuilt)), Some(removed))
                        }
                    }
                }
            }
        }
    }
}

fn seq_remove_first<T: Clone>(node: Arc<SeqNode<T>>) -> (Link<SeqNode<T>>, Arc<SeqNode<T>>) {
    match node.left.clone() {
        None => (node.right.clone(), node),
        Some(left) => {
            let (new_left, first) = seq_remove_first(left);
            let rebuilt = Arc::new(SeqNode::new(
                node.value.clone(),
                new_left,
                node.right.clone(),
            ));
            (Some(seq_rebalance(rebuilt)), first)
        }
    }
}

fn seq_get<T>(link: &Link<SeqNode<T>>, index: usize) -> Option<&T> {
    let mut current = link.as_deref();
    let mut index = index;
    while let Some(node) = current {
        let left_size = seq_size(&node.left);
        match index.cmp(&left_size) {
            std::cmp::Ordering::Less => current = node.left.as_deref(),
            std::cmp::Ordering::Greater => {
                index -= left_size + 1;
                current = node.right.as_deref();
            }
            std::cmp::Ordering::Equal => return Some(&node.value),
        }
    }
    None
}

/// A persistent sequence with `O(log n)` indexing and splicing.
///
/// This is the storage behind list Views. Elements keep their identity across
/// insertions because callers store stable ids as the element type.
#[derive(Clone)]
pub struct PSeq<T> {
    root: Link<SeqNode<T>>,
    len: usize,
}

impl<T> Default for PSeq<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> PSeq<T> {
    /// The empty sequence.
    #[must_use]
    pub fn new() -> Self {
        Self { root: None, len: 0 }
    }

    /// Number of elements.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` when the sequence is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the element at `index`.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&T> {
        seq_get(&self.root, index)
    }

    /// Iterates in order.
    pub fn iter(&self) -> SeqIter<'_, T> {
        let mut stack = Vec::new();
        let mut current = self.root.as_deref();
        while let Some(node) = current {
            stack.push(node);
            current = node.left.as_deref();
        }
        SeqIter { stack }
    }
}

impl<T: Clone> PSeq<T> {
    /// Inserts at `index`, shifting later elements right.
    ///
    /// # Panics
    ///
    /// Panics when `index > len`.
    #[must_use]
    pub fn insert(&self, index: usize, value: T) -> Self {
        assert!(index <= self.len, "insert index {index} out of bounds");
        Self {
            root: seq_insert_at(self.root.clone(), index, value),
            len: self.len + 1,
        }
    }

    /// Appends an element.
    #[must_use]
    pub fn push(&self, value: T) -> Self {
        self.insert(self.len, value)
    }

    /// Removes and returns the element at `index`.
    #[must_use]
    pub fn remove(&self, index: usize) -> (Self, Option<T>) {
        if index >= self.len {
            return (self.clone(), None);
        }
        let (root, removed) = seq_remove_at(self.root.clone(), index);
        (
            Self {
                root,
                len: self.len - 1,
            },
            removed,
        )
    }

    /// Replaces the element at `index`.
    ///
    /// # Panics
    ///
    /// Panics when `index` is at or past the length; see [`PSeq::insert`] for the checked
    /// path.
    #[must_use]
    pub fn set(&self, index: usize, value: T) -> Self {
        assert!(index < self.len, "set index {index} out of bounds");
        let (without, _) = self.remove(index);
        without.insert(index, value)
    }

    /// Moves the element at `from` to `to`, preserving element identity.
    #[must_use]
    pub fn move_element(&self, from: usize, to: usize) -> Self {
        if from == to || from >= self.len || to >= self.len {
            return self.clone();
        }
        let (without, element) = self.remove(from);
        match element {
            Some(element) => without.insert(to, element),
            None => without,
        }
    }

    /// Collects the sequence into a vector.
    #[must_use]
    pub fn to_vec(&self) -> Vec<T> {
        self.iter().cloned().collect()
    }
}

impl<T: Clone> FromIterator<T> for PSeq<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let values: Vec<T> = iter.into_iter().collect();
        let mut seq = Self::new();
        for (index, value) in values.into_iter().enumerate() {
            seq = seq.insert(index, value);
        }
        seq
    }
}

impl<T: PartialEq> PartialEq for PSeq<T> {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len && self.iter().eq(other.iter())
    }
}

impl<T: Eq> Eq for PSeq<T> {}

impl<T: std::fmt::Debug> std::fmt::Debug for PSeq<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

/// Iterator over a [`PSeq`] in order.
pub struct SeqIter<'a, T> {
    stack: Vec<&'a SeqNode<T>>,
}

impl<T> std::fmt::Debug for SeqIter<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SeqIter").finish_non_exhaustive()
    }
}

impl<'a, T> Iterator for SeqIter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        let node = self.stack.pop()?;
        let mut current = node.right.as_deref();
        while let Some(node) = current {
            self.stack.push(node);
            current = node.left.as_deref();
        }
        Some(&node.value)
    }
}

// ---------------------------------------------------------------------------
// Persistent set
// ---------------------------------------------------------------------------

/// A persistent ordered set.
#[derive(Clone)]
pub struct PSet<T> {
    map: PMap<T, ()>,
}

impl<T: Ord + std::fmt::Debug> std::fmt::Debug for PSet<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

impl<T: Ord> PartialEq for PSet<T> {
    fn eq(&self, other: &Self) -> bool {
        self.map == other.map
    }
}

impl<T: Ord> Eq for PSet<T> {}

impl<T: Ord> Default for PSet<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Ord> PSet<T> {
    /// The empty set.
    #[must_use]
    pub fn new() -> Self {
        Self { map: PMap::new() }
    }

    /// Number of members.
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Returns `true` when the set has no members.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Membership test.
    #[must_use]
    pub fn contains(&self, value: &T) -> bool {
        self.map.contains_key(value)
    }

    /// Iterates in order.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.map.iter().map(|(key, ())| key)
    }

    /// First member in order.
    #[must_use]
    pub fn first(&self) -> Option<&T> {
        self.map.first_key_value().map(|(key, ())| key)
    }
}

impl<T: Ord + Clone> PSet<T> {
    /// Inserts a member, returning the new set and whether it was new.
    pub fn insert(&self, value: T) -> (Self, bool) {
        if self.contains(&value) {
            return (self.clone(), false);
        }
        let (map, _) = self.map.insert(value, ());
        (Self { map }, true)
    }

    /// Removes a member, returning the new set and whether it was present.
    pub fn remove(&self, value: &T) -> (Self, bool) {
        let (map, removed) = self.map.remove(value);
        (Self { map }, removed.is_some())
    }
}

impl<T: Ord + Clone> FromIterator<T> for PSet<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut set = Self::new();
        for value in iter {
            let (next, _) = set.insert(value);
            set = next;
        }
        set
    }
}

impl<T: Ord + Clone> IntoIterator for PSet<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter().cloned().collect::<Vec<_>>().into_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_operations_preserve_order_and_length() {
        let map = PMap::new();
        let (map, previous) = map.insert(3, "c");
        assert_eq!(previous, None);
        let (map, _) = map.insert(1, "a");
        let (map, _) = map.insert(2, "b");
        assert_eq!(map.len(), 3);
        assert_eq!(map.get(&2), Some(&"b"));
        assert_eq!(
            map.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );

        let (map, removed) = map.remove(&2);
        assert_eq!(removed, Some("b"));
        assert_eq!(map.len(), 2);
        assert_eq!(map.get(&2), None);
        assert_eq!(map.iter().map(|(k, _)| *k).collect::<Vec<_>>(), vec![1, 3]);
    }

    #[test]
    fn map_shares_structure_with_previous_version() {
        let base: PMap<u32, u32> = (0..64).map(|i| (i, i)).collect();
        let (edited, _) = base.insert(10, 999);
        assert_eq!(base.get(&10), Some(&10));
        assert_eq!(edited.get(&10), Some(&999));
        assert_eq!(base.len(), edited.len());
    }

    #[test]
    fn map_insert_if_changed_reuses_equal_values() {
        let map: PMap<u32, u32> = [(1, 1)].into_iter().collect();
        let (same, changed) = map.insert_if_changed(1, 1);
        assert!(!changed);
        assert_eq!(same.get(&1), Some(&1));
        let (different, changed) = map.insert_if_changed(1, 2);
        assert!(changed);
        assert_eq!(different.get(&1), Some(&2));
    }

    #[test]
    fn sequence_supports_insert_remove_move_and_indexing() {
        let seq: PSeq<u32> = [1, 2, 3, 4].into_iter().collect();
        assert_eq!(seq.len(), 4);
        assert_eq!(seq.get(2), Some(&3));

        let inserted = seq.insert(0, 0);
        assert_eq!(inserted.to_vec(), vec![0, 1, 2, 3, 4]);
        assert_eq!(seq.to_vec(), vec![1, 2, 3, 4]);

        let (removed, value) = inserted.remove(0);
        assert_eq!(value, Some(0));
        assert_eq!(removed.to_vec(), vec![1, 2, 3, 4]);

        let moved = removed.move_element(0, 3);
        assert_eq!(moved.to_vec(), vec![2, 3, 4, 1]);
    }

    #[test]
    fn sequence_handles_large_sizes() {
        let seq: PSeq<u32> = (0..1000).collect();
        assert_eq!(seq.len(), 1000);
        for index in [0, 1, 499, 500, 998, 999] {
            assert_eq!(seq.get(index), Some(&(index as u32)));
        }
        let inserted = seq.insert(500, 10_000);
        assert_eq!(inserted.get(500), Some(&10_000));
        assert_eq!(inserted.get(501), Some(&500));
        assert_eq!(inserted.len(), 1001);
    }

    #[test]
    fn set_tracks_membership() {
        let set: PSet<u32> = [3, 1, 2, 1].into_iter().collect();
        assert_eq!(set.len(), 3);
        assert!(set.contains(&2));
        assert_eq!(set.iter().copied().collect::<Vec<_>>(), vec![1, 2, 3]);
        let (removed, was_present) = set.remove(&2);
        assert!(was_present);
        assert!(!removed.contains(&2));
        assert_eq!(removed.len(), 2);
    }
}
