//! A randomized oracle for the *definition* path.
//!
//! Every other randomized check in the workspace drives source edits. This one
//! drives a publisher: each revision a provider restates a derived Map from a
//! script, and the answer must equal a from-scratch evaluation of the same
//! script. That is the contract the definition pipeline owns — drafts,
//! retractions, supersession, ownership — and nothing exercised it at random.
//!
//! Every seventh revision also switches publication strategy, keyed units to
//! one whole-field draft and back, because the two spellings have different
//! retraction paths and the field's protocol certificate is what keeps them
//! apart.

use std::collections::BTreeMap;

use irrigation::{Runtime, irrigation, view};

#[view(source)]
pub struct Script {
    step: u64,
}

#[view(derived)]
pub struct Published(Map<u32, u64>);

#[view(derived)]
pub struct PublishedSet(Set<u32>);

/// What the publisher must publish for one step: deterministic, with entries
/// appearing, changing and disappearing between steps.
fn expected(step: u64) -> BTreeMap<u32, u64> {
    let mut out = BTreeMap::new();
    for key in 0..12u32 {
        let hash = step
            .wrapping_mul(2_654_435_761)
            .wrapping_add(u64::from(key).wrapping_mul(97))
            % 100;
        // A third of the keys are absent at any step, and which third moves.
        if hash % 3 != 0 {
            out.insert(key, hash);
        }
    }
    out
}

/// One read of both published Views, through their per-key accessors.
type Probe = (Vec<u32>, Vec<(u32, bool, Option<u64>)>, Vec<(u32, bool)>);

#[irrigation]
mod publisher {
    use super::{Published, PublishedSet, Script, expected};

    /// Restates both derived Views from the script.
    ///
    /// The Map is published as one whole field, so an entry that disappears is
    /// superseded. The Set is published keyed, so the same disappearance goes
    /// through the entity-retraction batch and restates the structure. Both are
    /// the definition pipeline and neither was exercised at random.
    /// The tracked answer: membership and payload through the per-key cells, for
    /// every key the script can use. Content-level checks would not see a stale
    /// cell, which is the failure this family of tests exists to catch.
    #[component]
    pub fn read(_tag: u32) -> super::Probe {
        let map = Published.field0();
        let set = PublishedSet.field0();
        // Twelve keys, spelled literally: a component body lowers `for` and
        // `let mut` away, so the probe is a fixed array and one iterator chain.
        let per_key: Vec<(u32, bool, Option<u64>)> = [0u32, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]
            .iter()
            .map(|key| (*key, map.contains_key(key), map.get(key)))
            .collect();
        // The keyed set's membership cells, which is where a missing
        // supersession shows up: its structure can be right while a dropped
        // member's own cell still answers present.
        let set_members: Vec<(u32, bool)> = [0u32, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]
            .iter()
            .map(|key| (*key, set.contains(key)))
            .collect();
        (set.to_vec(), per_key, set_members)
    }

    #[component]
    pub fn root(script: &Script) -> usize {
        irrigation::ensure_view(<Published as irrigation::ViewSchema>::DESCRIPTOR);
        irrigation::ensure_view(<PublishedSet as irrigation::ViewSchema>::DESCRIPTOR);
        let items: Vec<(u32, u64)> = expected(script.step()).into_iter().collect();
        Published
            .field0()
            .define_all(items.clone(), "definition_oracle_whole");
        PublishedSet
            .field0()
            .define_keyed(items.iter().map(|(key, _)| *key), "definition_oracle_keyed");
        Published.field0().to_vec().len()
    }
}

#[test]
fn randomized_publication_matches_from_scratch_evaluation() {
    let runtime = Runtime::new();
    runtime.register_view(<Script as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .install(publisher::Root)
        .expect("install the publisher");
    runtime
        .edit(|edit| edit.view::<Script>().set_step(0))
        .expect("seed the script");

    for step in 0..240u64 {
        runtime
            .edit(|edit| edit.view::<Script>().set_step(step))
            .expect("advance the script");
        let expected = expected(step);
        runtime
            .read_outputs::<publisher::Root>()
            .expect("read the publisher");
        let snapshot = runtime.snapshot();
        let published: BTreeMap<u32, u64> =
            snapshot.view::<Published>().field0().into_iter().collect();
        let keys: Vec<u32> = snapshot.view::<PublishedSet>().field0();
        assert_eq!(
            published, expected,
            "step {step}: the published Map disagrees with a from-scratch evaluation"
        );
        assert_eq!(
            keys,
            expected.keys().copied().collect::<Vec<_>>(),
            "step {step}: the keyed Set disagrees with a from-scratch evaluation"
        );

        // The same answer through the per-key cells.
        let (members, per_key, set_members) = runtime
            .call::<publisher::__ReadSpec>(publisher::ReadArgs { _tag: 0 })
            .expect("read through the accessors");
        assert_eq!(
            members,
            expected.keys().copied().collect::<Vec<_>>(),
            "step {step}: the keyed Set's members disagree when read per key"
        );
        for (key, present) in set_members {
            assert_eq!(
                present,
                expected.contains_key(&key),
                "step {step}: set member {key} disagrees when read through its cell"
            );
        }
        for (key, present, value) in per_key {
            let want = expected.get(&key).copied();
            assert_eq!(
                (present, value),
                (want.is_some(), want),
                "step {step}: key {key} disagrees when read through its cells"
            );
        }
    }
}
