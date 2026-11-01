//! Randomized edit sequences checked against a from-scratch oracle.

use std::collections::BTreeMap;

use reactive::{reactive, view, Runtime};

#[view(source)]
pub struct RandomTable {
    entries: Map<u32, u64>,
}

#[view(derived)]
pub struct RandomList(List<u64>);

#[reactive]
mod random_sum {
    use super::{RandomList, RandomTable};

    #[component]
    pub fn root(table: &RandomTable) -> u64 {
        reactive::ensure_view(<RandomList as reactive::ViewSchema>::DESCRIPTOR);
        let target = RandomList.field0();
        let entries = table.entries();
        entries.to_list(&target, |key, value| Some((u64::from(*key), *value)));
        let keys = entries.keys();
        keys.into_iter()
            .map(|key| entries.get(&key).unwrap_or(0))
            .sum::<u64>()
    }
}

struct SumModel(BTreeMap<u32, u64>);

impl reactive_test::Oracle for SumModel {
    type Output = u64;

    fn evaluate(&self) -> u64 {
        self.0.values().sum()
    }
}

#[test]
fn randomized_map_edits_match_the_from_scratch_oracle() {
    let runtime = Runtime::new();
    runtime.install(random_sum::Root).expect("install");
    let mut model = SumModel(BTreeMap::new());
    let mut rng = reactive_test::Lcg::new(0x0dd_ba11);

    for step in 0..256 {
        let key = rng.below(16) as u32;
        match rng.below(3) {
            0 => {
                let value = rng.range(0, 100);
                model.0.insert(key, value);
                runtime
                    .edit(|edit| {
                        let mut table = edit.view::<RandomTable>();
                        let mut entries = table.entries();
                        entries.insert(key, value);
                    })
                    .expect("insert");
            }
            1 if !model.0.is_empty() => {
                model.0.remove(&key);
                runtime
                    .edit(|edit| {
                        let mut table = edit.view::<RandomTable>();
                        let mut entries = table.entries();
                        entries.remove(&key);
                    })
                    .expect("remove");
            }
            _ => {
                if let Some(value) = model.0.get_mut(&key) {
                    *value += 1;
                    let value = *value;
                    runtime
                        .edit(|edit| {
                            let mut table = edit.view::<RandomTable>();
                            let mut entries = table.entries();
                            entries.insert(key, value);
                        })
                        .expect("update");
                }
            }
        }
        let expected = reactive_test::Oracle::evaluate(&model);
        let actual = runtime
            .read_outputs::<random_sum::Root>()
            .expect("read")
            ;
        assert_eq!(
            actual, expected,
            "step {step} diverged (keys: {:?})",
            model.0.keys().collect::<Vec<_>>()
        );
    }
}

#[view(source)]
pub struct RandomItems {
    values: List<u64>,
}

#[view(derived)]
pub struct ItemDoubles(Map<u64, u64>);

#[reactive]
mod random_doubles {
    use super::{ItemDoubles, RandomItems};

    #[component]
    pub fn root(items: &RandomItems) -> Option<u64> {
        reactive::ensure_view(<ItemDoubles as reactive::ViewSchema>::DESCRIPTOR);
        let target = ItemDoubles.field0();
        items
            .values()
            .to_map(&target, |id, value| Some((id, *value * 2)));
        target.get(&1)
    }
}

struct ListModel {
    items: Vec<(u64, u64)>,
    next_id: u64,
}

impl reactive_test::Oracle for ListModel {
    type Output = Option<u64>;

    fn evaluate(&self) -> Option<u64> {
        self.items
            .iter()
            .find(|(id, _)| *id == 1)
            .map(|(_, value)| value * 2)
    }
}

#[test]
fn randomized_list_edits_match_the_from_scratch_oracle() {
    let runtime = Runtime::new();
    runtime.install(random_doubles::Root).expect("install");
    let mut model = ListModel {
        items: Vec::new(),
        // Source list identities are allocated from one.
        next_id: 1,
    };
    let mut rng = reactive_test::Lcg::new(0x11_57);

    for step in 0..256 {
        match rng.below(3) {
            0 | 2 => {
                let value = rng.range(0, 50);
                let id = model.next_id;
                model.next_id += 1;
                model.items.push((id, value));
                runtime
                    .edit(|edit| {
                        let mut items = edit.view::<RandomItems>();
                        let mut values = items.values();
                        values.push(value);
                    })
                    .expect("push");
            }
            _ if !model.items.is_empty() => {
                let index = rng.below(model.items.len());
                if rng.one_in(2) {
                    model.items.remove(index);
                    runtime
                        .edit(|edit| {
                            let mut items = edit.view::<RandomItems>();
                            let mut values = items.values();
                            values.remove(index);
                        })
                        .expect("remove");
                } else {
                    let value = rng.range(0, 50);
                    model.items[index].1 = value;
                    runtime
                        .edit(|edit| {
                            let mut items = edit.view::<RandomItems>();
                            let mut values = items.values();
                            values.set(index, value);
                        })
                        .expect("set");
                }
            }
            _ => {}
        }
        let expected = reactive_test::Oracle::evaluate(&model);
        let actual = runtime
            .read_outputs::<random_doubles::Root>()
            .expect("read");
        assert_eq!(
            actual, expected,
            "step {step} diverged (items: {:?})",
            model.items
        );
    }
}

#[view(source)]
pub struct DiffLeft {
    entries: Map<u32, i64>,
}

#[view(source)]
pub struct DiffRight {
    entries: Map<u32, i64>,
}

#[view(derived)]
pub struct DiffJoined(Map<u32, i64>);

struct DiffWeightJoin;

impl reactive::JoinSpec for DiffWeightJoin {
    type Key = u32;
    type Left = i64;
    type Right = i64;
    type Joined = i64;

    fn left() -> (reactive::ViewId, reactive::FieldId) {
        (
            <DiffLeft as reactive::ViewSchema>::DESCRIPTOR.id,
            reactive::FieldId(0),
        )
    }

    fn right() -> (reactive::ViewId, reactive::FieldId) {
        (
            <DiffRight as reactive::ViewSchema>::DESCRIPTOR.id,
            reactive::FieldId(0),
        )
    }

    fn combine(left: &i64, right: &i64) -> i64 {
        left * right
    }

    fn target() -> Option<(reactive::ViewId, reactive::FieldId)> {
        Some((
            <DiffJoined as reactive::ViewSchema>::DESCRIPTOR.id,
            reactive::FieldId(0),
        ))
    }
}

fn expected_join(
    left: &std::collections::BTreeMap<u32, i64>,
    right: &std::collections::BTreeMap<u32, i64>,
) -> std::collections::BTreeMap<u32, i64> {
    left.iter()
        .filter_map(|(key, left_value)| {
            right
                .get(key)
                .map(|right_value| (*key, left_value * right_value))
        })
        .collect()
}

#[test]
fn randomized_join_edits_match_the_from_scratch_oracle() {
    use std::collections::BTreeMap;

    let runtime = Runtime::new();
    runtime.register_view(<DiffLeft as reactive::ViewSchema>::DESCRIPTOR);
    runtime.register_view(<DiffRight as reactive::ViewSchema>::DESCRIPTOR);
    runtime.register_view(<DiffJoined as reactive::ViewSchema>::DESCRIPTOR);
    runtime
        .install_general(reactive::EquiJoin::<DiffWeightJoin>::new())
        .expect("install");
    let mut left: BTreeMap<u32, i64> = BTreeMap::new();
    let mut right: BTreeMap<u32, i64> = BTreeMap::new();
    let mut rng = reactive_test::Lcg::new(0x0b_a6);

    for step in 0..160 {
        let key = rng.below(12) as u32;
        let value = rng.range(0, 21) as i64 - 10;
        match rng.below(3) {
            0 => {
                left.insert(key, value);
                runtime
                    .edit(|edit| {
                        let mut view = edit.view::<DiffLeft>();
                        let mut entries = view.entries();
                        entries.insert(key, value);
                    })
                    .expect("left insert");
            }
            1 => {
                right.insert(key, value);
                runtime
                    .edit(|edit| {
                        let mut view = edit.view::<DiffRight>();
                        let mut entries = view.entries();
                        entries.insert(key, value);
                    })
                    .expect("right insert");
            }
            _ => {
                let side = rng.below(2);
                if side == 0 {
                    left.remove(&key);
                    runtime
                        .edit(|edit| {
                            let mut view = edit.view::<DiffLeft>();
                            let mut entries = view.entries();
                            entries.remove(&key);
                        })
                        .expect("left remove");
                } else {
                    right.remove(&key);
                    runtime
                        .edit(|edit| {
                            let mut view = edit.view::<DiffRight>();
                            let mut entries = view.entries();
                            entries.remove(&key);
                        })
                        .expect("right remove");
                }
            }
        }
        let expected = expected_join(&left, &right);
        let actual = runtime
            .read_outputs_general::<reactive::EquiJoin<DiffWeightJoin>>()
            .expect("read join");
        assert_eq!(actual, expected, "step {step}");
        let snapshot: Vec<(u32, i64)> = runtime.snapshot().view::<DiffJoined>().field0();
        let expected_snapshot: Vec<(u32, i64)> =
            expected.iter().map(|(key, value)| (*key, *value)).collect();
        assert_eq!(snapshot, expected_snapshot, "target at step {step}");
    }
}

#[view(source)]
pub struct DiffGraph {
    graph: Graph<u64, u64>,
}

#[reactive]
mod diff_graph {
    use super::DiffGraph;

    #[component]
    pub fn root(graph: &DiffGraph) -> Vec<u64> {
        let g = graph.graph();
        let nodes = g.nodes();
        let edges = g.edges();
        let outgoing: Vec<u64> = nodes.iter().copied().flat_map(|node| {
            g.outgoing(node)
                .into_iter()
                .flat_map(move |(edge, to)| [node, edge, to])
        }).collect();
        let incoming: Vec<u64> = nodes.iter().copied().flat_map(|node| {
            g.incoming(node)
                .into_iter()
                .flat_map(move |(edge, from)| [node, edge, from])
        }).collect();
        [nodes.clone(), edges, outgoing, incoming].concat()
    }
}

struct GraphModel {
    nodes: std::collections::BTreeSet<u64>,
    edges: std::collections::BTreeMap<u64, (u64, u64, u64)>,
    next_edge: u64,
}

impl GraphModel {
    fn summary(&self) -> Vec<u64> {
        let mut out: Vec<u64> = self.nodes.iter().copied().collect();
        out.extend(self.edges.keys().copied());
        for node in &self.nodes {
            for (edge, (from, to, _)) in &self.edges {
                if from == node {
                    out.extend([*node, *edge, *to]);
                }
            }
            for (edge, (from, to, _)) in &self.edges {
                if to == node {
                    out.extend([*node, *edge, *from]);
                }
            }
        }
        out
    }
}

#[test]
fn randomized_graph_edits_match_the_from_scratch_oracle() {
    use std::collections::{BTreeMap, BTreeSet};

    let runtime = Runtime::new();
    runtime.install(diff_graph::Root).expect("install");
    let mut model = GraphModel {
        nodes: BTreeSet::new(),
        edges: BTreeMap::new(),
        next_edge: 0,
    };
    let mut rng = reactive_test::Lcg::new(0x6a_9f);
    for step in 0..200 {
        let node = rng.below(6) as u64;
        let payload = rng.below(100) as u64;
        let op = rng.below(5);
        eprintln!(
            "step {step}: op {op} node {node} payload {payload} model {:?} edges {:?}",
            model.nodes, model.edges
        );
        match op {
            0 => {
                model.nodes.insert(node);
                runtime
                    .edit(|edit| {
                        let mut graph = edit.view::<DiffGraph>();
                        let mut g = graph.graph();
                        g.add_node(node, payload);
                    })
                    .expect("add node");
            }
            1 if !model.nodes.is_empty() => {
                model.nodes.remove(&node);
                model.edges.retain(|_, (from, to, _)| *from != node && *to != node);
                runtime
                    .edit(|edit| {
                        let mut graph = edit.view::<DiffGraph>();
                        let mut g = graph.graph();
                        g.remove_node(node);
                    })
                    .expect("remove node");
            }
            2 if model.nodes.len() >= 2 => {
                let from = *model.nodes.iter().next().unwrap();
                let to = *model.nodes.iter().next_back().unwrap();
                let edge = model.next_edge;
                model.next_edge += 1;
                model.edges.insert(edge, (from, to, payload));
                let actual = runtime
                    .edit(|edit| {
                        let mut graph = edit.view::<DiffGraph>();
                        let mut g = graph.graph();
                        g.add_edge(from, to, payload)
                    })
                    .expect("add edge");
                assert_eq!(actual, edge);
            }
            3 if !model.edges.is_empty() => {
                let edge = *model.edges.keys().next().unwrap();
                model.edges.remove(&edge);
                runtime
                    .edit(|edit| {
                        let mut graph = edit.view::<DiffGraph>();
                        let mut g = graph.graph();
                        g.remove_edge(edge);
                    })
                    .expect("remove edge");
            }
            4 if !model.edges.is_empty() => {
                let edge = *model.edges.keys().next().unwrap();
                if let Some((_, _, value)) = model.edges.get_mut(&edge) {
                    *value = payload;
                }
                runtime
                    .edit(|edit| {
                        let mut graph = edit.view::<DiffGraph>();
                        let mut g = graph.graph();
                        g.set_edge(edge, payload);
                    })
                    .expect("set edge");
            }
            _ => {}
        }
        let actual = runtime
            .read_outputs::<diff_graph::Root>()
            .expect("read");
        assert_eq!(actual, model.summary(), "step {step}");
    }
}

#[reactive]
mod diff_edge_count {
    use super::DiffGraph;

    #[component]
    pub fn root(graph: &DiffGraph) -> u64 {
        graph.graph().edges().len() as u64
    }
}

#[test]
fn a_graph_edge_between_existing_nodes_is_visible() {
    let runtime = Runtime::new();
    runtime.install(diff_graph::Root).expect("install");
    runtime
        .edit(|edit| {
            let mut graph = edit.view::<DiffGraph>();
            let mut g = graph.graph();
            g.add_node(2, 20);
            g.add_node(4, 40);
        })
        .expect("nodes");
    assert_eq!(
        runtime.read_outputs::<diff_graph::Root>().expect("read"),
        vec![2, 4]
    );
    let edge = runtime
        .edit(|edit| {
            let mut graph = edit.view::<DiffGraph>();
            let mut g = graph.graph();
            g.add_edge(2, 4, 49)
        })
        .expect("edge");
    assert_eq!(
        runtime.read_outputs::<diff_graph::Root>().expect("read"),
        vec![2, 4, edge, 2, edge, 4, 4, edge, 2]
    );
}
