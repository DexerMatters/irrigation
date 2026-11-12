//! Mapping a grammar's productions onto a declared tree.
//!
//! A grammar says how a source is parsed; a `#[tree_view]` says what a tree looks
//! like. `@tree(Type::Variant)` on a production connects the two: this production
//! builds that variant, and each of the production's declared fields fills the
//! tree field of the same name.
//!
//! [`Grammar::map_tree`] resolves that connection once, against the tree's own
//! description ([`TreeLayout`]), and reports every mismatch instead of leaving a
//! hole that a reader would meet as a missing child. The result,
//! [`TreeMapping`], is what the parser publishes through.

use std::collections::BTreeSet;
use std::fmt;

use irrigation::{Cardinality, ChildLayout, LeafKind, TreeLayout, VariantLayout};

use crate::parser::GrammarFieldId;
use crate::parser::grammar::{FieldId, NonterminalId, Symbol};
use crate::parser::{Grammar, Production, ProductionId};

/// How one tree field is fed by one production field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feed {
    /// A scalar field, read from the single token at that position.
    Leaf {
        /// Leaf slot inside the variant.
        slot: u16,
        /// Storage kind the slot expects.
        kind: LeafKind,
    },
    /// A child field, filled from the rule nodes at that position.
    Child {
        /// Child slot inside the variant.
        slot: u16,
        /// How many children the slot holds.
        cardinality: Cardinality,
    },
}

/// One production's resolved build plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleBuild {
    /// Index of the declared type.
    pub type_index: u16,
    /// Index of the variant inside that type.
    pub variant: u16,
    /// The variant's declaration, for slot counts and order.
    pub variant_layout: &'static VariantLayout,
    /// One entry per tree field, in declaration order.
    pub feeds: Vec<(GrammarFieldId, Feed)>,
}

/// A resolved grammar-to-tree map.
///
/// Keyed by production, not by rule: two alternatives of one rule may build
/// different variants, and they publish the same rule identity.
#[derive(Clone, Debug)]
pub struct TreeMapping {
    builds: Vec<Option<RuleBuild>>,
    layout: &'static TreeLayout,
}

impl TreeMapping {
    /// The plan for one production, when it builds a declared node.
    #[must_use]
    pub fn build(&self, production: ProductionId) -> Option<&RuleBuild> {
        self.builds.get(production)?.as_ref()
    }

    /// The tree this map was resolved against.
    #[must_use]
    pub fn layout(&self) -> &'static TreeLayout {
        self.layout
    }

    /// How many productions build a declared node.
    #[must_use]
    pub fn len(&self) -> usize {
        self.builds.iter().filter(|build| build.is_some()).count()
    }

    /// Whether no production builds a declared node.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One thing a grammar cannot say to a declared tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeMappingError {
    /// `@tree` names a type the tree does not declare.
    UnknownType {
        /// The production's rule.
        rule: String,
        /// The type it names.
        ty: String,
    },
    /// `@tree` names a variant the type does not declare.
    UnknownVariant {
        /// The production's rule.
        rule: String,
        /// The type it names.
        ty: String,
        /// The variant it names.
        variant: String,
    },
    /// A production field names a field the variant does not declare.
    UnknownField {
        /// The production's rule.
        rule: String,
        /// The variant it builds.
        variant: String,
        /// The field it declares.
        field: String,
    },
    /// A declared tree field has no production field feeding it.
    MissingField {
        /// The production's rule.
        rule: String,
        /// The variant it builds.
        variant: String,
        /// The field nothing feeds.
        field: String,
    },
    /// A position feeds a field with the wrong number of children.
    Cardinality {
        /// The production's rule.
        rule: String,
        /// The field being fed.
        field: String,
        /// What the tree field expects.
        expected: Cardinality,
        /// What the position produces.
        found: &'static str,
    },
    /// A position feeds a field with the wrong kind of child.
    PositionKind {
        /// The production's rule.
        rule: String,
        /// The field being fed.
        field: String,
        /// What the tree field expects.
        expected: &'static str,
        /// What the position holds.
        found: &'static str,
    },
    /// A declared child can be produced by a rule that builds no declared node.
    UnmappedChildRule {
        /// The production's rule.
        rule: String,
        /// The field being fed.
        field: String,
        /// The rule that builds nothing.
        child_rule: String,
    },
    /// One production declares both `@tree` and `@wrap`.
    WrapAndTree {
        /// The production's rule.
        rule: String,
    },
    /// The start rule builds no node of the tree's root type.
    UnmappedRoot {
        /// The start rule.
        rule: String,
    },
}

impl fmt::Display for TreeMappingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownType { rule, ty } => write!(
                f,
                "`{rule}` builds `{ty}`, which the declared tree does not declare"
            ),
            Self::UnknownVariant { rule, ty, variant } => write!(
                f,
                "`{rule}` builds `{ty}::{variant}`, which `{ty}` does not declare"
            ),
            Self::UnknownField {
                rule,
                variant,
                field,
            } => write!(
                f,
                "`{rule}` declares field `{field}`, which `{variant}` does not declare"
            ),
            Self::MissingField {
                rule,
                variant,
                field,
            } => write!(
                f,
                "`{variant}` declares field `{field}`, which `{rule}` does not feed"
            ),
            Self::Cardinality {
                rule,
                field,
                expected,
                found,
            } => write!(
                f,
                "field `{field}` of `{rule}` expects {expected:?}, but its position produces {found}"
            ),
            Self::PositionKind {
                rule,
                field,
                expected,
                found,
            } => write!(
                f,
                "field `{field}` of `{rule}` expects {expected}, but its position holds {found}"
            ),
            Self::UnmappedChildRule {
                rule,
                field,
                child_rule,
            } => write!(
                f,
                "field `{field}` of `{rule}` can hold `{child_rule}`, which builds no declared node"
            ),
            Self::WrapAndTree { rule } => write!(
                f,
                "`{rule}` declares both `@tree` and `@wrap`: a wrapper builds no node, so it \
                 cannot also build one"
            ),
            Self::UnmappedRoot { rule } => write!(
                f,
                "the start rule `{rule}` builds no node of the tree's root type; give it a \
                 non-transparent production with `@tree(<Root>::<Variant>)`"
            ),
        }
    }
}

impl std::error::Error for TreeMappingError {}

/// What a position contributes to its parent's child list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PositionShape {
    /// A single token.
    Token,
    /// Exactly one rule node.
    Single,
    /// A group: the chosen alternative's symbols, one or more.
    Group,
    /// A repetition; `nullable` when it may match nothing.
    Repeat { nullable: bool },
}

impl PositionShape {
    fn describe(self) -> &'static str {
        match self {
            Self::Token => "a token",
            Self::Single => "one rule",
            Self::Group => "an alternative group",
            Self::Repeat { nullable: true } => "an optional repetition",
            Self::Repeat { nullable: false } => "a repetition",
        }
    }
}

impl Grammar {
    /// Resolves every `@tree` production against `layout`.
    ///
    /// # Errors
    ///
    /// Returns every mismatch at once, so one run lists all of them.
    pub fn map_tree(
        &self,
        layout: &'static TreeLayout,
    ) -> Result<TreeMapping, Vec<TreeMappingError>> {
        let mut errors = Vec::new();
        let mut builds: Vec<Option<RuleBuild>> = vec![None; self.productions().len()];
        for production in self.productions() {
            let rule = production_name(self, production);
            if production.tree.is_some() && production.wrap.is_some() {
                errors.push(TreeMappingError::WrapAndTree { rule });
                continue;
            }
            let Some(target) = &production.tree else {
                continue;
            };
            let Some(type_index) = layout.type_index(&target.ty) else {
                errors.push(TreeMappingError::UnknownType {
                    rule,
                    ty: target.ty.clone(),
                });
                continue;
            };
            let Some((variant, variant_layout)) = layout.variant(type_index, &target.variant)
            else {
                errors.push(TreeMappingError::UnknownVariant {
                    rule,
                    ty: target.ty.clone(),
                    variant: target.variant.clone(),
                });
                continue;
            };
            match self.build_production(production, variant_layout, &rule, &mut errors) {
                Some(feeds) => {
                    builds[production.id] = Some(RuleBuild {
                        type_index,
                        variant,
                        variant_layout,
                        feeds,
                    });
                }
                None => continue,
            }
            // Every declared field must be fed, and every declared child must be
            // producible: a hole here would reach a reader as a missing child.
            for leaf in variant_layout.leaves {
                if production_field(self, production, leaf.name).is_none() {
                    errors.push(TreeMappingError::MissingField {
                        rule: rule.clone(),
                        variant: variant_layout.name.to_string(),
                        field: leaf.name.to_string(),
                    });
                }
            }
            for child in variant_layout.children {
                let Some(field) = production_field(self, production, child.name) else {
                    errors.push(TreeMappingError::MissingField {
                        rule: rule.clone(),
                        variant: variant_layout.name.to_string(),
                        field: child.name.to_string(),
                    });
                    continue;
                };
                let Some(position) = production_position(production, field) else {
                    continue;
                };
                let Some(symbol) = production.rhs.get(position) else {
                    continue;
                };
                self.check_child_rules(symbol, &rule, child, &mut errors);
            }
        }

        let start = self.nonterminals()[self.start()].name.clone();
        let root_ok = self
            .productions()
            .iter()
            .filter(|production| production.lhs == self.start())
            .any(|production| {
                production
                    .tree
                    .as_ref()
                    .and_then(|target| layout.type_index(&target.ty))
                    == Some(layout.root)
                    && !production.transparent
            });
        if !root_ok {
            errors.push(TreeMappingError::UnmappedRoot { rule: start });
        }

        if errors.is_empty() {
            Ok(TreeMapping { builds, layout })
        } else {
            Err(errors)
        }
    }

    /// Resolves one production's fields into feeds, reporting mismatches.
    fn build_production(
        &self,
        production: &Production,
        variant: &'static VariantLayout,
        rule: &str,
        errors: &mut Vec<TreeMappingError>,
    ) -> Option<Vec<(GrammarFieldId, Feed)>> {
        let mut feeds = Vec::new();
        let mut failed = false;
        for (field, position) in &production.fields {
            let name = &self.fields()[*field].name;
            let Some(symbol) = production.rhs.get(*position) else {
                continue;
            };
            let shape = self.position_shape(symbol);
            let leaf = variant.leaf(name);
            let child = variant.child(name);
            match (leaf, child) {
                (Some(_), Some(_)) => {
                    errors.push(TreeMappingError::UnknownField {
                        rule: rule.to_string(),
                        variant: variant.name.to_string(),
                        field: name.clone(),
                    });
                    failed = true;
                }
                (None, None) => {
                    errors.push(TreeMappingError::UnknownField {
                        rule: rule.to_string(),
                        variant: variant.name.to_string(),
                        field: name.clone(),
                    });
                    failed = true;
                }
                (Some(leaf), None) => match shape {
                    PositionShape::Token => {
                        feeds.push((
                            *field,
                            Feed::Leaf {
                                slot: leaf.slot,
                                kind: leaf.kind,
                            },
                        ));
                    }
                    other => {
                        errors.push(TreeMappingError::PositionKind {
                            rule: rule.to_string(),
                            field: name.clone(),
                            expected: "a token",
                            found: other.describe(),
                        });
                        failed = true;
                    }
                },
                (None, Some(child)) => {
                    if shape == PositionShape::Token {
                        errors.push(TreeMappingError::PositionKind {
                            rule: rule.to_string(),
                            field: name.clone(),
                            expected: "a rule node",
                            found: "a token",
                        });
                        failed = true;
                    } else if Self::cardinality_matches(child, shape) {
                        feeds.push((
                            *field,
                            Feed::Child {
                                slot: child.slot,
                                cardinality: child.cardinality,
                            },
                        ));
                    } else {
                        errors.push(TreeMappingError::Cardinality {
                            rule: rule.to_string(),
                            field: name.clone(),
                            expected: child.cardinality,
                            found: shape.describe(),
                        });
                        failed = true;
                    }
                }
            }
        }
        if failed { None } else { Some(feeds) }
    }

    fn cardinality_matches(child: &ChildLayout, shape: PositionShape) -> bool {
        match child.cardinality {
            Cardinality::One => shape == PositionShape::Single,
            Cardinality::Optional => {
                matches!(shape, PositionShape::Repeat { nullable: true })
            }
            Cardinality::Many => matches!(
                shape,
                PositionShape::Group | PositionShape::Repeat { .. } | PositionShape::Single
            ),
        }
    }

    /// Every rule a declared child position can produce must build a node.
    fn check_child_rules(
        &self,
        symbol: &Symbol,
        rule: &str,
        child: &ChildLayout,
        errors: &mut Vec<TreeMappingError>,
    ) {
        let Symbol::Nonterminal(nonterminal) = symbol else {
            return;
        };
        for name in self.published_rules(*nonterminal) {
            let builds = self.productions().iter().any(|production| {
                self.published_rule_name(production) == name
                    && (production.tree.is_some() || production.wrap.is_some())
            });
            if !builds {
                errors.push(TreeMappingError::UnmappedChildRule {
                    rule: rule.to_string(),
                    field: child.name.to_string(),
                    child_rule: name,
                });
            }
        }
    }

    /// The names of the rules one nonterminal can publish, through transparent
    /// unit productions.
    #[must_use]
    pub fn published_rules(&self, nonterminal: NonterminalId) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let mut visited = BTreeSet::new();
        self.collect_rules(nonterminal, &mut out, &mut visited);
        out
    }

    fn collect_rules(
        &self,
        nonterminal: NonterminalId,
        out: &mut BTreeSet<String>,
        visited: &mut BTreeSet<NonterminalId>,
    ) {
        if !visited.insert(nonterminal) {
            return;
        }
        for production in self
            .productions()
            .iter()
            .filter(|production| production.lhs == nonterminal)
        {
            if production.transparent {
                if let Some(Symbol::Nonterminal(inner)) = production.rhs.first() {
                    self.collect_rules(*inner, out, visited);
                }
                continue;
            }
            out.insert(self.published_rule_name(production));
        }
    }

    /// The rule name one production publishes.
    #[must_use]
    pub fn published_rule_name(&self, production: &Production) -> String {
        production
            .ast_action
            .as_ref()
            .map_or_else(|| production.label.clone(), |action| action.name.clone())
    }

    /// What one position contributes.
    fn position_shape(&self, symbol: &Symbol) -> PositionShape {
        match symbol {
            Symbol::Terminal(_) => PositionShape::Token,
            Symbol::Nonterminal(nonterminal) => {
                let name = &self.nonterminals()[*nonterminal].name;
                if name.starts_with("$group") {
                    return PositionShape::Group;
                }
                if name.starts_with("$repeat") {
                    let productions: Vec<&Production> = self
                        .productions()
                        .iter()
                        .filter(|production| production.lhs == *nonterminal)
                        .collect();
                    let nullable = productions.iter().any(|p| p.rhs.is_empty());
                    return PositionShape::Repeat { nullable };
                }
                PositionShape::Single
            }
        }
    }
}

/// The name a production publishes, for error messages.
fn production_name(grammar: &Grammar, production: &Production) -> String {
    grammar.published_rule_name(production)
}

/// One production's field id by name, when it declares it.
fn production_field(grammar: &Grammar, production: &Production, name: &str) -> Option<FieldId> {
    production
        .fields
        .iter()
        .map(|(field, _)| *field)
        .find(|field| grammar.fields()[*field].name == name)
}

/// One production's position for one field.
fn production_position(production: &Production, field: FieldId) -> Option<usize> {
    production
        .fields
        .iter()
        .find(|(declared, _)| *declared == field)
        .map(|(_, position)| *position)
}
