//! Test-only constructors for a [`FieldRender`](crate::render::FieldRender).
//!
//! They live here rather than beside the type because they build render models
//! the production path never produces — an explicit pre-cap total, or a render
//! whose rows were capped at construction. `project`'s tests need both to state
//! what a truncation reports.

use crate::render::{FieldId, FieldRender, RenderModel};

impl FieldRender {
    /// Builds a render with an explicit pre-cap total.
    #[must_use]
    pub fn capped(field: FieldId, model: RenderModel, total: usize) -> Self {
        Self {
            field,
            truncated: total > model.len(),
            total,
            model,
        }
    }

    /// Applies a row cap, keeping an exact total.
    #[must_use]
    pub fn with_cap(mut self, cap: usize) -> Self {
        let total = self.total.max(self.model.len());
        match &mut self.model {
            RenderModel::Tree(rows) => rows.truncate(cap),
            RenderModel::List(rows) => rows.truncate(cap),
            RenderModel::Map(rows) => rows.truncate(cap),
            RenderModel::Set(members) => members.truncate(cap),
            RenderModel::Scalar(rows) => rows.truncate(cap),
            RenderModel::Reduced { sample, .. } => sample.truncate(cap.min(50)),
            RenderModel::Graph { nodes, edges } => {
                nodes.truncate(cap);
                let kept: std::collections::HashSet<&str> =
                    nodes.iter().map(|node| node.id.as_str()).collect();
                edges.retain(|edge| kept.contains(edge.from.as_str()));
                self.total = nodes.len();
            }
        }
        self.truncated = total > self.model.len() || self.truncated;
        self.total = total;
        self
    }
}
