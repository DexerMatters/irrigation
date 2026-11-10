import { describe, expect, it } from "vitest";
import {
  COLLAPSED_HEIGHT,
  assignPorts,
  avoidOverlap,
  displayName,
  edgeKey,
  edgePath,
  layoutBoard,
  viewNode,
} from "./layout";
import type { ComponentWire, ViewRender, ViewWire } from "../state/types";

function view(key: string, name: string, rows: number): ViewWire {
  return {
    view: key,
    name,
    kind: "source",
    fields: [
      {
        field: 0,
        name: "field0",
        kind: "list",
        granularity: "whole_value",
        units: 1,
        elements: rows,
        content_version: 1,
        changed_units: 0,
      },
    ],
    producer: null,
    contributors: [],
    routing: 0,
    content_version: 1,
    demanded: true,
    changed_units: 0,
    editable: false,
    state: "clean",
  };
}

function render(key: string, rows: number): ViewRender {
  return {
    view: key,
    content_version: 1,
    revision: 1,
    inconsistent: false,
    fields: [
      {
        field: 0,
        total: rows,
        truncated: false,
        model: {
          kind: "list",
          rows: Array.from({ length: rows }, (_, index) => ({
            key: `i${index}`,
            index,
            value: String(index),
            unit: null,
            state: "display",
          })),
        },
      },
    ],
  };
}

const component: ComponentWire = {
  component: "c:1",
  name: "demo::resolve_name",
  kind: "general",
  reads: ["v:a"],
  writes: ["v:c"],
  executed_at: 1,
  suspect_at: 0,
};

describe("displayName", () => {
  it("turns a snake_case function into the type's Pascal name", () => {
    expect(displayName("resolve::resolve_program")).toBe("ResolveProgram");
    expect(displayName("irrigation_syntax::TextComponent")).toBe("TextComponent");
    expect(displayName("stlc_scope_graphs::pipeline::resolve_name")).toBe("ResolveName");
  });
});

describe("layoutBoard", () => {
  it("draws one top-down band per level, with one height per band", () => {
    const short = view("v:a", "demo::Short", 1);
    const long = view("v:c", "demo::Long", 3);
    const board = layoutBoard(
      [short, long],
      [],
      { "v:a": render("v:a", 1), "v:c": render("v:c", 3) },
      {},
    );
    const a = board.placement.get(viewNode("v:a"));
    const c = board.placement.get(viewNode("v:c"));
    expect(a?.y).toBe(c?.y);
    expect(board.sizes.get(viewNode("v:a"))?.height).toBe(
      board.sizes.get(viewNode("v:c"))?.height,
    );
  });

  it("gives every edge on one side its own port", () => {
    const source = view("v:a", "demo::Source", 1);
    const left = view("v:b", "demo::Left", 1);
    const right = view("v:c", "demo::Right", 1);
    const one: ComponentWire = {
      ...component,
      component: "c:one",
      name: "demo::One",
      reads: ["v:a"],
      writes: ["v:b"],
    };
    const two: ComponentWire = {
      ...component,
      component: "c:two",
      name: "demo::Two",
      reads: ["v:a"],
      writes: ["v:c"],
    };
    const board = layoutBoard(
      [source, left, right],
      [one, two],
      { "v:a": render("v:a", 1), "v:b": render("v:b", 1), "v:c": render("v:c", 1) },
      {},
    );
    const ports = assignPorts(board, board.placement);
    const first = ports.get(
      edgeKey({ from: viewNode("v:a"), to: "component:c:one", component: "c:one", kind: "reads", label: "One" }),
    );
    const second = ports.get(
      edgeKey({ from: viewNode("v:a"), to: "component:c:two", component: "c:two", kind: "reads", label: "Two" }),
    );
    expect(first?.start.x).not.toBe(second?.start.x);
    expect(first?.end.x).not.toBe(second?.end.x);
  });

  it("keeps a one-to-one chain on one vertical line", () => {
    const source = view("v:a", "demo::A", 1);
    const derived = view("v:c", "demo::C", 1);
    const middle: ComponentWire = {
      ...component,
      component: "c:middle",
      name: "demo::Middle",
      reads: ["v:a"],
      writes: ["v:c"],
    };
    const board = layoutBoard(
      [source, derived],
      [middle],
      { "v:a": render("v:a", 1), "v:c": render("v:c", 1) },
      {},
    );
    const centre = (id: string) => {
      const at = board.placement.get(id);
      const size = board.sizes.get(id);
      return at && size ? at.x + size.width / 2 : 0;
    };
    expect(centre("view:v:c")).toBe(centre("component:c:middle"));
    expect(centre("component:c:middle")).toBe(centre("view:v:a"));
  });

  it("draws a relation as one cubic, not a routed series of turns", () => {
    const source = view("v:a", "demo::A", 1);
    const derived = view("v:c", "demo::C", 1);
    const middle: ComponentWire = {
      ...component,
      component: "c:middle",
      name: "demo::Middle",
      reads: ["v:a"],
      writes: ["v:c"],
    };
    const board = layoutBoard(
      [source, derived],
      [middle],
      { "v:a": render("v:a", 1), "v:c": render("v:c", 1) },
      {},
    );
    const edge = {
      from: viewNode("v:a"),
      to: "component:c:middle",
      component: "c:middle",
      kind: "reads" as const,
      label: "Middle",
    };
    const ports = assignPorts(board, board.placement);
    const path = edgePath(edge, board.placement, board.sizes, { ends: ports.get(edgeKey(edge)) });
    expect(path?.match(/Q/g)).toBeNull();
    expect(path?.match(/C/g)?.length).toBe(1);
  });

  it("pushes a dropped plate out of an occupied rectangle", () => {
    const sizes = new Map([
      ["a", { width: 316, height: 104 }],
      ["b", { width: 316, height: 104 }],
    ]);
    const placement = new Map([
      ["a", { x: 0, y: 0 }],
      ["b", { x: 100, y: 0 }],
    ]);
    const resolved = avoidOverlap("b", { x: 100, y: 0 }, sizes, placement);
    const a = { x: 0, y: 0, width: 316, height: 104 };
    const b = { ...resolved, width: 316, height: 104 };
    const overlap =
      b.x < a.x + a.width && b.x + b.width > a.x &&
      b.y < a.y + a.height && b.y + b.height > a.y;
    expect(overlap).toBe(false);
  });

  it("puts a component between the Views it reads and defines", () => {
    const source = view("v:a", "demo::Source", 1);
    const derived = view("v:c", "demo::Derived", 1);
    const board = layoutBoard(
      [source, derived],
      [component],
      { "v:a": render("v:a", 1), "v:c": render("v:c", 1) },
      {},
    );
    const a = board.placement.get(viewNode("v:a"));
    const middle = board.placement.get("component:c:1");
    const c = board.placement.get(viewNode("v:c"));
    expect(middle && a ? middle.y > a.y : false).toBe(true);
    expect(middle && c ? c.y > middle.y : false).toBe(true);
  });

  it("folds a plate to its header without changing its width", () => {
    const source = view("v:a", "demo::Source", 3);
    const open = layoutBoard([source], [], { "v:a": render("v:a", 3) }, {});
    const folded = layoutBoard([source], [], { "v:a": render("v:a", 3) }, { "v:a": true });
    expect(folded.sizes.get(viewNode("v:a"))?.height).toBe(COLLAPSED_HEIGHT);
    expect(folded.sizes.get(viewNode("v:a"))?.width).toBe(
      open.sizes.get(viewNode("v:a"))?.width,
    );
  });
});
