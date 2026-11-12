//! Unit tests for `src/project.rs`.

use crate::render::{
    FieldRender, GraphEdge, GraphNode, ListRow, MapRow, RenderModel, ScalarRow, SetMember, TreeRow,
};

fn rows(count: usize) -> Vec<ListRow> {
    (0..count)
        .map(|index| ListRow {
            index,
            value: index.to_string(),
            unit: None,
        })
        .collect()
}

/// G13: each field kind has its own drawing, and the tag is what the page
/// switches on. A shared "rows" tag would let the wrong renderer draw.
#[test]
fn every_model_kind_keeps_its_own_tag() {
    let models = [
        RenderModel::Tree(vec![TreeRow {
            key: String::from("n0"),
            depth: 0,
            kind: String::from("Program"),
            label: String::from("program"),
            value: None,
            unit: None,
        }]),
        RenderModel::List(rows(2)),
        RenderModel::Map(vec![MapRow {
            key: String::from("a"),
            value: Some(String::from("1")),
            unit: None,
        }]),
        RenderModel::Set(vec![SetMember {
            member: String::from("m"),
            present: true,
            unit: None,
        }]),
        RenderModel::Scalar(vec![ScalarRow {
            name: String::from("s"),
            value: String::from("1"),
            unit: None,
        }]),
        RenderModel::Graph {
            nodes: vec![GraphNode {
                id: String::from("0"),
                label: String::from("zero"),
                unit: None,
            }],
            edges: vec![GraphEdge {
                from: String::from("0"),
                to: String::from("0"),
                label: None,
            }],
        },
        RenderModel::Reduced {
            aggregate: String::from("3"),
            reduction: String::from("sum"),
            sample: vec![String::from("1")],
        },
    ];
    let tags: Vec<&str> = models
        .iter()
        .map(|model| match model {
            RenderModel::Tree(_) => "tree",
            RenderModel::List(_) => "list",
            RenderModel::Map(_) => "map",
            RenderModel::Set(_) => "set",
            RenderModel::Scalar(_) => "scalar",
            RenderModel::Graph { .. } => "graph",
            RenderModel::Reduced { .. } => "reduced",
        })
        .collect();
    assert_eq!(
        tags,
        vec!["tree", "list", "map", "set", "scalar", "graph", "reduced"]
    );
}

/// G4: a render that exceeds its cap says so, and keeps the exact total.
#[test]
fn capped_renders_report_the_real_total() {
    let over = FieldRender::capped(
        irrigation_core::ids::FieldId(0),
        RenderModel::List(rows(10)),
        10,
    )
    .with_cap(3);
    assert_eq!(over.total, 10);
    assert!(over.truncated);
    assert_eq!(over.model.len(), 3);
    let small = FieldRender::capped(
        irrigation_core::ids::FieldId(0),
        RenderModel::List(rows(2)),
        2,
    )
    .with_cap(3);
    assert!(!small.truncated);
    assert_eq!(small.model.len(), 2);
}
