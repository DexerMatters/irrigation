//! Persistent ownership trees with stable node identity.
//!
//! A tree node is an entity: its identity survives moves, its children are an
//! ordered list of child identities, and replacing a node's payload does not
//! disturb its descendants. Traversal is iterative so arbitrarily deep trees do
//! not overflow the host stack.

use thiserror::Error;

use reactive_core::change::Delta;

use crate::persistent::PMap;

/// Identity of a tree node.
pub type TreeNodeId = u64;

/// One node: its parent, ordered children, and payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeNode<N> {
    /// Parent identity, `None` for the root.
    pub parent: Option<TreeNodeId>,
    /// Ordered children.
    pub children: Vec<TreeNodeId>,
    /// Node payload.
    pub value: N,
}

/// A position in a child list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChildSlot {
    /// The parent node.
    pub parent: TreeNodeId,
    /// Index among the parent's children.
    pub index: usize,
}

/// Error returned by structural tree operations.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum TreeError {
    /// The referenced node does not exist.
    #[error("tree node {0} does not exist")]
    MissingNode(TreeNodeId),
    /// A node cannot be moved into its own subtree.
    #[error("cannot move tree node {node} under its own descendant {parent}")]
    Cycle {
        /// Node being moved.
        node: TreeNodeId,
        /// Requested parent.
        parent: TreeNodeId,
    },
    /// The root cannot be removed while other nodes exist.
    #[error("cannot remove the root of a non-empty tree")]
    CannotRemoveRoot,
}

/// A persistent ownership tree.
#[derive(Clone, Debug)]
pub struct TreeStore<N> {
    nodes: PMap<TreeNodeId, TreeNode<N>>,
    root: Option<TreeNodeId>,
    next_id: TreeNodeId,
}

impl<N> Default for TreeStore<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<N> TreeStore<N> {
    /// An empty tree.
    #[must_use]
    pub fn new() -> Self {
        Self {
            nodes: PMap::new(),
            root: None,
            next_id: 0,
        }
    }

    /// The root identity, if the tree is non-empty.
    #[must_use]
    pub fn root(&self) -> Option<TreeNodeId> {
        self.root
    }

    /// Number of nodes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Returns `true` when the tree has no nodes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Looks up a node.
    #[must_use]
    pub fn get(&self, id: TreeNodeId) -> Option<&TreeNode<N>> {
        self.nodes.get(&id)
    }

    /// Returns a node's parent.
    #[must_use]
    pub fn parent(&self, id: TreeNodeId) -> Option<TreeNodeId> {
        self.nodes.get(&id).and_then(|node| node.parent)
    }

    /// Returns a node's ordered children.
    #[must_use]
    pub fn children(&self, id: TreeNodeId) -> Option<&[TreeNodeId]> {
        self.nodes.get(&id).map(|node| node.children.as_slice())
    }

    /// Iterates over all nodes in id order.
    pub fn nodes(&self) -> impl Iterator<Item = (TreeNodeId, &TreeNode<N>)> {
        self.nodes.iter().map(|(id, node)| (*id, node))
    }

    /// Pre-order traversal starting at the root, using an explicit stack.
    pub fn preorder(&self) -> Preorder<'_, N> {
        let mut stack = Vec::new();
        if let Some(root) = self.root {
            stack.push(root);
        }
        Preorder {
            tree: self,
            stack,
        }
    }

    /// Depth-first traversal rooted at `id`.
    pub fn preorder_from(&self, id: TreeNodeId) -> Preorder<'_, N> {
        Preorder {
            tree: self,
            stack: vec![id],
        }
    }

    /// Computes the depth of a node by walking parents.
    #[must_use]
    pub fn depth(&self, id: TreeNodeId) -> usize {
        let mut depth = 0;
        let mut current = id;
        while let Some(parent) = self.parent(current) {
            depth += 1;
            current = parent;
            if depth > self.nodes.len() {
                break;
            }
        }
        depth
    }

    /// Returns the ancestor chain from the root down to `id`.
    #[must_use]
    pub fn path_to_root(&self, id: TreeNodeId) -> Vec<TreeNodeId> {
        let mut path = vec![id];
        let mut current = id;
        while let Some(parent) = self.parent(current) {
            path.push(parent);
            current = parent;
            if path.len() > self.nodes.len() + 1 {
                break;
            }
        }
        path.reverse();
        path
    }
}

impl<N: Clone> TreeStore<N> {
    /// Creates a tree with a root node.
    #[must_use]
    pub fn with_root(&self, value: N) -> (Self, TreeNodeId) {
        let id = self.next_id;
        let node = TreeNode {
            parent: None,
            children: Vec::new(),
            value,
        };
        let (nodes, _) = self.nodes.insert(id, node);
        (
            Self {
                nodes,
                root: Some(id),
                next_id: id + 1,
            },
            id,
        )
    }

    /// Inserts a child at `index`, returning the new tree and the new identity.
    ///
    /// # Panics
    ///
    /// Panics when `parent` does not exist or `index` is out of bounds.
    #[must_use]
    pub fn insert_child(&self, parent: TreeNodeId, index: usize, value: N) -> (Self, TreeNodeId) {
        let id = self.next_id;
        let mut tree = self.clone();
        tree = tree.attach_new(id, parent, index, value);
        (tree, id)
    }

    /// Appends a child, returning the new tree and the new identity.
    #[must_use]
    pub fn append_child(&self, parent: TreeNodeId, value: N) -> (Self, TreeNodeId) {
        let index = self
            .children(parent)
            .map_or(0, <[TreeNodeId]>::len);
        self.insert_child(parent, index, value)
    }

    fn attach_new(&self, id: TreeNodeId, parent: TreeNodeId, index: usize, value: N) -> Self {
        let mut parent_node = self
            .nodes
            .get(&parent)
            .cloned()
            .expect("insert_child requires an existing parent");
        let index = index.min(parent_node.children.len());
        parent_node.children.insert(index, id);
        let node = TreeNode {
            parent: Some(parent),
            children: Vec::new(),
            value,
        };
        let (nodes, _) = self.nodes.insert(id, node);
        let (nodes, _) = nodes.insert(parent, parent_node);
        Self {
            nodes,
            root: self.root,
            next_id: id + 1,
        }
    }

    /// Replaces a node's payload, keeping its identity and children.
    #[must_use]
    pub fn replace_value(&self, id: TreeNodeId, value: N) -> Self {
        let Some(node) = self.nodes.get(&id).cloned() else {
            return self.clone();
        };
        let (nodes, _) = self.nodes.insert(
            id,
            TreeNode {
                value,
                ..node
            },
        );
        Self {
            nodes,
            root: self.root,
            next_id: self.next_id,
        }
    }

    /// Moves a subtree under a new parent and position.
    ///
    /// Node identities inside the subtree are preserved.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError`] when the node or parent is missing or the move
    /// would create a cycle.
    pub fn move_subtree(
        &self,
        id: TreeNodeId,
        new_parent: TreeNodeId,
        index: usize,
    ) -> Result<Self, TreeError> {
        let node = self
            .nodes
            .get(&id)
            .cloned()
            .ok_or(TreeError::MissingNode(id))?;
        let mut parent_node = self
            .nodes
            .get(&new_parent)
            .cloned()
            .ok_or(TreeError::MissingNode(new_parent))?;
        if id == new_parent || self.is_descendant(new_parent, id) {
            return Err(TreeError::Cycle {
                node: id,
                parent: new_parent,
            });
        }

        let mut tree = self.clone();
        if let Some(old_parent) = node.parent {
            if let Some(mut old) = tree.nodes.get(&old_parent).cloned() {
                old.children.retain(|child| *child != id);
                let (nodes, _) = tree.nodes.insert(old_parent, old);
                tree.nodes = nodes;
            }
        } else {
            tree.root = None;
        }

        let index = index.min(parent_node.children.len());
        parent_node.children.insert(index, id);
        let (nodes, _) = tree.nodes.insert(new_parent, parent_node);
        tree.nodes = nodes;
        let (nodes, _) = tree.nodes.insert(
            id,
            TreeNode {
                parent: Some(new_parent),
                ..node
            },
        );
        tree.nodes = nodes;
        if tree.root.is_none() {
            tree.root = Some(new_parent);
        }
        Ok(tree)
    }

    /// Returns `true` when `candidate` is a descendant of `ancestor`.
    #[must_use]
    pub fn is_descendant(&self, candidate: TreeNodeId, ancestor: TreeNodeId) -> bool {
        let mut current = self.parent(candidate);
        let mut guard = 0;
        while let Some(node) = current {
            if node == ancestor {
                return true;
            }
            guard += 1;
            if guard > self.nodes.len() {
                return false;
            }
            current = self.parent(node);
        }
        false
    }

    /// Removes a subtree, returning the new tree and the removed nodes in
    /// pre-order.
    ///
    /// The returned order is the retraction order for the View layer: a parent
    /// is retracted before its children.
    #[must_use]
    pub fn remove_subtree(&self, id: TreeNodeId) -> (Self, Vec<(TreeNodeId, N)>) {
        let Some(node) = self.nodes.get(&id) else {
            return (self.clone(), Vec::new());
        };
        let parent = node.parent;
        let removed: Vec<TreeNodeId> = self.preorder_from(id).map(|(node, _)| node).collect();
        let mut values = Vec::with_capacity(removed.len());
        let mut tree = self.clone();
        for node_id in &removed {
            if let Some(entry) = tree.nodes.get(node_id) {
                values.push((*node_id, entry.value.clone()));
            }
            tree.nodes = tree.nodes.remove(node_id).0;
        }
        match parent {
            Some(parent) => {
                if let Some(mut parent_node) = tree.nodes.get(&parent).cloned() {
                    parent_node.children.retain(|child| *child != id);
                    tree.nodes = tree.nodes.insert(parent, parent_node).0;
                }
            }
            None => tree.root = None,
        }
        (tree, values)
    }
}

/// Iterative pre-order traversal.
pub struct Preorder<'a, N> {
    tree: &'a TreeStore<N>,
    stack: Vec<TreeNodeId>,
}

impl<N> std::fmt::Debug for Preorder<'_, N> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Preorder")
            .field("remaining", &self.stack.len())
            .finish()
    }
}

impl<'a, N> Iterator for Preorder<'a, N> {
    type Item = (TreeNodeId, &'a N);

    fn next(&mut self) -> Option<Self::Item> {
        let id = self.stack.pop()?;
        let node = self.tree.get(id)?;
        for child in node.children.iter().rev() {
            self.stack.push(*child);
        }
        Some((id, &node.value))
    }
}

/// One tree edit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeEdit<N> {
    /// Create a root.
    SetRoot {
        /// Root payload.
        value: N,
    },
    /// Insert a child.
    Insert {
        /// Parent identity.
        parent: TreeNodeId,
        /// Position among children.
        index: usize,
        /// Payload.
        value: N,
    },
    /// Remove a subtree.
    Remove {
        /// Subtree root.
        id: TreeNodeId,
    },
    /// Move a subtree.
    Move {
        /// Subtree root.
        id: TreeNodeId,
        /// New parent.
        parent: TreeNodeId,
        /// New position.
        index: usize,
    },
    /// Replace a payload.
    Replace {
        /// Node identity.
        id: TreeNodeId,
        /// New payload.
        value: N,
    },
}

/// An ordered script of tree edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeDelta<N> {
    edits: Vec<TreeEdit<N>>,
}

impl<N> Default for TreeDelta<N> {
    fn default() -> Self {
        Self { edits: Vec::new() }
    }
}

impl<N> TreeDelta<N> {
    /// The empty script.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a script from edits.
    #[must_use]
    pub fn from_edits(edits: Vec<TreeEdit<N>>) -> Self {
        Self { edits }
    }

    /// Appends an edit.
    pub fn push(&mut self, edit: TreeEdit<N>) {
        self.edits.push(edit);
    }
}

impl<N: Clone + Send + Sync + 'static> Delta for TreeDelta<N> {
    type Target = TreeStore<N>;

    fn empty() -> Self {
        Self::new()
    }

    fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }

    fn apply(&self, target: &Self::Target) -> Self::Target {
        let mut tree = target.clone();
        for edit in &self.edits {
            tree = match edit {
                TreeEdit::SetRoot { value } => tree.with_root(value.clone()).0,
                TreeEdit::Insert {
                    parent,
                    index,
                    value,
                } => tree.insert_child(*parent, *index, value.clone()).0,
                TreeEdit::Remove { id } => tree.remove_subtree(*id).0,
                TreeEdit::Move { id, parent, index } => {
                    tree.move_subtree(*id, *parent, *index).unwrap_or(tree)
                }
                TreeEdit::Replace { id, value } => tree.replace_value(*id, value.clone()),
            };
        }
        tree
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

    fn sample() -> (TreeStore<&'static str>, TreeNodeId, TreeNodeId) {
        let tree = TreeStore::new();
        let (tree, root) = tree.with_root("root");
        let (tree, first) = tree.append_child(root, "first");
        (tree, root, first)
    }

    #[test]
    fn child_insertion_and_preorder() {
        let (tree, root, _) = sample();
        let (tree, second) = tree.append_child(root, "second");
        let (tree, leaf) = tree.append_child(second, "leaf");

        assert_eq!(tree.len(), 4);
        assert_eq!(
            tree.preorder().map(|(_, value)| *value).collect::<Vec<_>>(),
            vec!["root", "first", "second", "leaf"]
        );
        assert_eq!(tree.parent(leaf), Some(second));
        assert_eq!(tree.depth(leaf), 2);
    }

    #[test]
    fn moving_a_subtree_preserves_identities() {
        let (tree, root, first) = sample();
        let (tree, second) = tree.append_child(root, "second");
        let (tree, leaf) = tree.append_child(first, "leaf");

        let moved = tree.move_subtree(first, second, 0).unwrap();
        assert_eq!(moved.parent(first), Some(second));
        assert_eq!(moved.parent(leaf), Some(first));
        assert_eq!(moved.children(second), Some(&[first][..]));
        assert!(moved.get(first).is_some());
        assert!(moved.get(leaf).is_some());
    }

    #[test]
    fn moving_into_own_subtree_is_rejected() {
        let (tree, _, first) = sample();
        let (tree, leaf) = tree.append_child(first, "leaf");
        let error = tree.move_subtree(first, leaf, 0).unwrap_err();
        assert!(matches!(error, TreeError::Cycle { .. }));
    }

    #[test]
    fn removal_returns_children_before_parents_are_retracted() {
        let (tree, root, first) = sample();
        let (tree, leaf) = tree.append_child(first, "leaf");
        let (tree, removed) = tree.remove_subtree(first);
        assert_eq!(
            removed.iter().map(|(_, value)| *value).collect::<Vec<_>>(),
            vec!["first", "leaf"]
        );
        assert_eq!(tree.len(), 1);
        assert_eq!(tree.children(root), Some(&[][..]));
        assert!(tree.get(leaf).is_none());
    }

    #[test]
    fn deep_trees_traverse_without_recursion() {
        let mut tree = TreeStore::new();
        let (next, mut current) = tree.with_root(0u32);
        tree = next;
        for depth in 1..10_000 {
            let (next, child) = tree.append_child(current, depth);
            tree = next;
            current = child;
        }
        assert_eq!(tree.len(), 10_000);
        assert_eq!(tree.preorder().count(), 10_000);
        assert_eq!(tree.depth(current), 9_999);
    }
}
