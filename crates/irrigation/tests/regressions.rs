//! Regression gates for the round-16 medium findings.
//!
//! Each test names the contract it holds rather than the implementation it
//! checks: one answer per revision, one owner per entity, and one generation
//! per cell change. A runtime installs one root, so a second actor is a
//! callable driven through `Runtime::call`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use irrigation::{Runtime, UnitKey, ViewSchema, irrigation, view};

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
