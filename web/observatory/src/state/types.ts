export type RowState = "changed" | "clean" | "suspect" | "display" | "removed";

export type UnitWire = {
  view: string;
  field: number;
  kind: string;
  entity: string | null;
  changed_at: number | null;
};

type RowBase = {
  key: string;
  unit: UnitWire | null;
  state: RowState;
};

export type TreeRow = RowBase & {
  depth: number;
  kind: string;
  label: string;
  value: string | null;
};

export type ListRow = RowBase & { index: number; value: string };
export type MapRow = RowBase & { label: string; value: string | null };
export type SetMember = RowBase & { member: string; present: boolean };
export type ScalarRow = RowBase & { label: string; value: string };

export type GraphNode = { id: string; label: string; unit: UnitWire | null; state: RowState };
export type GraphEdge = { from: string; to: string; label: string | null };

export type Model =
  | { kind: "tree"; rows: TreeRow[] }
  | { kind: "list"; rows: ListRow[] }
  | { kind: "map"; rows: MapRow[] }
  | { kind: "set"; members: SetMember[] }
  | { kind: "graph"; nodes: GraphNode[]; edges: GraphEdge[] }
  | { kind: "scalar"; rows: ScalarRow[] }
  | { kind: "reduced"; aggregate: string; reduction: string; sample: string[] }
  | { kind: "opaque"; type_name: string; summary: string };

export type FieldRender = {
  field: number;
  model: Model;
  total: number;
  truncated: boolean;
};

export type ViewRender = {
  view: string;
  content_version: number;
  revision: number;
  source: string;
  inconsistent: boolean;
  fields: FieldRender[];
};

export type FieldWire = {
  field: number;
  name: string;
  kind: string;
  granularity: "per_entity" | "whole_value" | "unknown";
  units: number;
  elements: number | null;
  content_version: number;
  changed_units: number;
  /** `host` when the host registered a drawing, `generic` when the server
   *  built one from the runtime's facts. */
  render: "host" | "generic";
};

export type ViewWire = {
  view: string;
  name: string;
  kind: "source" | "derived";
  fields: FieldWire[];
  producer: string | null;
  routing: number;
  content_version: number;
  demanded: boolean;
  changed_units: number;
  editable: boolean;
  state: string;
};

export type Placement = { x: number; y: number };

export type ComponentWire = {
  component: string;
  name: string;
  kind: "root" | "general" | "invoked";
  reads: string[];
  writes: string[];
  executed_at: number;
  suspect_at: number;
};

export type StatsWire = {
  nodes: number;
  revisions: number;
  executions: number;
  validations: number;
  validation_attempts: number;
  validation_failures: number;
  cutoffs: number;
  suspensions: number;
  demands: number;
  notifications: number;
  invalidations: number;
  queued: number;
  running: number;
  waiting: number;
  execution_failures: number;
};

export type WorkspaceWire = {
  seq: number;
  revision: number;
  runtime: string | null;
  level: string;
  views: ViewWire[];
  components: ComponentWire[];
  stats: StatsWire;
  dropped: number;
  last_noop?: string;
  last_noop_revision?: number;
};

export type RevisionWire = {
  revision: number;
  label: string;
  staged: number;
  changed: number;
  invalidated: number;
  published: boolean;
  demands: number;
  demand_causes: string[];
  executed: number;
  validated: number;
  cutoffs: number;
  failed: number;
  notifications: number;
  duration_ns: number;
  changed_views: string[];
  cutoff_views: string[];
  executed_nodes: string[];
  cutoff_nodes: string[];
  invalidated_seeds: string[];
  activity_capped: boolean;
  incomplete: boolean;
};

export type EventWire =
  | {
      kind: "transaction";
      revision: number;
      label: string;
      staged: number;
      changed: number;
      published: boolean;
      invalidated: number;
      views: string[];
      duration_ns: number;
    }
  | { kind: "invalidated"; revision: number; count: number; seeds: string[] }
  | { kind: "demand"; task: number; cause: string; subscription: number | null }
  | { kind: "demand_done"; task: number; ok: boolean; duration_ns: number }
  | { kind: "claim"; node: string; task: number }
  | { kind: "validated"; node: string; deps: number }
  | {
      kind: "committed";
      node: string;
      view: string | null;
      changed: boolean;
      duration_ns: number;
    }
  | { kind: "failed"; node: string; error: string }
  | { kind: "reused"; node: string }
  | { kind: "notified"; view: string; previous: number; current: number }
  | { kind: "running"; task: number }
  | { kind: "waiting"; task: number; node: string }
  | { kind: "woken"; task: number; node: string }
  | { kind: "collected"; nodes: number; scopes: number }
  | { kind: "catalog" };

export type EventsWire = { seq: number; events: EventWire[] };

export type HelloWire = {
  protocol: number;
  level: string;
  runtime: string | null;
  limits: Record<string, number>;
  history_from: number | null;
};

export type NodeDetailWire = {
  node: {
    key: string;
    kind: string;
    label: string;
    scope: string;
    component: string | null;
    unit: UnitWire | null;
    changed_at: number;
    verified_at: number;
    suspect_at: number;
    executed_at: number;
    validated_at: number;
    routing_at: number;
    waiters: number;
    value_size: number | null;
    state: RowState;
  };
  dependencies: {
    node: string;
    label: string;
    recorded: number;
    current: number;
    changed: boolean;
    suspect: boolean;
  }[];
  value: {
    kind: string;
    summary: string;
    detail: string | null;
    bytes: number;
    truncated: boolean;
    source: string;
  } | null;
  reclaimed: boolean;
};

export type Selection =
  | { kind: "row"; view: string; rowKey: string; label: string; unit: UnitWire | null }
  | { kind: "module"; view: string }
  | { kind: "component"; key: string; label: string }
  | { kind: "node"; key: string; label: string }
  | { kind: "revision"; revision: number };

export type GraphNodeWire = {
  key: string;
  kind: string;
  label: string;
  scope: string;
  component: string | null;
  unit: UnitWire | null;
  changed_at: number;
  verified_at: number;
  suspect_at: number;
  executed_at: number;
  validated_at: number;
  routing_at: number;
  waiters: number;
  value_size: number | null;
  state: RowState;
};
