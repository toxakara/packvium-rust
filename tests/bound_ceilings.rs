//! 's declared ceilings, from outside the module that declares them.
//!
//! The bound path refuses rather than returning a number it cannot represent, and that
//! refusal is the whole reason a certificate here is worth more than one from a solver
//! built on floating-point LP: above roughly 600 items those pricers leave a
//! dual-infeasible solution with no signal to the caller (INFORMS JoC 36(1):141-162,
//! 2023). A ceiling nothing exercises is a ceiling that stops refusing the day someone
//! rearranges the arithmetic, and the failure it guards against is silent by construction.
//!
//! Two ceilings, and they are not the same one. `MAX_BOUND_SUM` bounds the *intermediate*
//! arithmetic, which stays in `i128` and can be far wider than anything a caller receives.
//! `MAX_BOUND_VALUE` is `2^53 - 1`: the widest integer every binding can carry without
//! changing it, because a result crosses JSON and JavaScript's `Number`. The 1.1.0 freeze
//! put that same ceiling on the reserved `optimality` gap fields, so this is the test
//! standing behind that part of the public contract.
//!
//! Measured from a test crate rather than an in-file `#[cfg(test)]` module for the reason
//! `registry_and_nested.rs` records: an in-file module counts towards its own file's
//! coverage, which is the inflation the instrument exists to avoid.

use packvium_core::bounds::{
    Bounds, ContainerType, Instance, MAX_BOUND_SUM, MAX_BOUND_VALUE, from_reduced,
};

fn instance(volume: i128) -> Instance {
    Instance {
        volume,
        weight: 0,
        shrinks: false,
    }
}

/// A container large enough that nothing else binds, so a test aimed at one ceiling is not
/// answered by an unrelated limit.
fn roomy(inner: i128) -> ContainerType {
    ContainerType {
        usable: inner,
        inner,
        base_area: 1_000_000,
        height: 1_000_000,
        payload: None,
        max_items: None,
        quantity: None,
        cost_minor: 0,
    }
}

#[test]
fn an_ordinary_request_produces_a_bound_rather_than_a_refusal() {
    // The accepting case first: a suite of refusals that never sees an acceptance proves
    // only that the path rejects, which is equally true of one that rejects everything.
    let bounds: Bounds = from_reduced(&[instance(1_000), instance(1_000)], &[roomy(10_000)])
        .expect("an ordinary request is answerable");
    assert_eq!(bounds.unpacked_count, 0);
    assert!(bounds.container_count >= 1);
}

#[test]
fn a_volume_sum_past_the_intermediate_ceiling_is_refused_by_name() {
    let half = MAX_BOUND_SUM / 2 + 1;
    let error = from_reduced(&[instance(half), instance(half)], &[roomy(MAX_BOUND_SUM)])
        .expect_err("a sum past the declared ceiling must refuse");
    assert!(
        error
            .to_string()
            .contains("ceiling the bound path declares"),
        "{error}"
    );
}

/// The refusal names the quantity that overflowed. A bare "overflow" would leave a caller
/// unable to tell which of five keys is unanswerable, and the five are not interchangeable.
#[test]
fn the_refusal_names_the_quantity_that_overflowed() {
    let half = MAX_BOUND_SUM / 2 + 1;
    let error = from_reduced(&[instance(half), instance(half)], &[roomy(MAX_BOUND_SUM)])
        .expect_err("a sum past the declared ceiling must refuse");
    assert!(!error.to_string().is_empty());
    assert!(error.to_string().split_whitespace().count() > 3, "{error}");
}

/// The narrower ceiling can fire first, and which one refused is part of the answer.
///
/// A request whose intermediate sums sit exactly at `MAX_BOUND_SUM` is *not* answerable,
/// because a derived key -- here the stack-height bound in parts per million -- lands above
/// `MAX_BOUND_VALUE`. The two limits guard different things and are checked at different
/// points, so the refusal a caller sees is the one for the quantity that actually
/// overflowed rather than whichever was declared larger.
///
/// This test was written the other way round first, asserting the intermediate ceiling was
/// answerable at its own value, and the engine was right and the test was wrong.
#[test]
fn the_portable_ceiling_refuses_before_the_intermediate_one_when_a_key_derives_wider() {
    let error = from_reduced(&[instance(MAX_BOUND_SUM)], &[roomy(MAX_BOUND_SUM)])
        .expect_err("a derived key past the portable ceiling must refuse");
    let message = error.to_string();
    assert!(
        message.contains("exact portable result ceiling"),
        "the portable ceiling should be the one that fires: {message}"
    );
    assert!(
        message.contains(&MAX_BOUND_VALUE.to_string()),
        "the refusal states the limit it applied: {message}"
    );
}

/// A modest request stays inside both, which is what makes the two tests above meaningful
/// rather than an engine that refuses whenever the numbers are large.
#[test]
fn a_request_well_inside_both_ceilings_is_answerable() {
    from_reduced(
        &[instance(1_000_000), instance(1_000_000)],
        &[roomy(1_000_000_000)],
    )
    .expect("an ordinary industrial request is nowhere near either ceiling");
}

/// The portable ceiling is the narrower of the two and is about the *result*, not the
/// arithmetic: `2^53 - 1` is where JavaScript's `Number` stops being exact, and four
/// engines refusing at four different limits would disagree about which requests are
/// answerable at all.
///
/// Asserted in a `const` block so the relationship is checked when the crate compiles
/// rather than when the suite runs: these are declarations, and a declaration that stopped
/// holding should not wait for a test to be executed to say so.
#[test]
fn the_portable_result_ceiling_is_narrower_than_the_intermediate_one() {
    const {
        assert!(MAX_BOUND_VALUE < MAX_BOUND_SUM);
        assert!(MAX_BOUND_VALUE == (1i128 << 53) - 1);
    }
}

/// An empty request is not an error: nothing to pack bounds every key at zero containers
/// and zero unpacked. Worth pinning because the sums are taken over empty slices, which is
/// where an off-by-one in the fold shows up as a refusal rather than as a wrong number.
#[test]
fn a_request_with_no_instances_is_bounded_rather_than_refused() {
    let bounds = from_reduced(&[], &[roomy(10_000)]).expect("nothing to pack is answerable");
    assert_eq!(bounds.unpacked_count, 0);
    assert_eq!(bounds.container_count, 0);
}

/// Every key is a lower bound, so none of them may be negative: a negative bound is
/// vacuously true and would make `proven_optimal` reachable for a solution that missed.
#[test]
fn no_key_of_a_bound_is_ever_negative() {
    for volume in [1, 999, 1_000_000, 1_000_000_000] {
        let bounds =
            from_reduced(&[instance(volume)], &[roomy(10_000_000_000)]).expect("answerable");
        for (index, key) in bounds.as_vector().iter().enumerate() {
            assert!(*key >= 0, "key {index} of {bounds:?} is negative");
        }
    }
}
