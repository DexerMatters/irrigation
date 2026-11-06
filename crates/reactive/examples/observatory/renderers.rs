//! Native renderers for the demo Views.
//!
//! Each renderer reads one committed snapshot and returns display rows. Rows
//! cite the tree node's tag unit so the page can show which node changed; the
//! leaf values are display text, not units.

use reactive::{Snapshot, ViewSchema, tree_units};
use reactive_observe::{RenderModel, TreeRow, UnitRef, ValuePreview};

use crate::demo::{Diagnostics, Symbols, Total, Weights, ast};

/// Renders the syntax tree as an indented outline.
pub fn syntax(snapshot: &Snapshot) -> RenderModel {
    let view = snapshot.view::<ast::Syntax>();
    let mut rows = Vec::new();
    let Some(document) = view.root() else {
        return RenderModel::Tree(rows);
    };
    push_node(&mut rows, 0, "Program", "document", document.raw());
    for (index, statement) in document.statements().iter().enumerate() {
        let kind = statement.kind();
        let kind_label = if kind == Some(ast::StatementKind::Assign) {
            "Assign"
        } else if kind == Some(ast::StatementKind::Print) {
            "Print"
        } else {
            "statement"
        };
        let label = match statement.name() {
            Some(name) => format!("statement[{index}] {name} := …"),
            None => format!("statement[{index}] print"),
        };
        push_node(&mut rows, 1, kind_label, &label, statement.raw());
        if kind == Some(ast::StatementKind::Assign) {
            if let Some(value) = statement.value() {
                push_expression(&mut rows, 2, &value);
            }
        } else if kind == Some(ast::StatementKind::Print) {
            for value in statement.values() {
                push_expression(&mut rows, 2, &value);
            }
        }
    }
    RenderModel::Tree(rows)
}

fn push_expression(rows: &mut Vec<TreeRow>, depth: u16, expression: &ast::ExpressionRef) {
    let kind = expression.kind();
    if kind == Some(ast::ExpressionKind::Number) {
        rows.push(TreeRow {
            key: format!("e{}", expression.raw()),
            depth,
            kind: String::from("Number"),
            label: format!("{}", expression.number().unwrap_or_default()),
            value: None,
            unit: None,
        });
    } else if kind == Some(ast::ExpressionKind::Name) {
        rows.push(TreeRow {
            key: format!("e{}", expression.raw()),
            depth,
            kind: String::from("Name"),
            label: expression.name().unwrap_or_default(),
            value: None,
            unit: None,
        });
    } else if kind == Some(ast::ExpressionKind::Call) {
        push_node(
            rows,
            depth,
            "Call",
            &expression.callee().unwrap_or_default(),
            expression.raw(),
        );
        for arg in expression.args() {
            push_expression(rows, depth + 1, &arg);
        }
    }
}

/// Appends one tree node row that cites its tag unit.
fn push_node(rows: &mut Vec<TreeRow>, depth: u16, kind: &str, label: &str, raw: u64) {
    rows.push(TreeRow {
        key: format!("n{raw}"),
        depth,
        kind: kind.to_owned(),
        label: label.to_owned(),
        value: None,
        // The row cites the node's tag unit: this row changed when the node's
        // kind did. Leaf values are display text and carry no unit.
        unit: Some(UnitRef::from_key(&tree_units::tag(
            ast::Syntax::view_id(),
            raw,
        ))),
    });
}

/// Renders the symbol table as rows.
pub fn symbols(snapshot: &Snapshot) -> RenderModel {
    let symbols = snapshot.view::<Symbols>().field0();
    RenderModel::List(
        symbols
            .into_iter()
            .enumerate()
            .map(|(index, symbol)| reactive_observe::ListRow {
                index,
                value: symbol.name,
                unit: None,
            })
            .collect(),
    )
}

/// Renders diagnostics as rows.
pub fn diagnostics(snapshot: &Snapshot) -> RenderModel {
    let diagnostics = snapshot.view::<Diagnostics>().field0();
    RenderModel::List(
        diagnostics
            .into_iter()
            .enumerate()
            .map(|(index, message)| reactive_observe::ListRow {
                index,
                value: message,
                unit: None,
            })
            .collect(),
    )
}

/// Renders the weights as rows.
pub fn weights(snapshot: &Snapshot) -> RenderModel {
    RenderModel::List(
        snapshot
            .view::<Weights>()
            .field0()
            .into_iter()
            .enumerate()
            .map(|(index, value)| reactive_observe::ListRow {
                index,
                value: value.to_string(),
                unit: None,
            })
            .collect(),
    )
}

/// Renders the weighted total.
pub fn total(snapshot: &Snapshot) -> RenderModel {
    let view = snapshot.view::<Total>();
    let values = view.field0();
    RenderModel::Reduced {
        aggregate: view.field0_aggregate().to_string(),
        reduction: String::from("sum"),
        sample: values
            .into_iter()
            .take(4)
            .map(|value| value.to_string())
            .collect(),
    }
}

/// A short preview for the weights in the inspector.
pub fn weights_preview(snapshot: &Snapshot) -> ValuePreview {
    let values = snapshot.view::<Weights>().field0();
    ValuePreview {
        kind: String::from("List<u64>"),
        summary: format!("{} weights", values.len()),
        detail: Some(
            values
                .iter()
                .map(|value| value.to_string())
                .collect::<Vec<_>>()
                .join(", "),
        ),
        bytes: 0,
        truncated: false,
    }
}

/// A short preview for the symbol table in the inspector.
pub fn symbols_preview(snapshot: &Snapshot) -> ValuePreview {
    let symbols = snapshot.view::<Symbols>().field0();
    ValuePreview {
        kind: String::from("List<Symbol>"),
        summary: format!("{} symbols", symbols.len()),
        detail: Some(
            symbols
                .iter()
                .map(|symbol| symbol.name.clone())
                .collect::<Vec<_>>()
                .join(", "),
        ),
        bytes: 0,
        truncated: false,
    }
}

/// A short preview for diagnostics in the inspector.
pub fn diagnostics_preview(snapshot: &Snapshot) -> ValuePreview {
    let diagnostics = snapshot.view::<Diagnostics>().field0();
    ValuePreview {
        kind: String::from("List<String>"),
        summary: format!("{} diagnostics", diagnostics.len()),
        detail: diagnostics.first().cloned(),
        bytes: 0,
        truncated: false,
    }
}
