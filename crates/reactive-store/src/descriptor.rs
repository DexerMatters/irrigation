//! View schemas and logical unit identities.
//!
//! A View is a typed namespace, not one heap object per element. Its descriptor
//! names the fields and the kind of each field; the runtime uses it for
//! registration and diagnostics. Logical units inside a View are addressed by
//! [`UnitKey`], which is the identity the dependency graph records.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use reactive_core::ids::{FieldId, ViewId};
use reactive_core::key::AnyKey;

/// Whether a View is populated by edits or by component definitions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ViewKind {
    /// Values enter through edit transactions.
    Source,
    /// Values are defined by component computations.
    Derived,
}

/// The shape of one View field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FieldKind {
    /// A single value, possibly a record decomposed into scalar units.
    Scalar,
    /// An ordered sequence with stable element identity.
    List,
    /// An unordered collection with membership.
    Set,
    /// A keyed collection.
    Map,
    /// Nodes and edges with adjacency indexes.
    Graph,
    /// An ownership tree of enum/record nodes.
    Tree,
}

/// Describes one field of a View.
#[derive(Clone, Debug)]
pub struct FieldDescriptor {
    /// Field name as written by the user.
    pub name: &'static str,
    /// Field shape.
    pub kind: FieldKind,
}

/// Operations on a View's immutable content.
///
/// The runtime stores content type-erased; generated code supplies these
/// function pointers so definitions, retractions, and reads never need a
/// downcast outside the View that owns the type.
#[derive(Clone, Copy, Debug)]
pub struct ContentVtable {
    /// Creates the empty content value.
    pub empty: fn() -> std::sync::Arc<dyn std::any::Any + Send + Sync>,
    /// Installs one unit into the content.
    pub define: fn(
        &std::sync::Arc<dyn std::any::Any + Send + Sync>,
        &UnitKey,
        reactive_core::value::Value,
    ) -> std::sync::Arc<dyn std::any::Any + Send + Sync>,
    /// Retracts one unit from the content.
    pub retract: fn(
        &std::sync::Arc<dyn std::any::Any + Send + Sync>,
        &UnitKey,
    ) -> std::sync::Arc<dyn std::any::Any + Send + Sync>,
    /// Reports whether a unit is present.
    pub contains: fn(
        &std::sync::Arc<dyn std::any::Any + Send + Sync>,
        &UnitKey,
    ) -> bool,
    /// Reads the current value of a unit out of the content.
    pub get: fn(
        &std::sync::Arc<dyn std::any::Any + Send + Sync>,
        &UnitKey,
    ) -> Option<reactive_core::value::Value>,
    /// Compares two content values semantically.
    pub eq: fn(
        &std::sync::Arc<dyn std::any::Any + Send + Sync>,
        &std::sync::Arc<dyn std::any::Any + Send + Sync>,
    ) -> bool,
    /// Formats the content for diagnostics.
    pub debug: fn(&std::sync::Arc<dyn std::any::Any + Send + Sync>) -> String,
}

/// Describes a registered View namespace.
#[derive(Clone, Debug)]
pub struct ViewDescriptor {
    /// Stable identity derived from the declaring path.
    pub id: ViewId,
    /// Fully qualified name, used in diagnostics.
    pub name: &'static str,
    /// Source or derived.
    pub kind: ViewKind,
    /// Declared fields in declaration order.
    pub fields: &'static [FieldDescriptor],
    /// Operations on the immutable content value.
    pub content: ContentVtable,
}

/// Which observation a unit represents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum UnitKind {
    /// A value: scalar field, element payload, or edge payload.
    Value,
    /// Presence of one entity (map key, set member, graph node, tree node).
    Membership,
    /// The ordered structure of a collection, used by iteration and length.
    Structure,
    /// A positional selector such as `list[i]`.
    Rank,
    /// An adjacency index such as `graph.neighbors(v)`.
    Adjacency,
    /// The View's whole immutable content, used by snapshots and iteration.
    Content,
}

/// Identity of one logical unit inside a View.
///
/// `entity` is `None` for singleton units (a scalar field, a collection's
/// structure) and `Some(key)` for dynamically discovered entities. The hash of
/// an entity only selects a bucket; equality of the concrete key decides
/// identity.
#[derive(Clone)]
pub struct UnitKey {
    /// The owning View.
    pub view: ViewId,
    /// The field within the View.
    pub field: FieldId,
    /// What is observed.
    pub kind: UnitKind,
    /// The entity within the field, if any.
    pub entity: Option<Arc<dyn AnyKey>>,
}

impl UnitKey {
    /// A singleton unit for one field.
    #[must_use]
    pub fn field(view: ViewId, field: FieldId, kind: UnitKind) -> Self {
        Self {
            view,
            field,
            kind,
            entity: None,
        }
    }

    /// A unit for one entity inside a field.
    #[must_use]
    pub fn entity(
        view: ViewId,
        field: FieldId,
        kind: UnitKind,
        entity: Arc<dyn AnyKey>,
    ) -> Self {
        Self {
            view,
            field,
            kind,
            entity: Some(entity),
        }
    }

    /// The concrete entity key, if this unit is entity-scoped.
    #[must_use]
    pub fn entity_key(&self) -> Option<&Arc<dyn AnyKey>> {
        self.entity.as_ref()
    }
}

impl PartialEq for UnitKey {
    fn eq(&self, other: &Self) -> bool {
        if self.view != other.view || self.field != other.field || self.kind != other.kind {
            return false;
        }
        match (&self.entity, &other.entity) {
            (None, None) => true,
            (Some(left), Some(right)) => left.key_eq(right.as_ref()),
            _ => false,
        }
    }
}

impl Eq for UnitKey {}

impl Hash for UnitKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.view.hash(state);
        self.field.hash(state);
        self.kind.hash(state);
        match &self.entity {
            None => state.write_u8(0),
            Some(entity) => {
                state.write_u8(1);
                state.write_u64(entity.key_hash());
            }
        }
    }
}

impl fmt::Debug for UnitKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnitKey")
            .field("view", &self.view)
            .field("field", &self.field)
            .field("kind", &self.kind)
            .field(
                "entity",
                &self.entity.as_ref().map(|key| key.key_display()),
            )
            .finish()
    }
}

/// Formats a unit for diagnostics.
#[must_use]
pub fn unit_label(view_name: &str, field_name: &str, key: &UnitKey) -> String {
    match &key.entity {
        Some(entity) => format!("{view_name}.{field_name}[{}]", entity.key_display()),
        None => format!("{view_name}.{field_name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reactive_core::key::any_key;

    #[test]
    fn unit_keys_distinguish_entities_and_kinds() {
        let view = ViewId(1);
        let field = FieldId(0);
        let a = UnitKey::entity(view, field, UnitKind::Value, any_key(1u32));
        let b = UnitKey::entity(view, field, UnitKind::Value, any_key(1u32));
        let c = UnitKey::entity(view, field, UnitKind::Value, any_key(2u32));
        let structure = UnitKey::field(view, field, UnitKind::Structure);

        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, structure);
    }

    #[test]
    fn unit_keys_with_different_types_are_distinct() {
        let view = ViewId(1);
        let field = FieldId(0);
        let int_key = UnitKey::entity(view, field, UnitKind::Membership, any_key(1u64));
        let str_key = UnitKey::entity(
            view,
            field,
            UnitKind::Membership,
            any_key(String::from("1")),
        );
        assert_ne!(int_key, str_key);
    }
}
