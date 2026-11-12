import type { FieldWire, Model, ViewWire } from "../state/types";

/**
 * The words and counts a plate and the sidebar can both trust. They describe
 * the projection that is actually on screen, not the declared schema.
 */

export function kindWord(drawn: number, field: FieldWire | undefined): string {
  if (drawn > 1) return "fields";
  return field?.kind ?? "view";
}

export function countWord(
  field: FieldWire | undefined,
  modelKind: string | undefined,
  drawn: number,
): string {
  if (!field) return "—";
  if (modelKind === "reduced") return "aggregate";
  if (field.kind === "graph" || modelKind === "graph") {
    return drawn > 0 ? `${drawn} nodes` : "graph";
  }
  if (field.kind === "scalar" || modelKind === "scalar") {
    return drawn === 1 ? "1 value" : "value";
  }
  if (drawn > 0) return `${drawn} ${rowsNoun(modelKind, field)}`;
  if (field.granularity === "whole_value") {
    return field.elements === null ? "whole value" : `${field.elements} rows`;
  }
  return field.units === null ? "—" : `${field.units} units`;
}

export function rowsNoun(modelKind: string | undefined, field: FieldWire): string {
  if (modelKind === "graph" || field.kind === "graph") return "nodes";
  if (modelKind === "set") return "members";
  return "rows";
}

export type RowCounts = {
  drawn: number;
  changed: number;
  removed: number;
  identified: number;
  noun: string;
};

export function rowCounts(model: Model | undefined, field?: FieldWire): RowCounts {
  if (!model) return { drawn: 0, changed: 0, removed: 0, identified: 0, noun: "rows" };
  const states = (rows: { state: string; unit: unknown }[], noun = "rows") => ({
    drawn: rows.length,
    changed: rows.filter((row) => row.state === "changed").length,
    removed: rows.filter((row) => row.state === "removed").length,
    identified: rows.filter((row) => row.unit !== null).length,
    noun,
  });
  switch (model.kind) {
    case "tree":
    case "list":
    case "map":
      return states(model.rows, field ? rowsNoun(model.kind, field) : "rows");
    case "set":
      return states(model.members, "members");
    case "scalar":
      return states(model.rows, "rows");
    case "graph":
      return states(model.nodes, "nodes");
    default:
      return { drawn: 0, changed: 0, removed: 0, identified: 0, noun: "rows" };
  }
}

/** What a plate's footer can say about the field it is showing. */
export function fieldNote(
  counts: RowCounts,
  field: FieldWire | undefined,
  modelKind: string | undefined,
  fieldMoved: boolean,
  otherChange: number,
): string {
  if (counts.changed > 0) {
    return `${counts.changed} of ${counts.drawn} ${counts.noun}`;
  }
  if (counts.removed > 0) {
    return `${counts.removed} removed`;
  }
  const wholeValue =
    field?.granularity === "whole_value" ||
    field?.kind === "scalar" ||
    modelKind === "reduced" ||
    modelKind === "opaque";
  if (otherChange > 0 && !fieldMoved) {
    return `${otherChange} other field${otherChange === 1 ? "" : "s"}`;
  }
  if (wholeValue || counts.drawn === 0) return "whole value";
  if (fieldMoved) return `${field?.changed_units} units`;
  return "whole value";
}

/** Whether a View's declared field has moved at the displayed revision. */
export function movedField(wire: ViewWire, field: number): boolean {
  return (wire.fields.find((candidate) => candidate.field === field)?.changed_units ?? 0) > 0;
}
