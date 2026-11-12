//! Unit tests for `src/lower.rs`.

use super::*;
use quote::quote;

fn lowered(body: TokenStream) -> String {
    let block: Block = syn::parse2(body).expect("block");
    Lowerer::new().block(&block).to_string()
}

#[test]
fn a_format_capture_records_the_binding() {
    let tokens = lowered(quote!({
        let name = input.name();
        format!("{name}")
    }));
    assert!(
        tokens.contains("observe_local (__node_name"),
        "a format capture must observe its binding: {tokens}"
    );
}

#[test]
fn a_vec_element_is_lowered() {
    let tokens = lowered(quote!({
        let seed = input.seed();
        vec![seed]
    }));
    assert!(
        tokens.contains("observe_clone (__node_seed"),
        "a vec element must be lowered: {tokens}"
    );
}

#[test]
fn a_struct_update_base_is_lowered() {
    let tokens = lowered(quote!({
        let base = Point {
            x: 1,
            y: input.wide(),
        };
        let point = Point { x: 2, ..base };
        point.y
    }));
    assert!(
        tokens.contains(".. irrigation :: observe_clone (__node_base"),
        "a struct-update base must be lowered: {tokens}"
    );
}

#[test]
fn a_matches_scrutinee_is_lowered() {
    let tokens = lowered(quote!({
        let flag = input.flag();
        matches!(flag, true)
    }));
    assert!(
        tokens.contains("observe_clone (__node_flag"),
        "a matches scrutinee must be lowered: {tokens}"
    );
}

#[test]
fn a_matches_guard_reads_its_pattern_binding() {
    let tokens = lowered(quote!({
        let n = input.wide();
        matches!(input.option(), Some(n) if n > 1)
    }));
    assert!(
        !tokens.contains("observe_clone (__node_n"),
        "a matches guard must see the pattern binding: {tokens}"
    );
}

#[test]
fn a_matches_guard_still_observes_a_visible_local() {
    let tokens = lowered(quote!({
        let limit = input.limit();
        matches!(input.option(), Some(n) if n > limit)
    }));
    assert!(
        tokens.contains("observe_clone (__node_limit"),
        "a matches guard must observe an outer local: {tokens}"
    );
}

#[test]
fn a_match_arm_binding_shadows_a_tracked_local() {
    let tokens = lowered(quote!({
        let n = input.wide();
        match input.narrow() {
            n => n + 1,
        }
    }));
    assert!(
        !tokens.contains("observe_clone (__node_n"),
        "an arm binding must shadow the tracked local: {tokens}"
    );
}

#[test]
fn a_closure_parameter_shadows_a_tracked_local() {
    let tokens = lowered(quote!({
        let n = input.wide();
        let values = vec![1, 2, 3];
        values.iter().map(|n| *n + 1).sum::<u64>()
    }));
    assert!(
        !tokens.contains("observe_clone (__node_n"),
        "a closure parameter must shadow the tracked local: {tokens}"
    );
}

#[test]
fn a_trivial_let_binding_shadows_a_tracked_local() {
    let tokens = lowered(quote!({
        let n = input.wide();
        let n = 5u64;
        n
    }));
    assert!(
        !tokens.contains("observe_clone (__node_n"),
        "a trivial let must shadow the tracked local: {tokens}"
    );
}

#[test]
fn a_visible_tracked_local_is_still_observed() {
    let tokens = lowered(quote!({
        let n = input.wide();
        n + 1
    }));
    assert!(
        tokens.contains("observe_clone (__node_n"),
        "a read outside a binder must observe: {tokens}"
    );
}

#[test]
fn a_bound_iterator_chain_stays_lazy() {
    let tokens = lowered(quote!({
        let values = input.values().to_vec();
        let chain = values.iter();
        chain.map(|value| *value * 2).sum::<u64>()
    }));
    assert_eq!(
        tokens.matches("eval").count(),
        1,
        "only the collection read is memoized; a bound iterator adapter must not \
             become an `eval` node: {tokens}"
    );
}

#[test]
fn a_collection_handle_binding_stays_eager() {
    let tokens = lowered(quote!({
        let handle = input.values();
        handle.map(|value| *value * 2).sum::<u64>()
    }));
    assert_eq!(
        tokens.matches("eval").count(),
        2,
        "a collection adapter on a bound handle is eager work and must stay \
             memoized: {tokens}"
    );
}
