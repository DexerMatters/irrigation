//! View schemas and logical unit identities.
//!
//! A View is a typed namespace, not one heap object per element. Its descriptor
//! names the fields and the kind of each field; the runtime uses it for
//! registration and diagnostics. Logical units inside a View are addressed by
//! [`UnitKey`], which is the identity the dependency graph records.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::ids::{FieldId, ViewId};
use crate::key::AnyKey;

/// An immutable, type-erased View content value.
pub type Content = Arc<dyn std::any::Any + Send + Sync>;

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

/// A canonical structure projection of a collection field.
///
/// A map's structure is its key set, a list's is its element-id sequence, and a
/// set's is its member set. The projection lets the runtime maintain structure
/// units from committed content without knowing the concrete field type.
#[derive(Clone)]
pub struct Structure {
    /// The structure value (for example `PMap<K, ()>` for a map).
    pub value: crate::value::Value,
    /// Equality for cutoff on this projection.
    pub eq: fn(&crate::value::Value, &crate::value::Value) -> bool,
}

impl fmt::Debug for Structure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Structure")
    }
}

/// Operations on a View's immutable content.
///
/// The runtime stores content type-erased; generated code supplies these
/// function pointers so definitions, retractions, and reads never need a
/// downcast outside the View that owns the type.
#[derive(Clone, Copy, Debug)]
pub struct ContentVtable {
    /// Creates the empty content value.
    pub empty: fn() -> Content,
    /// Installs one unit into the content.
    pub define: fn(&Content, &UnitKey, crate::value::Value) -> Content,
    /// Installs a batch of units into the content.
    ///
    /// The default family path is the same unit operation repeated against one
    /// content clone; a family may override it with a true batch later.
    pub define_many: fn(&Content, &[(UnitKey, crate::value::Value)]) -> Content,
    /// Retracts one unit from the content.
    pub retract: fn(&Content, &UnitKey) -> Content,
    /// Reports whether a unit is present.
    pub contains: fn(&Content, &UnitKey) -> bool,
    /// Reads the current value of a unit out of the content.
    pub get: fn(&Content, &UnitKey) -> Option<crate::value::Value>,
    /// Reads a field's whole storage.
    ///
    /// A whole-field definition is installed under the entity-less `Value`
    /// unit, whose payload is the family's storage, while `get` answers the
    /// same field's `Structure` unit with its projection. The two payloads
    /// have different types, so the retraction that restores a whole-field
    /// definition reads the storage here instead of through the unit getter.
    pub field_storage: fn(&Content, FieldId) -> Option<crate::value::Value>,
    /// Projects a field's structure, when the family has one.
    pub structure: fn(&Content, FieldId) -> Option<Structure>,
    /// Counts a field's display elements, when that is cheap.
    pub len: fn(&Content, FieldId) -> Option<usize>,
    /// Whether one unit changes its field's content value.
    pub moves_content: fn(&UnitKey) -> bool,
    /// Compares the changed units between two contents; `None` falls back to
    /// whole-content equality.
    pub changed_units_equal: fn(&Content, &Content, &[UnitKey]) -> Option<bool>,
    /// Projects a whole-field summary value, when the family has one.
    ///
    /// A reduction publishes its aggregate here; the generic projection spells
    /// it through `describe`. `None` means the field has no summary.
    pub summary: fn(&Content, FieldId) -> Option<crate::value::Value>,
    /// Compares two content values semantically.
    pub eq: fn(&Content, &Content) -> bool,
    /// Whether two storage values of one field hold the same answer.
    ///
    /// A retraction installs the field's reverted storage, and this is how it
    /// knows whether that is already what the cell holds. A family that cannot
    /// compare its storage answers `false`, which publishes -- the safe
    /// direction.
    pub field_equal: fn(FieldId, &crate::value::Value, &crate::value::Value) -> bool,
    /// Spells one element value for the board, when the View's types can.
    ///
    /// A projection that draws a View without a host renderer calls this with
    /// the unit's value; `None` means the projection asked a value it cannot
    /// spell (a tree's own kinds, a View that opted out) and the row should show
    /// identity only.
    pub describe: fn(&crate::value::Value) -> Option<String>,
    /// Names a tree node's declared variant, for a drawing with no renderer.
    ///
    /// Only tree Views fill this in; anything else reports `None` and the
    /// projection falls back to the type index it has.
    pub variant_label: fn(u32, u16) -> Option<&'static str>,
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
    /// An incoming adjacency index such as `graph.incoming(v)`.
    IncomingAdjacency,
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
    pub fn entity(view: ViewId, field: FieldId, kind: UnitKind, entity: Arc<dyn AnyKey>) -> Self {
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
            .field("entity", &self.entity.as_ref().map(|key| key.key_display()))
            .finish()
    }
}

/// The scope one writer claims.
///
/// A claim is acquired before a definition is staged and released when the
/// writer commits or fails. Two live claims conflict when their scopes overlap;
/// a field or View scope covers every unit underneath it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum WriteScope {
    /// One entity's ownership pair (membership and value).
    Entity(UnitKey),
    /// Every unit in one field.
    Field(ViewId, FieldId),
    /// Every unit in one View.
    View(ViewId),
}

impl WriteScope {
    /// Normalizes a unit to the claim identity of its owner pair.
    #[must_use]
    pub fn entity(unit: &UnitKey) -> Self {
        if unit.entity.is_some() {
            Self::Entity(UnitKey {
                kind: UnitKind::Membership,
                ..unit.clone()
            })
        } else {
            Self::Entity(unit.clone())
        }
    }

    /// Returns `true` when this scope covers `unit`.
    ///
    /// An entity claim owns every kind of unit for that entity: the ownership
    /// pair, not the observation, is the claim identity. A singleton claim
    /// names exactly one `(view, field, kind)` slot.
    #[must_use]
    pub fn covers(&self, unit: &UnitKey) -> bool {
        match self {
            Self::Entity(own) => {
                own.view == unit.view && own.field == unit.field && same_claim_target(own, unit)
            }
            Self::Field(view, field) => *view == unit.view && *field == unit.field,
            Self::View(view) => *view == unit.view,
        }
    }

    /// Returns `true` when the two scopes overlap.
    ///
    /// Scopes overlap exactly when some unit is covered by both.
    #[must_use]
    pub fn overlaps(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Entity(left), Self::Entity(right)) => {
                left.view == right.view
                    && left.field == right.field
                    && same_claim_target(left, right)
            }
            (Self::Entity(unit), Self::Field(view, field))
            | (Self::Field(view, field), Self::Entity(unit)) => {
                unit.view == *view && unit.field == *field
            }
            (Self::Entity(unit), Self::View(view)) | (Self::View(view), Self::Entity(unit)) => {
                unit.view == *view
            }
            (Self::Field(left_view, left_field), Self::Field(right_view, right_field)) => {
                left_view == right_view && left_field == right_field
            }
            (Self::Field(view, _), Self::View(other))
            | (Self::View(other), Self::Field(view, _)) => view == other,
            (Self::View(left), Self::View(right)) => left == right,
        }
    }

    /// Human-readable description for diagnostics.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Entity(unit) => format!("{unit:?}"),
            Self::Field(view, field) => format!("field {field:?} of view {view:?}"),
            Self::View(view) => format!("view {view:?}"),
        }
    }
}

/// Compares the writable target of two unit identities.
///
/// Entity-scoped units of one entity share one ownership pair, so the entity
/// decides identity and the observation kind does not. A singleton unit is one
/// exact `(view, field, kind)` slot, so its kind does.
fn same_claim_target(left: &UnitKey, right: &UnitKey) -> bool {
    match (&left.entity, &right.entity) {
        (Some(left), Some(right)) => left.key_eq(right.as_ref()),
        (None, None) => left.kind == right.kind,
        _ => false,
    }
}

#[cfg(test)]
#[path = "../tests/unit/descriptor/tests.rs"]
mod tests;
