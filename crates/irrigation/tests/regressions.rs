//! Regression gates for the audit loop's findings.
//!
//! Each test names the contract it holds rather than the implementation it
//! checks: one answer per revision, one owner per entity, and one generation
//! per cell change. A runtime installs one root, so a second actor is a
//! callable driven through `Runtime::call`.
//!
//! This file also holds the only gates for **entity ownership** across unit
//! kinds: `a_live_presence_owner_cannot_be_taken_over_by_a_payload_retraction`
//! and `a_false_presence_retraction_cannot_take_a_presence_owner_over`. Disabling
//! `Inner::retraction_may_take_over` leaves both `arbitration.rs` and the
//! runtime's own unit tests green, so a change to ownership arbitration has to
//! be checked here even though this file's name does not suggest it.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use irrigation::{FieldId, Runtime, UnitKey, ViewSchema, irrigation, view};

// ---------------------------------------------------------------------------
// A source Map seeded through its content, and the helpers the gates share
// ---------------------------------------------------------------------------

#[view(source)]
pub struct Ledger {
    entries: Map<u32, u64>,
}

fn ledger_content(entries: &[(u32, u64)]) -> LedgerContent {
    let mut content = LedgerContent::empty();
    for (key, value) in entries {
        content.entries = content.entries.insert(*key, *value).0;
    }
    content
}

/// Commits a source View's content without materializing any cell.
fn seed_ledger(runtime: &Runtime, entries: &[(u32, u64)]) {
    runtime.register_view(<Ledger as ViewSchema>::DESCRIPTOR);
    runtime
        .edit(|edit| {
            edit.set_content(
                <Ledger as ViewSchema>::view_id(),
                Arc::new(ledger_content(entries)),
                irrigation::values_equal::<LedgerContent>,
            );
        })
        .expect("seed the content");
}

fn ledger_value(entry: u32) -> UnitKey {
    irrigation::value_unit(
        <Ledger as ViewSchema>::view_id(),
        irrigation::FieldId(0),
        entry,
    )
}

fn ledger_presence(entry: u32) -> UnitKey {
    irrigation::membership_unit(
        <Ledger as ViewSchema>::view_id(),
        irrigation::FieldId(0),
        entry,
    )
}

fn ledger_structure() -> UnitKey {
    irrigation::structure_unit(<Ledger as ViewSchema>::view_id(), irrigation::FieldId(0))
}

// ---------------------------------------------------------------------------
// collections-r16-01 — a definition-path retraction of a cell-less entity
// moves the content, so the field's structure must move with it
// ---------------------------------------------------------------------------

#[irrigation]
mod ledger_verbs {
    use super::Ledger;

    /// Reads the key set only: materializing the entry's presence cell would
    /// hand the retraction a cell it must not have.
    #[component]
    pub fn keys(ledger: &Ledger) -> (Vec<u32>, usize) {
        let entries = ledger.entries();
        (entries.keys(), entries.len())
    }

    /// Reads the entry's presence and payload after the fact.
    #[component]
    pub fn entry(_tag: u32) -> (bool, Option<u64>) {
        let entries = Ledger.entries();
        (entries.contains_key(&1), entries.get(&1))
    }

    /// Publishes an empty map, which retracts the key it no longer contains.
    #[component]
    pub fn clear(_tag: u32) -> u32 {
        irrigation::ensure_view(<Ledger as irrigation::ViewSchema>::DESCRIPTOR);
        let entries = Ledger.entries();
        entries.define_all(Vec::<(u32, u64)>::new(), "ledger_clear");
        1
    }
}

#[test]
fn a_definition_retraction_of_a_content_only_key_moves_the_structure() {
    let runtime = Runtime::new();
    seed_ledger(&runtime, &[(1, 10)]);
    runtime.install(ledger_verbs::Keys).expect("install");
    assert_eq!(
        runtime.read_outputs::<ledger_verbs::Keys>().expect("read"),
        (vec![1], 1),
        "the seeded entry is the revision's answer"
    );

    // The key was only ever in the content: the reader above materialized the
    // key set and never the entry's presence cell, so the retraction must
    // follow the content rather than the presence cell.
    runtime
        .call::<ledger_verbs::__ClearSpec>(ledger_verbs::ClearArgs { _tag: 0 })
        .expect("clear");
    assert_eq!(
        runtime.inspect_snapshot().view::<Ledger>().entries(),
        Vec::new(),
        "the committed content no longer holds the entry"
    );
    assert_eq!(
        runtime.read_outputs::<ledger_verbs::Keys>().expect("read"),
        (Vec::new(), 0),
        "the structure must agree with the content the retraction committed"
    );
    assert_eq!(
        runtime
            .call::<ledger_verbs::__EntrySpec>(ledger_verbs::EntryArgs { _tag: 0 })
            .expect("read the entry"),
        (false, None)
    );
}

// ---------------------------------------------------------------------------
// macros-r16-01 — retracting the field-level structure unit is a protocol
// change, not a field erasure
// ---------------------------------------------------------------------------

#[irrigation]
mod ledger_structure {
    use super::{Ledger, ledger_structure};

    #[component]
    pub fn inspect(ledger: &Ledger) -> (Vec<u32>, bool) {
        let entries = ledger.entries();
        (entries.keys(), entries.contains_key(&1))
    }

    /// Retracts the field-level structure unit the reader materialized.
    #[component]
    pub fn drop_structure(_tag: u32) -> u32 {
        irrigation::ensure_view(<Ledger as irrigation::ViewSchema>::DESCRIPTOR);
        irrigation::retract_unit(ledger_structure(), "ledger_drop_structure");
        0
    }
}

#[test]
fn retracting_a_field_structure_leaves_the_entities_the_field_holds() {
    let runtime = Runtime::new();
    seed_ledger(&runtime, &[(1, 10)]);
    runtime.install(ledger_structure::Inspect).expect("install");
    assert_eq!(
        runtime
            .read_outputs::<ledger_structure::Inspect>()
            .expect("read"),
        (vec![1], true)
    );

    runtime
        .call::<ledger_structure::__DropStructureSpec>(ledger_structure::DropStructureArgs {
            _tag: 0,
        })
        .expect("retract the structure");
    assert_eq!(
        runtime.inspect_snapshot().view::<Ledger>().entries(),
        vec![(1u32, 10u64)],
        "the structure's retraction must not erase the entries it projected"
    );
    assert_eq!(
        runtime
            .read_outputs::<ledger_structure::Inspect>()
            .expect("read"),
        (vec![1], true),
        "the field's readers must keep answering the content's entries"
    );
}

// ---------------------------------------------------------------------------
// runtime-api-r16-01 — the commit reconciles its own cell writes against the
// content it commits
// ---------------------------------------------------------------------------

#[irrigation]
mod ledger_entry {
    use super::Ledger;

    #[component]
    pub fn inspect(ledger: &Ledger) -> (Vec<u32>, bool, Option<u64>) {
        let entries = ledger.entries();
        (entries.keys(), entries.contains_key(&1), entries.get(&1))
    }
}

#[test]
fn a_cell_write_the_replacement_content_does_not_project_is_not_served() {
    let runtime = Runtime::new();
    seed_ledger(&runtime, &[(1, 10)]);
    runtime.install(ledger_entry::Inspect).expect("install");
    assert_eq!(
        runtime
            .read_outputs::<ledger_entry::Inspect>()
            .expect("read"),
        (vec![1], true, Some(10))
    );

    // The editor stages a coherent delta for the entry it adds; the raw
    // payload write beside it does not project the content the editor
    // publishes, and the content is the revision's answer.
    runtime
        .edit(|edit| {
            {
                let mut view = edit.view::<Ledger>();
                view.entries().insert(2, 99);
            }
            edit.set(ledger_value(1), 99u64);
        })
        .expect("insert and stage the payload");
    assert_eq!(
        runtime.inspect_snapshot().view::<Ledger>().entries(),
        vec![(1u32, 10u64), (2, 99)]
    );
    assert_eq!(
        runtime
            .read_outputs::<ledger_entry::Inspect>()
            .expect("read"),
        (vec![1, 2], true, Some(10)),
        "the committed content decides the payload"
    );
}

#[test]
fn a_cell_write_beside_an_equal_replacement_is_still_reconciled() {
    let runtime = Runtime::new();
    seed_ledger(&runtime, &[(1, 10)]);
    runtime.install(ledger_entry::Inspect).expect("install");
    assert_eq!(
        runtime
            .read_outputs::<ledger_entry::Inspect>()
            .expect("read"),
        (vec![1], true, Some(10))
    );

    // The replacement restates the committed content, so the content cutoff
    // publishes nothing; the payload written beside it still does not project
    // that content.
    runtime
        .edit(|edit| {
            edit.set(ledger_value(1), 99u64);
            edit.set_content(
                <Ledger as ViewSchema>::view_id(),
                Arc::new(ledger_content(&[(1, 10)])),
                irrigation::values_equal::<LedgerContent>,
            );
        })
        .expect("restate the content");
    assert_eq!(
        runtime
            .read_outputs::<ledger_entry::Inspect>()
            .expect("read"),
        (vec![1], true, Some(10)),
        "the content, not the staged payload, decides the value"
    );
}

// ---------------------------------------------------------------------------
// runtime-api-r16-02 — a retraction closes the payload generation even when a
// replacement opened it without a payload
// ---------------------------------------------------------------------------

#[test]
fn a_retraction_closes_a_replacement_opened_payload_generation() {
    let runtime = Runtime::new();
    runtime.register_view(<Ledger as ViewSchema>::DESCRIPTOR);
    runtime.install(ledger_entry::Inspect).expect("install");

    // r0: an editor installs the entry's cells.
    runtime
        .edit(|edit| {
            let mut view = edit.view::<Ledger>();
            view.entries().insert(1, 10);
        })
        .expect("insert");

    let fired = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&fired);
    let _subscription = runtime
        .observe_units::<Ledger>(vec![ledger_value(1)], move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        })
        .expect("subscribe to the payload");

    // r1: a whole-region replacement reopens the payload as a new generation
    // and keeps the presence cell. The payload's subscriber hears it.
    runtime
        .edit(|edit| {
            edit.set_content(
                <Ledger as ViewSchema>::view_id(),
                Arc::new(ledger_content(&[(1, 10), (2, 99)])),
                irrigation::values_equal::<LedgerContent>,
            );
        })
        .expect("replace the content");
    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "the replacement opened the payload's generation"
    );

    // r2: the editor removes the entry. The presence moves, and the payload
    // generation the replacement opened closes with it.
    runtime
        .edit(|edit| {
            let mut view = edit.view::<Ledger>();
            view.entries().remove(&1);
        })
        .expect("remove");
    assert_eq!(
        fired.load(Ordering::SeqCst),
        2,
        "a payload subscriber hears the removal of a generation it never saw filled"
    );
    assert_eq!(
        runtime
            .read_outputs::<ledger_entry::Inspect>()
            .expect("read"),
        (vec![2], false, None),
        "a read after the removal observes the absence"
    );
}

// ---------------------------------------------------------------------------
// runtime-kernel-r16-01 — ownership is one question in one key space
// ---------------------------------------------------------------------------

#[view(derived)]
pub struct Roster(Set<u32>);

fn roster_value(member: u32) -> UnitKey {
    irrigation::value_unit(
        <Roster as ViewSchema>::view_id(),
        irrigation::FieldId(0),
        member,
    )
}

fn roster_presence(member: u32) -> UnitKey {
    irrigation::membership_unit(
        <Roster as ViewSchema>::view_id(),
        irrigation::FieldId(0),
        member,
    )
}

#[irrigation]
mod roster_owners {
    use super::{Roster, roster_presence, roster_value};

    /// Publishes the member as a set does: a presence draft.
    #[component]
    pub fn presence(_tag: u32) -> u32 {
        irrigation::ensure_view(<Roster as irrigation::ViewSchema>::DESCRIPTOR);
        Roster.field0().define_keyed([1u32], "roster_presence");
        0
    }

    /// Publishes the same entity as a payload draft.
    #[component]
    pub fn payload(_tag: u32) -> u32 {
        irrigation::ensure_view(<Roster as irrigation::ViewSchema>::DESCRIPTOR);
        irrigation::define_unit(roster_value(1), 10u32, "roster_payload");
        0
    }

    /// Stages the removal spelled as a payload retraction.
    #[component]
    pub fn withdraw_value(_tag: u32) -> u32 {
        irrigation::ensure_view(<Roster as irrigation::ViewSchema>::DESCRIPTOR);
        irrigation::retract_unit(roster_value(1), "roster_withdraw_value");
        0
    }

    /// Stages the removal spelled as a false presence draft.
    #[component]
    pub fn withdraw_presence(_tag: u32) -> u32 {
        irrigation::ensure_view(<Roster as irrigation::ViewSchema>::DESCRIPTOR);
        irrigation::define_unit(roster_presence(1), false, "roster_withdraw_presence");
        0
    }

    #[component]
    pub fn read(_tag: u32) -> Vec<u32> {
        Roster.field0().to_vec()
    }
}

#[test]
fn a_payload_provider_colliding_with_a_presence_owner_is_a_duplicate_definition() {
    let runtime = Runtime::new();
    runtime.register_view(<Roster as ViewSchema>::DESCRIPTOR);
    runtime
        .call::<roster_owners::__PresenceSpec>(roster_owners::PresenceArgs { _tag: 0 })
        .expect("publish the member");
    let error = runtime
        .call::<roster_owners::__PayloadSpec>(roster_owners::PayloadArgs { _tag: 0 })
        .unwrap_err();
    assert!(
        matches!(error, irrigation::ComputeError::DuplicateDefinition { .. }),
        "one entity, two live providers: {error:?}"
    );
    assert_eq!(
        runtime
            .call::<roster_owners::__ReadSpec>(roster_owners::ReadArgs { _tag: 0 })
            .expect("read"),
        vec![1],
        "the rejected revision keeps the presence owner's content"
    );
}

/// The two spellings of one removal answer the same authority: the entity's
/// owner key, not the unit kind the retracting provider staged.
fn assert_withdrawal_is_denied(withdraw: fn(&Runtime)) {
    let runtime = Runtime::new();
    runtime.register_view(<Roster as ViewSchema>::DESCRIPTOR);
    runtime
        .call::<roster_owners::__PresenceSpec>(roster_owners::PresenceArgs { _tag: 0 })
        .expect("publish the member");
    withdraw(&runtime);
    assert_eq!(
        runtime
            .call::<roster_owners::__ReadSpec>(roster_owners::ReadArgs { _tag: 0 })
            .expect("read"),
        vec![1],
        "the refusal leaves the presence owner's content in place"
    );
}

#[test]
fn a_live_presence_owner_cannot_be_taken_over_by_a_payload_retraction() {
    assert_withdrawal_is_denied(|runtime| {
        let error = runtime
            .call::<roster_owners::__WithdrawValueSpec>(roster_owners::WithdrawValueArgs {
                _tag: 0,
            })
            .unwrap_err();
        assert!(
            matches!(error, irrigation::ComputeError::DefinitionDenied { .. }),
            "a live provider's entity is not silently removable: {error:?}"
        );
    });
}

#[test]
fn a_false_presence_retraction_cannot_take_a_presence_owner_over() {
    assert_withdrawal_is_denied(|runtime| {
        let error = runtime
            .call::<roster_owners::__WithdrawPresenceSpec>(roster_owners::WithdrawPresenceArgs {
                _tag: 0,
            })
            .unwrap_err();
        assert!(
            matches!(error, irrigation::ComputeError::DefinitionDenied { .. }),
            "the presence spelling of the same removal answers the same authority: {error:?}"
        );
    });
}

// ---------------------------------------------------------------------------
// runtime-kernel-r16-03 — a presence filled from the content invalidates the
// readers that observed its vacant cell
// ---------------------------------------------------------------------------

#[irrigation]
mod ledger_presence {
    use super::{Ledger, ledger_presence, ledger_value};

    /// The documented negative read: the accessor declares no projection, so a
    /// unit the content answers is still observed as absent.
    #[component]
    pub fn negative(ledger: &Ledger) -> bool {
        let _ = ledger.entries();
        irrigation::try_read_unit_value::<bool, _>(ledger_presence(1), |_| None).unwrap_or(false)
    }

    /// Defines the payload of an entity the committed content already holds.
    #[component]
    pub fn install(_tag: u32) -> u32 {
        irrigation::ensure_view(<Ledger as irrigation::ViewSchema>::DESCRIPTOR);
        irrigation::define_unit(ledger_value(1), 10u64, "presence_install");
        1
    }
}

#[test]
fn a_presence_filled_from_the_content_invalidates_its_negative_readers() {
    let runtime = Runtime::new();
    seed_ledger(&runtime, &[(1, 10)]);
    runtime.install(ledger_presence::Negative).expect("install");
    assert!(
        !runtime
            .read_outputs::<ledger_presence::Negative>()
            .expect("read"),
        "the vacant membership cell is observed as absent"
    );

    runtime
        .call::<ledger_presence::__InstallSpec>(ledger_presence::InstallArgs { _tag: 0 })
        .expect("install the payload");
    assert!(
        runtime
            .read_outputs::<ledger_presence::Negative>()
            .expect("read"),
        "the definition filled the presence its reader observed as vacant"
    );
}

// ---------------------------------------------------------------------------
// A provider-owned structure is the provider's to publish, on every path
// ---------------------------------------------------------------------------

#[view(source)]
pub struct GraphMode {
    nodes: u64,
}

#[view(derived)]
pub struct SizeGraph(Graph<u64, u64>);

#[irrigation]
mod owned_structure {
    use super::{GraphMode, SizeGraph};

    /// `define_graph` owns the field's structure unit (the topology) and
    /// retracts the node units a smaller definition no longer contains.
    #[component]
    pub fn publish(_tag: u32) -> u32 {
        irrigation::ensure_view(<SizeGraph as irrigation::ViewSchema>::DESCRIPTOR);
        let nodes = match GraphMode.nodes() {
            1 => vec![(1u64, 10u64)],
            _ => vec![(1u64, 10u64), (2u64, 20u64)],
        };
        SizeGraph.define_graph(&nodes, &[]);
        0
    }

    #[component]
    pub fn read() -> Vec<u64> {
        SizeGraph.nodes()
    }
}

#[test]
fn a_provider_owned_structure_is_not_republished_by_a_node_retraction() {
    let runtime = Runtime::new();
    runtime.register_view(<GraphMode as ViewSchema>::DESCRIPTOR);
    runtime.register_view(<SizeGraph as ViewSchema>::DESCRIPTOR);
    runtime.install(owned_structure::Read).expect("install");
    runtime
        .edit(|edit| edit.view::<GraphMode>().set_nodes(2))
        .expect("mode");
    runtime
        .call::<owned_structure::__PublishSpec>(owned_structure::PublishArgs { _tag: 0 })
        .expect("define two nodes");
    assert_eq!(
        runtime
            .read_outputs::<owned_structure::Read>()
            .expect("read"),
        vec![1, 2]
    );

    // The provider shrinks the graph: the stale node unit is retracted, and the
    // field's structure is restated by the provider itself.
    let before = runtime.stats().structure_refreshes;
    runtime
        .edit(|edit| edit.view::<GraphMode>().set_nodes(1))
        .expect("mode");
    runtime
        .call::<owned_structure::__PublishSpec>(owned_structure::PublishArgs { _tag: 0 })
        .expect("define one node");
    assert_eq!(
        runtime
            .read_outputs::<owned_structure::Read>()
            .expect("read"),
        vec![1]
    );
    assert_eq!(
        runtime.stats().structure_refreshes,
        before,
        "the structure cell belongs to the provider; a node retraction must not \
         republish it"
    );
}

// ---------------------------------------------------------------------------
// collections-seq-12 — a keyed/whole flip must invalidate keyed-branch readers
// ---------------------------------------------------------------------------

#[view(source)]
pub struct FlipMode {
    keyed: bool,
}

#[view(derived)]
pub struct FlipSet(Set<u32>);

#[irrigation]
mod flip_verbs {
    use super::{FlipMode, FlipSet};

    /// Publishes the member keyed, or the same members as one whole value.
    #[component]
    pub fn publish(_tag: u32) -> u32 {
        irrigation::ensure_view(<FlipSet as irrigation::ViewSchema>::DESCRIPTOR);
        let set = FlipSet.field0();
        if FlipMode.keyed() {
            set.define_keyed([1u32], "flip_keyed");
        } else {
            set.define_all([1u32, 2], "flip_whole");
        }
        0
    }

    /// A negative membership read. Under the keyed protocol it observes the
    /// member's own presence cell and nothing else, so only the protocol
    /// certificate can tell it that the field changed mode.
    #[component]
    pub fn absent() -> bool {
        FlipSet.field0().contains(&2)
    }
}

#[test]
fn a_keyed_to_whole_flip_invalidates_a_negative_membership_read() {
    let runtime = Runtime::new();
    runtime.register_view(<FlipMode as ViewSchema>::DESCRIPTOR);
    runtime.register_view(<FlipSet as ViewSchema>::DESCRIPTOR);
    runtime.install(flip_verbs::Absent).expect("install");
    runtime
        .edit(|edit| edit.view::<FlipMode>().set_keyed(true))
        .expect("mode");
    runtime
        .call::<flip_verbs::__PublishSpec>(flip_verbs::PublishArgs { _tag: 0 })
        .expect("publish keyed");
    assert!(
        !runtime.read_outputs::<flip_verbs::Absent>().expect("read"),
        "under the keyed protocol member 2 is absent"
    );

    // Flip the field to the whole-value protocol. A from-scratch evaluation of
    // this revision takes the whole branch and answers true.
    runtime
        .edit(|edit| edit.view::<FlipMode>().set_keyed(false))
        .expect("mode");
    runtime
        .call::<flip_verbs::__PublishSpec>(flip_verbs::PublishArgs { _tag: 0 })
        .expect("publish whole");
    assert!(
        runtime.read_outputs::<flip_verbs::Absent>().expect("read"),
        "the flip must invalidate the keyed-branch reader, not leave it certified"
    );
}

// ---------------------------------------------------------------------------
// A batch that retracts nothing must publish nothing
// ---------------------------------------------------------------------------

#[view(derived)]
pub struct AbsentSet(Set<u32>);

#[irrigation]
mod absent_verbs {
    use super::{AbsentSet, absent_member};

    /// Publishes member 1 keyed, so the field's content is `{1}`.
    #[component]
    pub fn publish(_tag: u32) -> u32 {
        irrigation::ensure_view(<AbsentSet as irrigation::ViewSchema>::DESCRIPTOR);
        AbsentSet.field0().define_keyed([1u32], "absent_publish");
        0
    }

    /// Retracts member 2, which this field never held.
    #[component]
    pub fn withdraw_absent(_tag: u32) -> u32 {
        irrigation::ensure_view(<AbsentSet as irrigation::ViewSchema>::DESCRIPTOR);
        irrigation::retract_unit(absent_member(2), "absent_withdraw");
        0
    }
}

fn absent_member(member: u32) -> UnitKey {
    irrigation::membership_unit(
        <AbsentSet as ViewSchema>::view_id(),
        irrigation::FieldId(0),
        member,
    )
}

#[test]
fn a_retraction_of_an_absent_member_publishes_no_content() {
    let runtime = Runtime::new();
    runtime.register_view(<AbsentSet as ViewSchema>::DESCRIPTOR);
    runtime
        .call::<absent_verbs::__PublishSpec>(absent_verbs::PublishArgs { _tag: 0 })
        .expect("publish member 1");
    let before = runtime.stats().content_commits;

    runtime
        .call::<absent_verbs::__WithdrawAbsentSpec>(absent_verbs::WithdrawAbsentArgs { _tag: 0 })
        .expect("retract the absent member");
    assert_eq!(
        runtime.stats().content_commits,
        before,
        "retracting a member the content never held moves no content, so the \
         field must not publish a new content version"
    );
}

// ---------------------------------------------------------------------------
// Supersession: a keyed restatement retires the members it stops listing
// ---------------------------------------------------------------------------

#[view(source)]
pub struct MemberScript {
    members: u64,
}

#[view(derived)]
pub struct KeyedSet(Set<u32>);

#[irrigation]
mod keyed_restates {
    use super::{KeyedSet, MemberScript};

    /// Restates the keyed set from the script. `define_keyed` publishes one
    /// whole-field draft plus a membership per *current* member, so a member it
    /// stops listing is not retracted explicitly: supersession is what retires
    /// its cells.
    #[component]
    pub fn publish(_tag: u32) -> u32 {
        irrigation::ensure_view(<KeyedSet as irrigation::ViewSchema>::DESCRIPTOR);
        let members: Vec<u32> = match MemberScript.members() {
            1 => vec![1],
            2 => vec![1, 2],
            _ => vec![1, 2, 3],
        };
        KeyedSet.field0().define_keyed(members, "keyed_restate");
        0
    }

    #[component(root)]
    pub fn read() -> Vec<(u32, bool)> {
        let set = KeyedSet.field0();
        [1u32, 2, 3]
            .iter()
            .map(|member| (*member, set.contains(member)))
            .collect()
    }
}

#[test]
fn a_keyed_restatement_retires_the_members_it_drops() {
    let runtime = Runtime::new();
    runtime.register_view(<MemberScript as ViewSchema>::DESCRIPTOR);
    runtime.register_view(<KeyedSet as ViewSchema>::DESCRIPTOR);
    runtime.install(keyed_restates::Read).expect("install");
    runtime
        .edit(|edit| edit.view::<MemberScript>().set_members(3))
        .expect("script");
    runtime
        .call::<keyed_restates::__PublishSpec>(keyed_restates::PublishArgs { _tag: 0 })
        .expect("publish three");
    assert_eq!(
        runtime
            .read_outputs::<keyed_restates::Read>()
            .expect("read"),
        vec![(1, true), (2, true), (3, true)]
    );

    // The presence cell of a member this provider still owns, so the drop below
    // is a release by a provider and not a host edit.
    let payload = irrigation::value_unit(KeyedSet::view_id(), irrigation::FieldId(0), 2u32);
    let membership = irrigation::UnitKey {
        kind: irrigation::UnitKind::Membership,
        ..payload
    };
    let routing_before = runtime.routing_generation(&membership);

    // Drop members 2 and 3 by listing only member 1.
    runtime
        .edit(|edit| edit.view::<MemberScript>().set_members(1))
        .expect("script");
    runtime
        .call::<keyed_restates::__PublishSpec>(keyed_restates::PublishArgs { _tag: 0 })
        .expect("publish one");
    assert_eq!(
        runtime
            .read_outputs::<keyed_restates::Read>()
            .expect("read"),
        vec![(1, true), (2, false), (3, false)],
        "a member the restatement stops listing must read absent"
    );

    // Both cells of the dropped member belong to one release by one provider, so
    // the presence cell's routing generation moves with the payload's. A source
    // View's host edit moves neither.
    assert!(
        runtime.routing_generation(&membership) > routing_before,
        "the provider released the member, so the presence cell's routing moved too"
    );
}

// ---------------------------------------------------------------------------
// A read or an edit from inside an edit transaction refuses instead of hanging
// (runtime-api-02)
// ---------------------------------------------------------------------------

#[test]
fn a_demand_inside_an_edit_transaction_is_refused() {
    let runtime = Runtime::new();
    seed_ledger(&runtime, &[(1, 10)]);
    runtime.install(ledger_verbs::Keys).expect("install");

    // A read from the closure used to wait for the write side of the revision
    // gate, which this same thread holds, so it never returned.
    // The refusal is the closure's value; the transaction itself still commits,
    // which is why the outer `edit` succeeds.
    let error = runtime
        .edit(|_edit| runtime.read_outputs::<ledger_verbs::Keys>())
        .expect("the transaction still commits")
        .expect_err("a demand inside an edit cannot run");
    assert!(
        matches!(
            error,
            irrigation::ComputeError::ReentrantUse {
                operation: "read",
                ..
            }
        ),
        "expected the structured refusal, got {error:?}"
    );

    // The runtime is still usable afterwards.
    assert_eq!(
        runtime.read_outputs::<ledger_verbs::Keys>().expect("read"),
        (vec![1], 1)
    );
}

#[test]
fn an_edit_inside_an_edit_transaction_is_refused() {
    let runtime = Runtime::new();
    seed_ledger(&runtime, &[(1, 10)]);
    runtime.install(ledger_verbs::Keys).expect("install");

    let error = runtime
        .edit(|_outer| runtime.edit(|edit| edit.set(ledger_value(1), 20u64)))
        .expect("the outer transaction still commits")
        .expect_err("a nested edit cannot open the gate its parent holds");
    assert!(
        matches!(error, irrigation::ComputeError::ReentrantUse { .. }),
        "expected the structured refusal, got {error:?}"
    );
    assert_eq!(
        runtime.read_outputs::<ledger_verbs::Keys>().expect("read"),
        (vec![1], 1)
    );
}

// ---------------------------------------------------------------------------
// One component identity, one component (runtime-api-r11-04)
// ---------------------------------------------------------------------------

#[irrigation]
mod shared_identity {
    use super::Ledger;

    /// An ordinary root, so its identity is the one a producer could adopt.
    #[component(root)]
    pub fn root(ledger: &Ledger) -> usize {
        ledger.entries().len()
    }
}

/// A derived View for the producer to publish into.
#[view(derived)]
pub struct AdoptedOut(Set<u32>);

/// A general component that claims the root's identity.
struct AdoptsRootIdentity;

impl irrigation::GeneralComponent for AdoptsRootIdentity {
    type State = ();
    type Output = ();

    // Exactly the root's own identity, which is the input the check exists to
    // reject: the root's id is derived from its path, not declared here.
    const ID: irrigation::ComponentId = <shared_identity::Root as irrigation::RootComponent>::ID;
    const NAME: &'static str = "regressions::AdoptsRootIdentity";
    const VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<AdoptedOut as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<AdoptedOut as ViewSchema>::DESCRIPTOR];

    fn initialize(
        &self,
        _cx: &mut irrigation::GeneralCx,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        Ok(((), ()))
    }

    fn update(
        &self,
        _cx: &mut irrigation::GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        Ok(((), ()))
    }
}

#[test]
fn a_producer_cannot_adopt_the_roots_component_id() {
    let runtime = Runtime::new();
    runtime.register_view(<Ledger as ViewSchema>::DESCRIPTOR);
    runtime.install(shared_identity::Root).expect("root");
    let error = runtime
        .install_general_producer(AdoptsRootIdentity)
        .expect_err("two live components cannot share one identity");
    assert!(
        format!("{error}").contains("component id is already installed"),
        "expected the uniqueness error, got {error:?}"
    );
}

// ---------------------------------------------------------------------------
// A source removal moves no routing generation (runtime-api-r11-03)
// ---------------------------------------------------------------------------

#[test]
fn a_source_removal_moves_no_routing_generation() {
    let runtime = Runtime::new();
    seed_ledger(&runtime, &[(1, 10)]);
    runtime.install(ledger_verbs::Keys).expect("install");

    // Read presence and payload, so both cells exist: an unmaterialized cell has
    // no stamp to move and this test would measure nothing.
    assert_eq!(
        runtime
            .call::<ledger_verbs::__EntrySpec>(ledger_verbs::EntryArgs { _tag: 0 })
            .expect("entry"),
        (true, Some(10))
    );

    let value = ledger_value(1);
    let membership = irrigation::UnitKey {
        kind: irrigation::UnitKind::Membership,
        ..value.clone()
    };
    let value_routing = runtime.routing_generation(&value);
    let membership_routing = runtime.routing_generation(&membership);

    // A host edit removes the member. No provider owned it before or after, so
    // no routing generation can have moved -- routing answers "who else could
    // define this", and a source View has no provider at all.
    runtime
        .edit(|edit| edit.set(membership.clone(), false))
        .expect("remove");

    assert_eq!(
        runtime.routing_generation(&value),
        value_routing,
        "a removal in a View with no provider is not a provider change"
    );
    assert_eq!(
        runtime.routing_generation(&membership),
        membership_routing,
        "the entity's two cells must agree about whether the provider moved"
    );
}

// ---------------------------------------------------------------------------
// A host removal stops the removed entity's payload from being served
// (collections-seq-04)
// ---------------------------------------------------------------------------

/// Reads the payload unit of entry 1 straight from the runtime, bypassing the
/// View's accessors, so the answer depends on the cell alone.
#[irrigation]
mod payload_peek {
    use super::ledger_value;

    #[component]
    pub fn peek(_tag: u32) -> bool {
        irrigation::try_read_unit_value::<u64, _>(ledger_value(1), |content| {
            content.downcast_ref::<u64>().copied()
        })
        .is_some()
    }
}

#[test]
fn a_host_removal_stops_serving_the_removed_payload() {
    let runtime = Runtime::new();
    seed_ledger(&runtime, &[(1, 10)]);
    runtime.install(ledger_verbs::Keys).expect("install");

    // Materialize the payload cell, so the removal has a generation to close.
    assert_eq!(
        runtime
            .call::<ledger_verbs::__EntrySpec>(ledger_verbs::EntryArgs { _tag: 0 })
            .expect("entry"),
        (true, Some(10))
    );
    assert!(
        runtime
            .call::<payload_peek::__PeekSpec>(payload_peek::PeekArgs { _tag: 0 })
            .expect("peek"),
        "the payload is served while the entry is present"
    );

    let membership = irrigation::UnitKey {
        kind: irrigation::UnitKind::Membership,
        ..ledger_value(1)
    };
    runtime
        .edit(|edit| edit.set(membership, false))
        .expect("remove");

    assert!(
        !runtime
            .call::<payload_peek::__PeekSpec>(payload_peek::PeekArgs { _tag: 0 })
            .expect("peek"),
        "a removed entry's payload unit must not be served from its old certification"
    );
}

// ---------------------------------------------------------------------------
// An edit in one runtime does not mark another runtime as editing
// (runtime-kernel-r66-01)
// ---------------------------------------------------------------------------

#[test]
fn an_edit_closure_may_read_a_second_runtime() {
    let first = Runtime::new();
    seed_ledger(&first, &[(1, 10)]);
    first.install(ledger_verbs::Keys).expect("first");

    let second = Runtime::new();
    seed_ledger(&second, &[(2, 20)]);
    second.install(ledger_verbs::Keys).expect("second");

    // The marker answers "does *this* runtime hold its write gate on this
    // thread". The second runtime's gate is open, so its read must be served
    // rather than refused as re-entrant.
    let read = first
        .edit(|_edit| {
            second
                .read_outputs::<ledger_verbs::Keys>()
                .expect("a second runtime is readable from inside an edit")
        })
        .expect("the transaction commits");
    assert_eq!(read, (vec![2], 1));
}

// ---------------------------------------------------------------------------
// A commit whose value equals the committed one is counted as a cutoff
// (runtime-kernel-08)
// ---------------------------------------------------------------------------

/// Answers with the parity of the summed payloads: a payload edit re-runs the
/// body without changing the answer.
#[irrigation]
mod ledger_count {
    use super::Ledger;

    #[component(root)]
    pub fn count(ledger: &Ledger) -> u64 {
        let entries = ledger.entries();
        entries
            .keys()
            .into_iter()
            .map(|key| entries.get(&key).unwrap_or(0))
            .sum::<u64>()
            % 2
    }
}

#[test]
fn an_equal_commit_is_counted_as_a_cutoff() {
    let runtime = Runtime::new();
    seed_ledger(&runtime, &[(1, 10)]);
    runtime.install(ledger_count::Count).expect("install");
    assert_eq!(
        runtime.read_outputs::<ledger_count::Count>().expect("read"),
        0
    );
    assert_eq!(
        runtime.stats().cutoffs,
        0,
        "the first evaluation has no previous value to cut off against"
    );

    // The payload moves, so the body runs again -- and 12 keeps the parity, which
    // is the cutoff.
    runtime
        .edit(|edit| edit.set(ledger_value(1), 12u64))
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<ledger_count::Count>().expect("read"),
        0
    );
    let cutoffs = runtime.stats().cutoffs;
    assert!(
        cutoffs > 0,
        "a commit whose value did not move is a cutoff, and the counter says so"
    );
}

// ---------------------------------------------------------------------------
// A workerless runtime reports what it has, and still computes
// (runtime-api-r77-01)
// ---------------------------------------------------------------------------

#[test]
fn a_workerless_runtime_reports_zero_workers_and_still_runs() {
    let runtime = Runtime::with_config(irrigation::RuntimeConfig {
        workers: 0,
        ..irrigation::RuntimeConfig::default()
    });
    seed_ledger(&runtime, &[(1, 10)]);
    runtime.install(ledger_verbs::Keys).expect("install");

    assert_eq!(
        runtime.worker_count(),
        0,
        "no workers were configured, so the accessor must not claim one"
    );
    // Every demand runs inline on this thread; the answer is the same.
    assert_eq!(
        runtime.read_outputs::<ledger_verbs::Keys>().expect("read"),
        (vec![1], 1)
    );
}

// ---------------------------------------------------------------------------
// The entity list follows the content order, for a list
// ---------------------------------------------------------------------------

#[view(source)]
pub struct Attendance {
    items: List<u32>,
}

/// A list's items are listed by `field_entities`.
///
/// This is a behaviour check, not a gate on the content-order branch: the edit path creates a
/// cell per item, so the row list would include them with or without that branch (verified by
/// deleting the branch and watching this still pass). The branch is for entities the content
/// spells *without* cells, and no fixture in the suite produces that shape for any family --
/// which is what keeps the finding open.
#[cfg(feature = "observatory")]
#[test]
fn a_list_lists_the_items_its_content_spells() {
    let runtime = Runtime::new();
    runtime
        .edit(|edit| {
            let mut attendance = edit.view::<Attendance>();
            let mut roster = attendance.items();
            roster.push(1);
            roster.push(2);
            roster.push(3);
        })
        .expect("seed");

    let entities = runtime.field_entities(<Attendance as ViewSchema>::view_id(), FieldId(0));
    assert_eq!(
        entities.len(),
        3,
        "the three items the content defines are listed"
    );
}
