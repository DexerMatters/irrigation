//! Incremental ownership trees.
//!
//! A tree node is an entity with stable identity. Its tag (which enum type and
//! which variant), each leaf field, each child slot, its parent relation, and
//! its membership are separate logical units, so:
//!
//! * reading a discriminant does not read any payload;
//! * changing one field does not invalidate readers of another field;
//! * moving a subtree keeps every node identity inside it;
//! * replacing a variant retracts removed projections and installs new ones.
//!
//! Refs work in two modes: live (tracked reads through the runtime) and
//! snapshot (pure reads from an immutable [`TreeStorage`]). The same typed
//! accessors serve both.

use std::any::Any;
use std::sync::Arc;

use reactive_core::descriptor::{ContentVtable, UnitKey, UnitKind};
use reactive_core::ids::{FieldId, ViewId};
use reactive_core::key::any_key;
use reactive_core::value::{Value, value_of};
use reactive_runtime::edit::Edit;
use reactive_runtime::helpers::read_unit_value;
use reactive_store::persistent::PMap;

/// Identity of one tree node.
pub type TreeNodeId = u64;

/// A leaf payload stored on a tree node.
#[derive(Clone, Debug, PartialEq)]
pub enum TreeValue {
    /// A unit value, used for variants with no payload.
    Unit,
    /// A boolean payload.
    Bool(bool),
    /// A signed integer payload.
    Int(i64),
    /// An unsigned integer payload.
    UInt(u64),
    /// A floating-point payload.
    Float(f64),
    /// A text payload.
    Text(String),
}

impl TreeValue {
    /// Returns the text payload, if this is text.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(value) => Some(value),
            _ => None,
        }
    }

    /// Returns the integer payload, if this is a signed integer.
    #[must_use]
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(value) => Some(*value),
            _ => None,
        }
    }

    /// Returns the unsigned payload, if this is an unsigned integer.
    #[must_use]
    pub fn as_uint(&self) -> Option<u64> {
        match self {
            Self::UInt(value) => Some(*value),
            _ => None,
        }
    }

    /// Returns the boolean payload, if this is a boolean.
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    /// Returns the floating-point payload, if this is a float.
    #[must_use]
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Self::Float(value) => Some(*value),
            _ => None,
        }
    }
}

/// The children of one child field of a node.
#[derive(Clone, Debug, PartialEq)]
pub enum ChildSlot {
    /// No child (a leaf field that is not a child).
    None,
    /// Exactly one child.
    One(TreeNodeId),
    /// An optional child.
    Optional(Option<TreeNodeId>),
    /// Zero or more children.
    Many(Vec<TreeNodeId>),
}

impl ChildSlot {
    /// Children in order.
    #[must_use]
    pub fn ids(&self) -> Vec<TreeNodeId> {
        match self {
            Self::None => Vec::new(),
            Self::One(id) => vec![*id],
            Self::Optional(Some(id)) => vec![*id],
            Self::Optional(None) => Vec::new(),
            Self::Many(ids) => ids.clone(),
        }
    }
}

/// One tree node.
#[derive(Clone, Debug, PartialEq)]
pub struct TreeNode {
    /// Which declared enum type this node is.
    pub type_index: u16,
    /// Which variant of that type.
    pub variant: u16,
    /// Leaf payloads in declaration order.
    pub fields: Vec<TreeValue>,
    /// Child fields in declaration order.
    pub children: Vec<ChildSlot>,
    /// Parent identity, `None` for the root.
    pub parent: Option<TreeNodeId>,
}

/// Immutable tree content.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TreeStorage {
    /// Nodes by identity.
    pub nodes: PMap<TreeNodeId, TreeNode>,
    /// Root identity.
    pub root: Option<TreeNodeId>,
    /// Next identity to allocate.
    pub next_id: TreeNodeId,
}

impl TreeStorage {
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
    pub fn node(&self, id: TreeNodeId) -> Option<&TreeNode> {
        self.nodes.get(&id)
    }

    /// Pre-order traversal from the root using an explicit stack.
    #[must_use]
    pub fn preorder(&self) -> Vec<TreeNodeId> {
        let mut order = Vec::new();
        let mut stack: Vec<TreeNodeId> = self.root.into_iter().collect();
        while let Some(id) = stack.pop() {
            let Some(node) = self.node(id) else {
                continue;
            };
            order.push(id);
            for slot in node.children.iter().rev() {
                for child in slot.ids().into_iter().rev() {
                    stack.push(child);
                }
            }
        }
        order
    }
}

/// A tree under construction.
#[derive(Clone, Debug, PartialEq)]
pub struct TreeBuild {
    /// Optional stable node identity.
    pub id: Option<TreeNodeId>,
    /// Which declared enum type this node is.
    pub type_index: u16,
    /// Which variant of that type.
    pub variant: u16,
    /// Leaf payloads in declaration order.
    pub fields: Vec<TreeValue>,
    /// Child fields in declaration order.
    pub children: Vec<BuildSlot>,
}

/// A child field under construction.
#[derive(Clone, Debug, PartialEq)]
pub enum BuildSlot {
    /// No child.
    None,
    /// Exactly one child.
    One(Box<TreeBuild>),
    /// An optional child.
    Optional(Option<Box<TreeBuild>>),
    /// Zero or more children.
    Many(Vec<TreeBuild>),
}

impl TreeBuild {
    /// Builds a node with no fields.
    #[must_use]
    pub fn leaf(type_index: u16, variant: u16) -> Self {
        Self {
            id: None,
            type_index,
            variant,
            fields: Vec::new(),
            children: Vec::new(),
        }
    }

    /// Adds a leaf payload.
    #[must_use]
    pub fn with_field(mut self, value: TreeValue) -> Self {
        self.fields.push(value);
        self
    }
}

/// Reserved unit slots for a tree View.
pub mod slots {
    /// The root identity.
    pub const ROOT: u16 = 0;
    /// `(type_index, variant)` of one node.
    pub const TAG: u16 = 1;
    /// One leaf payload of one node.
    pub const FIELD: u16 = 2;
    /// One child slot of one node.
    pub const CHILDREN: u16 = 3;
    /// The parent of one node.
    pub const PARENT: u16 = 4;
    /// Whether one node exists.
    pub const MEMBERSHIP: u16 = 5;
}

/// Unit keys a host renderer may cite when it wants per-node change marks.
///
/// A tree node's *tag* unit moves when the node's kind or variant changes; its
/// leaf and child units move independently. A renderer that shows a node row
/// cites the tag unit, and a renderer that shows a leaf row cites that leaf.
pub mod units {
    use reactive_core::descriptor::UnitKey;
    use reactive_core::ids::ViewId;

    use super::{
        TreeNodeId, children_unit, field_unit, membership_unit, parent_unit, root_unit, tag_unit,
    };

    /// The tree's root identity.
    #[must_use]
    pub fn root(view: ViewId) -> UnitKey {
        root_unit(view)
    }

    /// One node's kind and variant.
    #[must_use]
    pub fn tag(view: ViewId, node: TreeNodeId) -> UnitKey {
        tag_unit(view, node)
    }

    /// One node's leaf payload at `slot`.
    #[must_use]
    pub fn field(view: ViewId, node: TreeNodeId, slot: u16) -> UnitKey {
        field_unit(view, node, slot)
    }

    /// One node's child slot.
    #[must_use]
    pub fn children(view: ViewId, node: TreeNodeId, slot: u16) -> UnitKey {
        children_unit(view, node, slot)
    }

    /// One node's parent.
    #[must_use]
    pub fn parent(view: ViewId, node: TreeNodeId) -> UnitKey {
        parent_unit(view, node)
    }

    /// Whether one node exists.
    #[must_use]
    pub fn membership(view: ViewId, node: TreeNodeId) -> UnitKey {
        membership_unit(view, node)
    }
}

fn root_unit(view: ViewId) -> UnitKey {
    UnitKey::field(view, FieldId(slots::ROOT), UnitKind::Value)
}

fn tag_unit(view: ViewId, node: TreeNodeId) -> UnitKey {
    UnitKey::entity(view, FieldId(slots::TAG), UnitKind::Value, any_key(node))
}

fn field_unit(view: ViewId, node: TreeNodeId, slot: u16) -> UnitKey {
    UnitKey::entity(
        view,
        FieldId(slots::FIELD),
        UnitKind::Value,
        any_key((node, slot)),
    )
}

fn children_unit(view: ViewId, node: TreeNodeId, slot: u16) -> UnitKey {
    UnitKey::entity(
        view,
        FieldId(slots::CHILDREN),
        UnitKind::Structure,
        any_key((node, slot)),
    )
}

fn parent_unit(view: ViewId, node: TreeNodeId) -> UnitKey {
    UnitKey::entity(view, FieldId(slots::PARENT), UnitKind::Value, any_key(node))
}

fn membership_unit(view: ViewId, node: TreeNodeId) -> UnitKey {
    UnitKey::entity(
        view,
        FieldId(slots::MEMBERSHIP),
        UnitKind::Membership,
        any_key(node),
    )
}

/// How a ref reads the tree.
#[derive(Clone)]
pub enum TreeAccess {
    /// Tracked reads through the runtime.
    Live {
        /// Accessor from the View's content.
        content: fn(&dyn Any) -> TreeStorage,
    },
    /// Pure reads from immutable content.
    Snapshot(Arc<TreeStorage>),
}

impl std::fmt::Debug for TreeAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Live { .. } => f.write_str("TreeAccess::Live"),
            Self::Snapshot(_) => f.write_str("TreeAccess::Snapshot"),
        }
    }
}

/// An untyped tree node reference.
#[derive(Clone)]
pub struct TreeRef {
    view: ViewId,
    node: TreeNodeId,
    access: TreeAccess,
}

impl std::fmt::Debug for TreeRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TreeRef")
            .field("view", &self.view)
            .field("node", &self.node)
            .finish_non_exhaustive()
    }
}

impl PartialEq for TreeRef {
    fn eq(&self, other: &Self) -> bool {
        self.view == other.view && self.node == other.node
    }
}

impl Eq for TreeRef {}

impl std::hash::Hash for TreeRef {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.view.hash(state);
        self.node.hash(state);
    }
}

impl TreeRef {
    /// Builds a live ref.
    #[must_use]
    pub fn live(view: ViewId, node: TreeNodeId, content: fn(&dyn Any) -> TreeStorage) -> Self {
        Self {
            view,
            node,
            access: TreeAccess::Live { content },
        }
    }

    /// Builds a snapshot ref.
    #[must_use]
    pub fn snapshot(storage: Arc<TreeStorage>, node: TreeNodeId) -> Self {
        Self {
            view: ViewId(0),
            node,
            access: TreeAccess::Snapshot(storage),
        }
    }

    /// The node identity.
    #[must_use]
    pub fn id(&self) -> TreeNodeId {
        self.node
    }

    /// The owning View.
    #[must_use]
    pub fn view(&self) -> ViewId {
        self.view
    }

    /// Returns `true` when the node exists.
    #[must_use]
    pub fn exists(&self) -> bool {
        match &self.access {
            TreeAccess::Live { content } => {
                read_unit_value(membership_unit(self.view, self.node), |any| {
                    Some(content(any).node(self.node).is_some())
                })
            }
            TreeAccess::Snapshot(storage) => storage.node(self.node).is_some(),
        }
    }

    /// Reads the `(type_index, variant)` tag.
    #[must_use]
    pub fn tag(&self) -> Option<(u16, u16)> {
        match &self.access {
            TreeAccess::Live { content } => {
                if !self.exists() {
                    return None;
                }
                Some(read_unit_value(tag_unit(self.view, self.node), |any| {
                    content(any)
                        .node(self.node)
                        .map(|node| (node.type_index, node.variant))
                }))
            }
            TreeAccess::Snapshot(storage) => storage
                .node(self.node)
                .map(|node| (node.type_index, node.variant)),
        }
    }

    /// Reads one leaf payload.
    #[must_use]
    pub fn field(&self, slot: u16) -> Option<TreeValue> {
        match &self.access {
            TreeAccess::Live { content } => {
                if !self.exists() {
                    return None;
                }
                let value: TreeValue =
                    read_unit_value(field_unit(self.view, self.node, slot), |any| {
                        content(any)
                            .node(self.node)
                            .and_then(|node| node.fields.get(slot as usize).cloned())
                    });
                Some(value)
            }
            TreeAccess::Snapshot(storage) => storage
                .node(self.node)
                .and_then(|node| node.fields.get(slot as usize).cloned()),
        }
    }

    /// Reads one child slot.
    #[must_use]
    pub fn child_slot(&self, slot: u16) -> ChildSlot {
        match &self.access {
            TreeAccess::Live { content } => {
                if !self.exists() {
                    return ChildSlot::None;
                }
                read_unit_value(children_unit(self.view, self.node, slot), |any| {
                    content(any)
                        .node(self.node)
                        .and_then(|node| node.children.get(slot as usize).cloned())
                        .or(Some(ChildSlot::None))
                })
            }
            TreeAccess::Snapshot(storage) => storage
                .node(self.node)
                .and_then(|node| node.children.get(slot as usize).cloned())
                .unwrap_or(ChildSlot::None),
        }
    }

    /// Reads the parent relation.
    #[must_use]
    pub fn parent(&self) -> Option<TreeRef> {
        let parent = match &self.access {
            TreeAccess::Live { content } => {
                if !self.exists() {
                    return None;
                }
                let parent: Option<TreeNodeId> =
                    read_unit_value(parent_unit(self.view, self.node), |any| {
                        content(any).node(self.node).map(|node| node.parent)
                    });
                parent
            }
            TreeAccess::Snapshot(storage) => storage.node(self.node).and_then(|node| node.parent),
        }?;
        Some(Self {
            view: self.view,
            node: parent,
            access: self.access.clone(),
        })
    }

    /// Wraps a child identity with the same access mode.
    #[must_use]
    pub fn child(&self, node: TreeNodeId) -> Self {
        Self {
            view: self.view,
            node,
            access: self.access.clone(),
        }
    }
}

/// Transactional editor for a tree View.
/// Cost and reconciliation report for one whole-tree import or replacement.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TreeImportReport {
    /// Nodes created by the import.
    pub created: usize,
    /// Nodes whose identity was reused.
    pub reused: usize,
    /// Nodes whose variant changed and were replaced.
    pub replaced: usize,
    /// Duplicate-looking positions the positional rule could not distinguish.
    pub ambiguous: usize,
}

/// Transactional editor for a tree View.
pub struct TreeEditor<'a> {
    edit: &'a mut Edit,
    view: ViewId,
    storage: &'a mut TreeStorage,
    created: usize,
    reused: usize,
    replaced: usize,
}

impl std::fmt::Debug for TreeEditor<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TreeEditor").finish_non_exhaustive()
    }
}

impl<'a> TreeEditor<'a> {
    /// Builds an editor over a transaction and mutable content.
    pub fn new(edit: &'a mut Edit, view: ViewId, storage: &'a mut TreeStorage) -> Self {
        Self {
            edit,
            view,
            storage,
            created: 0,
            reused: 0,
            replaced: 0,
        }
    }

    /// Reports the cost and reconciliation choices of the last import.
    #[must_use]
    pub fn report(&self) -> TreeImportReport {
        TreeImportReport {
            created: self.created,
            reused: self.reused,
            replaced: self.replaced,
            ambiguous: 0,
        }
    }

    /// Replaces the whole tree, returning its root and an import report.
    pub fn set_root_report(&mut self, build: &TreeBuild) -> (TreeNodeId, TreeImportReport) {
        let root = self.set_root(build);
        (root, self.report())
    }

    /// Reconciles one subtree, returning its root and an import report.
    pub fn replace_report(
        &mut self,
        node: TreeNodeId,
        build: &TreeBuild,
    ) -> (TreeNodeId, TreeImportReport) {
        let root = self.replace(node, build);
        (root, self.report())
    }

    /// Replaces the whole tree, returning the new root identity.
    pub fn set_root(&mut self, build: &TreeBuild) -> TreeNodeId {
        let previous = self.storage.root;
        if let Some(previous) = previous {
            self.remove_subtree(previous);
        }
        let root = self.build(build, None);
        self.storage.root = Some(root);
        self.edit.set(root_unit(self.view), Some(root));
        root
    }

    /// Replaces one node's payload and children, preserving its identity.
    ///
    /// The old children are retracted. Unrelated nodes keep their identities.
    pub fn replace(&mut self, node: TreeNodeId, build: &TreeBuild) -> TreeNodeId {
        if self.storage.node(node).is_none() {
            return self.set_root(build);
        }
        self.apply(node, build);
        node
    }

    /// Applies a build to an existing node, reusing children whose variant is
    /// unchanged. The node keeps its identity; only changed units are staged.
    fn apply(&mut self, id: TreeNodeId, build: &TreeBuild) {
        let Some(existing) = self.storage.node(id).cloned() else {
            return;
        };
        self.edit
            .set(tag_unit(self.view, id), (build.type_index, build.variant));
        for (slot, value) in build.fields.iter().enumerate() {
            self.edit
                .set(field_unit(self.view, id, slot as u16), value.clone());
        }
        for slot in build.fields.len()..existing.fields.len() {
            self.edit
                .set(field_unit(self.view, id, slot as u16), TreeValue::Unit);
        }
        let mut children = Vec::with_capacity(build.children.len());
        for (slot, child) in build.children.iter().enumerate() {
            let old = existing
                .children
                .get(slot)
                .cloned()
                .unwrap_or(ChildSlot::None);
            children.push(self.reconcile(id, old, child));
        }
        for slot in build.children.len()..existing.children.len() {
            for child in existing.children[slot].ids() {
                self.remove_subtree(child);
            }
            self.edit
                .set(children_unit(self.view, id, slot as u16), ChildSlot::None);
        }
        let node = TreeNode {
            type_index: build.type_index,
            variant: build.variant,
            fields: build.fields.clone(),
            children: children.clone(),
            parent: existing.parent,
        };
        self.storage.nodes = self.storage.nodes.insert(id, node).0;
        for (slot, value) in children.iter().enumerate() {
            self.edit
                .set(children_unit(self.view, id, slot as u16), value.clone());
        }
    }

    fn reconcile(&mut self, parent: TreeNodeId, old: ChildSlot, build: &BuildSlot) -> ChildSlot {
        match build {
            BuildSlot::None => {
                self.drop_slot(old);
                ChildSlot::None
            }
            BuildSlot::Optional(None) => {
                self.drop_slot(old);
                ChildSlot::Optional(None)
            }
            BuildSlot::One(child) => {
                let olds = old.ids();
                let first = olds.first().copied();
                for extra in olds.iter().skip(1) {
                    self.remove_subtree(*extra);
                }
                ChildSlot::One(self.reuse_or_build(parent, first, child))
            }
            BuildSlot::Optional(Some(child)) => {
                let olds = old.ids();
                let first = olds.first().copied();
                for extra in olds.iter().skip(1) {
                    self.remove_subtree(*extra);
                }
                ChildSlot::Optional(Some(self.reuse_or_build(parent, first, child)))
            }
            BuildSlot::Many(builds) => {
                let olds = old.ids();
                let mut result = Vec::with_capacity(builds.len());
                for (index, child) in builds.iter().enumerate() {
                    match olds.get(index).copied() {
                        Some(old_id) if self.reusable(old_id, child) => {
                            self.reused += 1;
                            self.apply(old_id, child);
                            result.push(old_id);
                        }
                        old_id => {
                            if let Some(old_id) = old_id {
                                self.replaced += 1;
                                self.remove_subtree(old_id);
                            }
                            result.push(self.build(child, Some(parent)));
                        }
                    }
                }
                for id in olds.iter().skip(builds.len()) {
                    self.remove_subtree(*id);
                }
                ChildSlot::Many(result)
            }
        }
    }

    fn reuse_or_build(
        &mut self,
        parent: TreeNodeId,
        old: Option<TreeNodeId>,
        build: &TreeBuild,
    ) -> TreeNodeId {
        if let Some(old) = old {
            if self.reusable(old, build) {
                self.reused += 1;
                self.apply(old, build);
                return old;
            }
            self.replaced += 1;
            self.remove_subtree(old);
        }
        self.build(build, Some(parent))
    }

    fn reusable(&self, id: TreeNodeId, build: &TreeBuild) -> bool {
        self.storage.node(id).is_some_and(|node| {
            node.type_index == build.type_index && node.variant == build.variant
        })
    }

    fn drop_slot(&mut self, old: ChildSlot) {
        for child in old.ids() {
            self.remove_subtree(child);
        }
    }

    /// Removes a subtree.
    pub fn remove_subtree(&mut self, node: TreeNodeId) {
        let Some(existing) = self.storage.node(node).cloned() else {
            return;
        };
        let mut order = Vec::new();
        let mut stack = vec![node];
        while let Some(id) = stack.pop() {
            let Some(current) = self.storage.node(id).cloned() else {
                continue;
            };
            order.push(id);
            for slot in current.children.iter() {
                for child in slot.ids() {
                    stack.push(child);
                }
            }
        }
        for id in &order {
            self.clear_node_units(*id);
            self.storage.nodes = self.storage.nodes.remove(id).0;
        }
        if let Some(parent) = existing.parent {
            if let Some(mut parent_node) = self.storage.node(parent).cloned() {
                for slot in parent_node.children.iter_mut() {
                    match slot {
                        ChildSlot::One(id) if *id == node => *slot = ChildSlot::None,
                        ChildSlot::Optional(Some(id)) if *id == node => {
                            *slot = ChildSlot::Optional(None);
                        }
                        ChildSlot::Many(ids) => ids.retain(|id| *id != node),
                        _ => {}
                    }
                }
                self.storage.nodes = self.storage.nodes.insert(parent, parent_node).0;
                self.stage_children(parent);
            }
        } else {
            self.storage.root = None;
            self.edit
                .set(root_unit(self.view), Option::<TreeNodeId>::None);
        }
    }

    /// Moves a subtree under a new parent and slot position.
    ///
    /// Node identities inside the subtree are preserved; only parent and child
    /// observations change.
    pub fn move_subtree(
        &mut self,
        node: TreeNodeId,
        new_parent: TreeNodeId,
        slot: u16,
        index: usize,
    ) -> bool {
        let Some(moving) = self.storage.node(node).cloned() else {
            return false;
        };
        let Some(target) = self.storage.node(new_parent).cloned() else {
            return false;
        };
        if node == new_parent || self.is_descendant(new_parent, node) {
            return false;
        }
        if let Some(old_parent) = moving.parent {
            if let Some(mut parent_node) = self.storage.node(old_parent).cloned() {
                for existing in parent_node.children.iter_mut() {
                    match existing {
                        ChildSlot::One(id) if *id == node => *existing = ChildSlot::None,
                        ChildSlot::Optional(Some(id)) if *id == node => {
                            *existing = ChildSlot::Optional(None);
                        }
                        ChildSlot::Many(ids) => ids.retain(|id| *id != node),
                        _ => {}
                    }
                }
                self.storage.nodes = self.storage.nodes.insert(old_parent, parent_node).0;
                self.stage_children(old_parent);
            }
        }
        let mut target = target;
        let position = index.min(
            target
                .children
                .get(slot as usize)
                .map_or(0, |slot| slot.ids().len()),
        );
        match target.children.get_mut(slot as usize) {
            Some(ChildSlot::One(id)) => *id = node,
            Some(ChildSlot::Optional(current)) => *current = Some(node),
            Some(ChildSlot::Many(ids)) => {
                let position = position.min(ids.len());
                ids.insert(position, node);
            }
            Some(ChildSlot::None) | None => return false,
        }
        self.storage.nodes = self.storage.nodes.insert(new_parent, target).0;
        let mut moving = moving;
        moving.parent = Some(new_parent);
        self.storage.nodes = self.storage.nodes.insert(node, moving).0;
        self.edit
            .set(parent_unit(self.view, node), Some(new_parent));
        self.stage_children(new_parent);
        true
    }

    /// Returns `true` when `candidate` is a descendant of `ancestor`.
    #[must_use]
    pub fn is_descendant(&self, candidate: TreeNodeId, ancestor: TreeNodeId) -> bool {
        let mut current = self.storage.node(candidate).and_then(|node| node.parent);
        let mut guard = 0;
        while let Some(id) = current {
            if id == ancestor {
                return true;
            }
            guard += 1;
            if guard > self.storage.len() {
                return false;
            }
            current = self.storage.node(id).and_then(|node| node.parent);
        }
        false
    }

    fn build(&mut self, build: &TreeBuild, parent: Option<TreeNodeId>) -> TreeNodeId {
        self.created += 1;
        let id = self.storage.next_id;
        self.storage.next_id = id + 1;
        let node = TreeNode {
            type_index: build.type_index,
            variant: build.variant,
            fields: build.fields.clone(),
            children: vec![ChildSlot::None; build.children.len()],
            parent,
        };
        self.storage.nodes = self.storage.nodes.insert(id, node).0;
        self.edit.set(membership_unit(self.view, id), true);
        self.edit
            .set(tag_unit(self.view, id), (build.type_index, build.variant));
        self.edit.set(parent_unit(self.view, id), parent);
        for (slot, value) in build.fields.iter().enumerate() {
            self.edit
                .set(field_unit(self.view, id, slot as u16), value.clone());
        }
        for (slot, child) in build.children.iter().enumerate() {
            let slot_id = slot as u16;
            let built = match child {
                BuildSlot::None => ChildSlot::None,
                BuildSlot::One(child) => ChildSlot::One(self.build(child, Some(id))),
                BuildSlot::Optional(None) => ChildSlot::Optional(None),
                BuildSlot::Optional(Some(child)) => {
                    ChildSlot::Optional(Some(self.build(child, Some(id))))
                }
                BuildSlot::Many(children) => ChildSlot::Many(
                    children
                        .iter()
                        .map(|child| self.build(child, Some(id)))
                        .collect(),
                ),
            };
            if let Some(node) = self.storage.node(id).cloned() {
                let mut node = node;
                node.children[slot] = built.clone();
                self.storage.nodes = self.storage.nodes.insert(id, node).0;
            }
            self.edit.set(children_unit(self.view, id, slot_id), built);
        }
        id
    }

    fn clear_node_units(&mut self, id: TreeNodeId) {
        self.edit.set(membership_unit(self.view, id), false);
        if let Some(node) = self.storage.node(id).cloned() {
            for slot in 0..node.fields.len() {
                self.edit
                    .set(field_unit(self.view, id, slot as u16), TreeValue::Unit);
            }
            for slot in 0..node.children.len() {
                self.edit
                    .set(children_unit(self.view, id, slot as u16), ChildSlot::None);
            }
        }
        self.edit.set(tag_unit(self.view, id), (u16::MAX, u16::MAX));
        self.edit
            .set(parent_unit(self.view, id), Option::<TreeNodeId>::None);
    }

    fn stage_children(&mut self, id: TreeNodeId) {
        if let Some(node) = self.storage.node(id).cloned() {
            for (slot, children) in node.children.iter().enumerate() {
                self.edit
                    .set(children_unit(self.view, id, slot as u16), children.clone());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Content vtable
// ---------------------------------------------------------------------------

/// Creates empty tree content.
#[must_use]
pub fn tree_empty() -> Arc<dyn Any + Send + Sync> {
    Arc::new(TreeStorage::default())
}

/// Installs a unit into tree content.
#[must_use]
pub fn tree_define(
    content: &Arc<dyn Any + Send + Sync>,
    _key: &UnitKey,
    _value: Value,
) -> Arc<dyn Any + Send + Sync> {
    Arc::clone(content)
}

/// Retracts a unit from tree content.
#[must_use]
pub fn tree_retract(
    content: &Arc<dyn Any + Send + Sync>,
    _key: &UnitKey,
) -> Arc<dyn Any + Send + Sync> {
    Arc::clone(content)
}

/// Reports whether a unit is present in tree content.
#[must_use]
pub fn tree_contains(content: &Arc<dyn Any + Send + Sync>, key: &UnitKey) -> bool {
    let Some(storage) = content.downcast_ref::<TreeStorage>() else {
        return false;
    };
    match key.field.0 {
        slots::ROOT => storage.root.is_some(),
        slots::TAG | slots::PARENT => key
            .entity
            .as_deref()
            .and_then(|entity| entity.as_any().downcast_ref::<TreeNodeId>())
            .is_some_and(|node| storage.node(*node).is_some()),
        _ => false,
    }
}

/// Reads one unit out of tree content.
#[must_use]
pub fn tree_get(content: &Arc<dyn Any + Send + Sync>, key: &UnitKey) -> Option<Value> {
    let storage = content.downcast_ref::<TreeStorage>()?;
    match key.field.0 {
        slots::ROOT => Some(value_of(storage.root)),
        slots::TAG => {
            let node = key
                .entity
                .as_deref()?
                .as_any()
                .downcast_ref::<TreeNodeId>()?;
            storage
                .node(*node)
                .map(|node| value_of((node.type_index, node.variant)))
        }
        slots::FIELD => {
            let (node, slot) = key
                .entity
                .as_deref()?
                .as_any()
                .downcast_ref::<(TreeNodeId, u16)>()?;
            storage
                .node(*node)
                .and_then(|node| node.fields.get(*slot as usize).cloned())
                .map(value_of)
        }
        slots::CHILDREN => {
            let (node, slot) = key
                .entity
                .as_deref()?
                .as_any()
                .downcast_ref::<(TreeNodeId, u16)>()?;
            storage
                .node(*node)
                .and_then(|node| node.children.get(*slot as usize).cloned())
                .map(value_of)
        }
        slots::PARENT => {
            let node = key
                .entity
                .as_deref()?
                .as_any()
                .downcast_ref::<TreeNodeId>()?;
            storage.node(*node).map(|node| value_of(node.parent))
        }
        slots::MEMBERSHIP => {
            let node = key
                .entity
                .as_deref()?
                .as_any()
                .downcast_ref::<TreeNodeId>()?;
            Some(value_of(storage.node(*node).is_some()))
        }
        _ => None,
    }
}

/// Compares two tree contents.
#[must_use]
pub fn tree_eq(left: &Arc<dyn Any + Send + Sync>, right: &Arc<dyn Any + Send + Sync>) -> bool {
    left.downcast_ref::<TreeStorage>() == right.downcast_ref::<TreeStorage>()
}

/// Formats tree content for diagnostics.
#[must_use]
pub fn tree_debug(content: &Arc<dyn Any + Send + Sync>) -> String {
    match content.downcast_ref::<TreeStorage>() {
        Some(storage) => format!("tree({} nodes)", storage.len()),
        None => String::from("tree(?)"),
    }
}

/// The content operations for a tree View.
#[must_use]
pub const fn tree_content_vtable() -> ContentVtable {
    ContentVtable {
        empty: tree_empty,
        define: tree_define,
        retract: tree_retract,
        contains: tree_contains,
        get: tree_get,
        structure: tree_structure,
        len: tree_len,
        eq: tree_eq,
        debug: tree_debug,
        describe: |_| None,
        variant_label: |_, _| None,
    }
}

fn tree_structure(
    _content: &reactive_core::descriptor::Content,
    _field: reactive_core::ids::FieldId,
) -> Option<reactive_core::descriptor::Structure> {
    None
}

fn tree_len(
    content: &reactive_core::descriptor::Content,
    field: reactive_core::ids::FieldId,
) -> Option<usize> {
    // Only the tree field has nodes; other fields are scalars.
    if field.0 != 0 {
        return None;
    }
    content.downcast_ref::<TreeStorage>().map(TreeStorage::len)
}

/// The field layout reported by a tree View descriptor.
#[must_use]
pub const fn tree_fields() -> &'static [reactive_core::descriptor::FieldDescriptor] {
    &[
        reactive_core::descriptor::FieldDescriptor {
            name: "root",
            kind: reactive_core::descriptor::FieldKind::Tree,
        },
        reactive_core::descriptor::FieldDescriptor {
            name: "tag",
            kind: reactive_core::descriptor::FieldKind::Scalar,
        },
        reactive_core::descriptor::FieldDescriptor {
            name: "field",
            kind: reactive_core::descriptor::FieldKind::Scalar,
        },
        reactive_core::descriptor::FieldDescriptor {
            name: "children",
            kind: reactive_core::descriptor::FieldKind::List,
        },
        reactive_core::descriptor::FieldDescriptor {
            name: "parent",
            kind: reactive_core::descriptor::FieldKind::Scalar,
        },
        reactive_core::descriptor::FieldDescriptor {
            name: "membership",
            kind: reactive_core::descriptor::FieldKind::Set,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> TreeBuild {
        TreeBuild {
            id: None,
            type_index: 0,
            variant: 0,
            fields: vec![TreeValue::Text(String::from("root"))],
            children: vec![BuildSlot::Many(vec![
                TreeBuild::leaf(1, 0).with_field(TreeValue::Int(1)),
                TreeBuild {
                    id: None,
                    type_index: 1,
                    variant: 1,
                    fields: vec![TreeValue::Text(String::from("call"))],
                    children: vec![
                        BuildSlot::One(Box::new(
                            TreeBuild::leaf(2, 0).with_field(TreeValue::Text(String::from("x"))),
                        )),
                        BuildSlot::Many(vec![
                            TreeBuild::leaf(2, 1).with_field(TreeValue::Int(2)),
                            TreeBuild::leaf(2, 2).with_field(TreeValue::Int(3)),
                        ]),
                    ],
                },
            ])],
        }
    }

    fn insert(storage: &mut TreeStorage, id: TreeNodeId, node: TreeNode) {
        storage.nodes = storage.nodes.insert(id, node).0;
    }

    #[test]
    fn snapshot_refs_read_tags_fields_and_children() {
        // Build a storage by hand mirroring what the editor produces.
        let mut storage = TreeStorage::default();
        insert(
            &mut storage,
            0,
            TreeNode {
                type_index: 0,
                variant: 0,
                fields: vec![TreeValue::Text(String::from("root"))],
                children: vec![ChildSlot::Many(vec![1, 2])],
                parent: None,
            },
        );
        insert(
            &mut storage,
            1,
            TreeNode {
                type_index: 1,
                variant: 0,
                fields: vec![TreeValue::Int(1)],
                children: vec![],
                parent: Some(0),
            },
        );
        insert(
            &mut storage,
            2,
            TreeNode {
                type_index: 1,
                variant: 1,
                fields: vec![TreeValue::Text(String::from("call"))],
                children: vec![ChildSlot::One(3), ChildSlot::Many(vec![4, 5])],
                parent: Some(0),
            },
        );
        for (id, parent) in [(3, 2), (4, 2), (5, 2)] {
            insert(
                &mut storage,
                id,
                TreeNode {
                    type_index: 2,
                    variant: 0,
                    fields: vec![TreeValue::UInt(id)],
                    children: vec![],
                    parent: Some(parent),
                },
            );
        }
        storage.root = Some(0);
        storage.next_id = 6;

        let storage = Arc::new(storage);
        let root_ref = TreeRef::snapshot(Arc::clone(&storage), 0);
        assert_eq!(root_ref.tag(), Some((0, 0)));
        assert_eq!(
            root_ref
                .field(0)
                .and_then(|v| v.as_text().map(str::to_string)),
            Some(String::from("root"))
        );
        let children = root_ref.child_slot(0);
        assert_eq!(children.ids(), vec![1, 2]);
        let call = root_ref.child(2);
        assert_eq!(call.tag(), Some((1, 1)));
        assert_eq!(call.parent().map(|parent| parent.id()), Some(0));
        assert_eq!(call.child_slot(1).ids(), vec![4, 5]);
        assert_eq!(storage.preorder(), vec![0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn tree_build_construction_is_structural() {
        let build = sample();
        assert_eq!(build.children.len(), 1);
        match &build.children[0] {
            BuildSlot::Many(children) => {
                assert_eq!(children.len(), 2);
                assert_eq!(children[1].fields[0], TreeValue::Text(String::from("call")));
            }
            other => panic!("unexpected slot {other:?}"),
        }
    }

    #[test]
    fn vtable_round_trips_root_and_membership() {
        let content = tree_empty();
        let key = UnitKey::field(ViewId(1), FieldId(slots::ROOT), UnitKind::Value);
        assert!(tree_get(&content, &key).is_some());
        assert!(tree_eq(&content, &content));
        assert!(tree_debug(&content).contains("tree"));
    }
}

/// Extension for publishing a Tree View from a general component.
pub trait TreeOutputExt {
    /// Starts a Tree output transaction for `view`.
    fn tree_output(&mut self, view: ViewId) -> TreeOutputTxn<'_>;
}

impl TreeOutputExt for reactive_runtime::general::GeneralCx {
    fn tree_output(&mut self, view: ViewId) -> TreeOutputTxn<'_> {
        TreeOutputTxn { cx: self, view }
    }
}

/// One changed node in a local Tree patch.
#[derive(Clone, Debug, PartialEq)]
pub struct TreePatchNode {
    /// Stable node identity.
    pub id: TreeNodeId,
    /// Parent identity.
    pub parent: Option<TreeNodeId>,
    /// Declared enum type index.
    pub type_index: u16,
    /// Variant index.
    pub variant: u16,
    /// Leaf payloads.
    pub fields: Vec<TreeValue>,
    /// Child slots.
    pub children: Vec<ChildSlot>,
}

/// Local Tree patch. No whole-tree comparison is implied.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TreePatch {
    /// Root identity.
    pub root: Option<TreeNodeId>,
    /// Inserted or changed nodes.
    pub nodes: Vec<TreePatchNode>,
    /// Removed nodes; subtrees are removed with their roots.
    pub removed: Vec<TreeNodeId>,
}

/// Transactional Tree publisher for a GeneralComponent output View.
pub struct TreeOutputTxn<'a> {
    cx: &'a mut reactive_runtime::general::GeneralCx,
    view: ViewId,
}

impl std::fmt::Debug for TreeOutputTxn<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TreeOutputTxn").finish_non_exhaustive()
    }
}

impl TreeOutputTxn<'_> {
    /// Replaces the whole tree with `build` and stages all tree units.
    pub fn set_root(&mut self, build: &TreeBuild) -> TreeNodeId {
        let mut storage = TreeStorage::default();
        let root = build_tree_node(build, None, &mut storage);
        storage.root = Some(root);
        self.cx.set_content(self.view, Arc::new(storage.clone()));
        let mut output = self.cx.output();
        output.define(
            UnitKey::field(self.view, FieldId(slots::ROOT), UnitKind::Value),
            Some(root),
        );
        for (id, node) in storage.nodes.iter() {
            output.define(
                UnitKey::entity(
                    self.view,
                    FieldId(slots::MEMBERSHIP),
                    UnitKind::Membership,
                    any_key(*id),
                ),
                true,
            );
            output.define(
                UnitKey::entity(
                    self.view,
                    FieldId(slots::TAG),
                    UnitKind::Value,
                    any_key(*id),
                ),
                (node.type_index, node.variant),
            );
            output.define(
                UnitKey::entity(
                    self.view,
                    FieldId(slots::PARENT),
                    UnitKind::Value,
                    any_key(*id),
                ),
                node.parent,
            );
            for (slot, value) in node.fields.iter().enumerate() {
                output.define(
                    UnitKey::entity(
                        self.view,
                        FieldId(slots::FIELD),
                        UnitKind::Value,
                        any_key((*id, slot as u16)),
                    ),
                    value.clone(),
                );
            }
            for (slot, value) in node.children.iter().enumerate() {
                output.define(
                    UnitKey::entity(
                        self.view,
                        FieldId(slots::CHILDREN),
                        UnitKind::Structure,
                        any_key((*id, slot as u16)),
                    ),
                    value.clone(),
                );
            }
        }
        root
    }
}

impl TreeOutputTxn<'_> {
    /// Applies a local patch to `previous` content and stages only changed units.
    pub fn apply_patch(&mut self, previous: &TreeStorage, patch: &TreePatch) -> TreeStorage {
        let mut storage = previous.clone();
        for id in &patch.removed {
            // Patch removals are explicit node identities. Do not recurse into
            // children that may be reused by a replacement node.
            if let Some(node) = storage.nodes.get(id).cloned() {
                storage.nodes = storage.nodes.remove(id).0;
                let _ = node;
            }
        }
        for node in &patch.nodes {
            storage.nodes = storage
                .nodes
                .insert(
                    node.id,
                    TreeNode {
                        type_index: node.type_index,
                        variant: node.variant,
                        fields: node.fields.clone(),
                        children: node.children.clone(),
                        parent: node.parent,
                    },
                )
                .0;
            storage.next_id = storage.next_id.max(node.id.saturating_add(1));
        }
        storage.root = patch.root;
        self.cx.set_content(self.view, Arc::new(storage.clone()));
        let mut output = self.cx.output();
        output.define(
            UnitKey::field(self.view, FieldId(slots::ROOT), UnitKind::Value),
            patch.root,
        );
        for node in &patch.nodes {
            output.define(
                UnitKey::entity(
                    self.view,
                    FieldId(slots::MEMBERSHIP),
                    UnitKind::Membership,
                    any_key(node.id),
                ),
                true,
            );
            output.define(
                UnitKey::entity(
                    self.view,
                    FieldId(slots::TAG),
                    UnitKind::Value,
                    any_key(node.id),
                ),
                (node.type_index, node.variant),
            );
            output.define(
                UnitKey::entity(
                    self.view,
                    FieldId(slots::PARENT),
                    UnitKind::Value,
                    any_key(node.id),
                ),
                node.parent,
            );
            for (slot, value) in node.fields.iter().enumerate() {
                output.define(
                    UnitKey::entity(
                        self.view,
                        FieldId(slots::FIELD),
                        UnitKind::Value,
                        any_key((node.id, slot as u16)),
                    ),
                    value.clone(),
                );
            }
            for (slot, value) in node.children.iter().enumerate() {
                output.define(
                    UnitKey::entity(
                        self.view,
                        FieldId(slots::CHILDREN),
                        UnitKind::Structure,
                        any_key((node.id, slot as u16)),
                    ),
                    value.clone(),
                );
            }
        }
        for id in &patch.removed {
            output.define(
                UnitKey::entity(
                    self.view,
                    FieldId(slots::MEMBERSHIP),
                    UnitKind::Membership,
                    any_key(*id),
                ),
                false,
            );
        }
        storage
    }
}

fn build_tree_node(
    build: &TreeBuild,
    parent: Option<TreeNodeId>,
    storage: &mut TreeStorage,
) -> TreeNodeId {
    let id = build.id.unwrap_or(storage.next_id);
    storage.next_id = storage.next_id.max(id.saturating_add(1));
    let mut children = Vec::with_capacity(build.children.len());
    for child in &build.children {
        children.push(match child {
            BuildSlot::None => ChildSlot::None,
            BuildSlot::One(child) => ChildSlot::One(build_tree_node(child, Some(id), storage)),
            BuildSlot::Optional(None) => ChildSlot::Optional(None),
            BuildSlot::Optional(Some(child)) => {
                ChildSlot::Optional(Some(build_tree_node(child, Some(id), storage)))
            }
            BuildSlot::Many(children) => ChildSlot::Many(
                children
                    .iter()
                    .map(|child| build_tree_node(child, Some(id), storage))
                    .collect(),
            ),
        });
    }
    storage.nodes = storage
        .nodes
        .insert(
            id,
            TreeNode {
                type_index: build.type_index,
                variant: build.variant,
                fields: build.fields.clone(),
                children,
                parent,
            },
        )
        .0;
    id
}
