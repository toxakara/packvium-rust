//! The two typed entry points the coverage ratchet found unexercised.
//!
//! `pack_nested` and `SolverRegistry` are public Rust API that no `cargo test` reached:
//! the first coverage measurement put `nested.rs` at 0% and `solver.rs` at 6.45%. Neither
//! is reachable through `pack_json`, which is why the JSON-boundary suites in
//! `conformance.rs` and `constraints.rs` never touched them — the shared fixtures speak
//! JSON, and these are struct-level APIs.
//!
//! The tests live here rather than in a `#[cfg(test)]` module inside those files on
//! purpose: an in-file test module is compiled into the same source file and counts
//! towards its own coverage, which is the inflation the Python instrument corrects for.
//! Measuring from outside keeps the two numbers honest.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use packvium_core::{
    Candidate, CandidateScorer, ConstraintDecision, Container, Dimensions, Item, ItemInstance,
    ItemOrderStrategy, Length, NestedLevel, NestedPackingRequest, PackResult, PackingConfig,
    PackingRequest, PackingResult, PlacementConstraint, Rotation, Solver, SolverContext,
    SolverRegistry, Weight, pack_nested, pack_request,
};

const MM: i64 = Length::TICKS_PER_MM;

fn item(id: &str, side_mm: i64) -> Item {
    Item {
        id: id.into(),
        dimensions: Dimensions {
            length: Length(side_mm * MM),
            width: Length(side_mm * MM),
            height: Length(side_mm * MM),
        },
        weight: Weight(0),
        quantity: 1,
        allowed_rotations: vec![Rotation::Lwh],
        stackable: true,
        must_be_on_floor: false,
        max_top_load: None,
        minimum_support_ratio: 0.0,
        group: None,
        tags: BTreeSet::new(),
        incompatible_tags: BTreeSet::new(),
        priority: 0,
        metadata: BTreeMap::new(),
        nesting_height: None,
        max_stacked_items: None,
        ground_contact_rule: None,
        stop_index: None,
        eligible_container_tags: BTreeSet::new(),
        value: None,
    }
}

fn container(id: &str, side_mm: i64) -> Container {
    Container {
        id: id.into(),
        inner_dimensions: Dimensions {
            length: Length(side_mm * MM),
            width: Length(side_mm * MM),
            height: Length(side_mm * MM),
        },
        outer_dimensions: None,
        tare_weight: Weight(0),
        max_payload: None,
        cost_minor: 0,
        quantity: None,
        obstacles: Vec::new(),
        tags: BTreeSet::new(),
        max_items: None,
        metadata: BTreeMap::new(),
        axles: None,
        void_fill_reserve_ppm: 0,
        tag_limits: BTreeMap::new(),
        max_stack_density: None,
        rate_table: None,
    }
}

fn request(items: Vec<Item>, containers: Vec<Container>) -> PackingRequest {
    PackingRequest {
        items,
        containers,
        config: PackingConfig::default(),
        output_length_unit: "mm".into(),
        output_weight_unit: "kg".into(),
        catalog_versions_used: Vec::new(),
    }
}

fn one_fitting_item() -> PackingRequest {
    request(vec![item("a", 10)], vec![container("box", 100)])
}

// --------------------------------------------------------------------- pack_nested

#[test]
fn nested_levels_run_in_the_order_they_were_supplied() {
    let nested = NestedPackingRequest {
        levels: vec![
            NestedLevel {
                name: "inner".into(),
                request: one_fitting_item(),
            },
            NestedLevel {
                name: "outer".into(),
                request: request(vec![item("b", 20)], vec![container("pallet", 200)]),
            },
        ],
        beam_width: 1,
    };

    let result = pack_nested(&nested).expect("both levels are packable");

    // Order is the contract: docs call these "explicitly supplied levels in order", and
    // a map-shaped result would lose it. Asserting the names in sequence is what
    // distinguishes that promise from "returns one result per level".
    let names: Vec<&str> = result
        .levels
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(names, ["inner", "outer"]);
    assert!(result.levels.iter().all(|(_, packed)| packed.complete()));
}

#[test]
fn each_nested_level_returns_exactly_what_packing_it_alone_returns() {
    // 0.1.0 deliberately does not transform between levels: a level's result must not
    // depend on which levels preceded it. That is the property a future beam-search
    // implementation would break, so it is worth pinning now rather than after.
    let alone = pack_request(&one_fitting_item()).expect("single level packs");
    let nested = NestedPackingRequest {
        levels: vec![
            NestedLevel {
                name: "first".into(),
                request: request(vec![item("z", 30)], vec![container("crate", 90)]),
            },
            NestedLevel {
                name: "second".into(),
                request: one_fitting_item(),
            },
        ],
        beam_width: 4,
    };

    let result = pack_nested(&nested).expect("both levels are packable");

    let (_, second) = &result.levels[1];
    assert_eq!(second.score, alone.score);
    assert_eq!(second.packed_item_count(), alone.packed_item_count());
}

#[test]
fn a_failing_level_aborts_the_run_instead_of_returning_a_partial_result() {
    let nested = NestedPackingRequest {
        levels: vec![
            NestedLevel {
                name: "ok".into(),
                request: one_fitting_item(),
            },
            // No containers: `validate_request` rejects this outright, rather than
            // reporting the items as unpacked.
            NestedLevel {
                name: "broken".into(),
                request: request(vec![item("a", 10)], vec![]),
            },
            NestedLevel {
                name: "never-reached".into(),
                request: one_fitting_item(),
            },
        ],
        beam_width: 1,
    };

    let error = pack_nested(&nested).expect_err("the second level cannot be validated");

    assert!(
        error
            .to_string()
            .contains("items and containers are required"),
        "the level's own error must survive, not be flattened into a generic one: {error}"
    );
}

#[test]
fn a_run_with_no_levels_succeeds_and_packs_nothing() {
    let nested = NestedPackingRequest {
        levels: Vec::new(),
        beam_width: 0,
    };

    let result = pack_nested(&nested).expect("an empty plan is not an error");

    assert!(result.levels.is_empty());
}

// ------------------------------------------------------------------ SolverRegistry

#[derive(Debug)]
struct CountingSolver {
    name: &'static str,
    calls: Arc<AtomicUsize>,
    /// When false the solver returns a result with something still unpacked, which is
    /// what makes `PackingResult::complete()` false and the registry keep looking.
    complete: bool,
}

impl Solver for CountingSolver {
    fn name(&self) -> &str {
        self.name
    }

    fn solve(&self, request: &PackingRequest) -> PackResult<PackingResult> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut result = pack_request(request)?;
        if !self.complete {
            // Borrow a genuine unpacked entry from a request that cannot be satisfied,
            // rather than hand-building one: the registry only reads `complete()`, but a
            // synthetic result that no solver could produce would prove less.
            let oversized = request_that_cannot_be_packed();
            result.unpacked = pack_request(&oversized)?.unpacked;
        }
        Ok(result)
    }
}

fn request_that_cannot_be_packed() -> PackingRequest {
    request(vec![item("too-big", 500)], vec![container("small", 10)])
}

#[derive(Debug)]
struct RecordingConstraint {
    consulted: Arc<AtomicUsize>,
}

impl PlacementConstraint for RecordingConstraint {
    fn evaluate(
        &self,
        _context: &SolverContext<'_>,
        _item: &ItemInstance,
        _point: packvium_core::Point,
        _rotation: Rotation,
        _dimensions: Dimensions,
    ) -> ConstraintDecision {
        self.consulted.fetch_add(1, Ordering::SeqCst);
        ConstraintDecision::allow()
    }
}

#[derive(Debug)]
struct RecordingScorer {
    consulted: Arc<AtomicUsize>,
}

impl CandidateScorer for RecordingScorer {
    fn score(
        &self,
        _context: &SolverContext<'_>,
        _item: &ItemInstance,
        _candidate: &Candidate,
    ) -> i128 {
        self.consulted.fetch_add(1, Ordering::SeqCst);
        0
    }
}

#[derive(Debug)]
struct RecordingOrder {
    consulted: Arc<AtomicUsize>,
}

impl ItemOrderStrategy for RecordingOrder {
    fn name(&self) -> &str {
        "identity"
    }

    fn order(&self, _items: &mut [ItemInstance]) {
        self.consulted.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Debug)]
struct RejectEveryPlacement {
    consulted: Arc<AtomicUsize>,
}

impl PlacementConstraint for RejectEveryPlacement {
    fn evaluate(
        &self,
        _context: &SolverContext<'_>,
        _item: &ItemInstance,
        _point: packvium_core::Point,
        _rotation: Rotation,
        _dimensions: Dimensions,
    ) -> ConstraintDecision {
        self.consulted.fetch_add(1, Ordering::SeqCst);
        ConstraintDecision::reject("test_rejection")
    }
}

#[test]
fn a_decision_carries_its_rejection_code_and_an_allowance_carries_none() {
    let allowed = ConstraintDecision::allow();
    assert!(allowed.allowed);
    assert_eq!(allowed.code, None);

    let rejected = ConstraintDecision::reject("support_insufficient");
    assert!(!rejected.allowed);
    assert_eq!(rejected.code.as_deref(), Some("support_insufficient"));
}

#[test]
fn the_builder_accumulates_every_registered_extension() {
    let consulted = Arc::new(AtomicUsize::new(0));
    let registry = SolverRegistry::default()
        .with_constraint(Arc::new(RecordingConstraint {
            consulted: Arc::clone(&consulted),
        }))
        .with_scorer(Arc::new(RecordingScorer {
            consulted: Arc::clone(&consulted),
        }))
        .with_order(Arc::new(RecordingOrder {
            consulted: Arc::clone(&consulted),
        }))
        .with_solver(Arc::new(CountingSolver {
            name: "counting",
            calls: Arc::new(AtomicUsize::new(0)),
            complete: true,
        }));

    assert_eq!(registry.constraints.len(), 1);
    assert_eq!(registry.scorers.len(), 1);
    assert_eq!(registry.orders.len(), 1);
    assert_eq!(registry.solvers.len(), 1);
    // The hand-written Debug impl reports lengths rather than the trait objects, which
    // have no Debug of their own; a derive here would not compile.
    let rendered = format!("{registry:?}");
    assert!(rendered.contains("constraints: 1"), "{rendered}");
    assert!(rendered.contains("solvers: 1"), "{rendered}");
}

#[test]
fn a_complete_solver_result_short_circuits_the_rest_of_the_chain() {
    let first = Arc::new(AtomicUsize::new(0));
    let second = Arc::new(AtomicUsize::new(0));
    let registry = SolverRegistry::default()
        .with_solver(Arc::new(CountingSolver {
            name: "first",
            calls: Arc::clone(&first),
            complete: true,
        }))
        .with_solver(Arc::new(CountingSolver {
            name: "second",
            calls: Arc::clone(&second),
            complete: true,
        }));

    let result = registry
        .solve(&one_fitting_item())
        .expect("the first solver answers");

    assert!(result.complete());
    assert_eq!(first.load(Ordering::SeqCst), 1);
    assert_eq!(
        second.load(Ordering::SeqCst),
        0,
        "the documented precedence is first-complete-wins; a second call means the chain ran on"
    );
}

#[test]
fn an_incomplete_solver_result_falls_through_to_the_portfolio() {
    let attempted = Arc::new(AtomicUsize::new(0));
    let registry = SolverRegistry::default().with_solver(Arc::new(CountingSolver {
        name: "gives-up",
        calls: Arc::clone(&attempted),
        complete: false,
    }));

    let result = registry
        .solve(&one_fitting_item())
        .expect("the portfolio finishes the job");

    assert_eq!(
        attempted.load(Ordering::SeqCst),
        1,
        "the registered solver must be tried first"
    );
    assert!(
        result.complete(),
        "the portfolio packs what the registered solver would not"
    );
}

#[test]
fn the_deadline_entry_point_also_falls_through_to_the_portfolio() {
    use packvium_core::Deadline;

    // The mirror of the test above, and not redundant with it: `solve_with_deadline`
    // duplicates the chain rather than delegating to `solve`, so its fallback is a
    // separate branch. Covering only the short-circuit left those lines unexecuted --
    // which is how this test came to exist, from reading the uncovered lines rather than
    // from guessing what else to assert.
    let attempted = Arc::new(AtomicUsize::new(0));
    let registry = SolverRegistry::default().with_solver(Arc::new(CountingSolver {
        name: "gives-up",
        calls: Arc::clone(&attempted),
        complete: false,
    }));

    let result = registry
        .solve_with_deadline(&one_fitting_item(), &Deadline::new(300_000))
        .expect("the portfolio finishes the job");

    assert_eq!(attempted.load(Ordering::SeqCst), 1);
    assert!(result.complete());
}

/// Two item types: enough to disqualify the uniform-lattice fast path, which is the only
/// portfolio entry that takes no constraints (see the test below).
fn two_item_types() -> PackingRequest {
    request(
        vec![item("a", 10), item("b", 12)],
        vec![container("box", 100)],
    )
}

#[test]
fn the_portfolio_invokes_every_registered_candidate_extension() {
    let constraint_calls = Arc::new(AtomicUsize::new(0));
    let scorer_calls = Arc::new(AtomicUsize::new(0));
    let order_calls = Arc::new(AtomicUsize::new(0));
    let registry = SolverRegistry::default()
        .with_constraint(Arc::new(RecordingConstraint {
            consulted: Arc::clone(&constraint_calls),
        }))
        .with_scorer(Arc::new(RecordingScorer {
            consulted: Arc::clone(&scorer_calls),
        }))
        .with_order(Arc::new(RecordingOrder {
            consulted: Arc::clone(&order_calls),
        }));

    let result = registry.solve(&two_item_types()).expect("both items fit");

    assert!(result.complete());
    for (name, calls) in [
        ("constraint", constraint_calls.load(Ordering::SeqCst)),
        ("scorer", scorer_calls.load(Ordering::SeqCst)),
        ("order", order_calls.load(Ordering::SeqCst)),
    ] {
        assert!(calls > 0, "the registered {name} was never invoked");
    }
}

#[test]
fn the_uniform_lattice_fast_path_cannot_bypass_a_registered_constraint() {
    let consulted = Arc::new(AtomicUsize::new(0));
    let registry = SolverRegistry::default().with_constraint(Arc::new(RejectEveryPlacement {
        consulted: Arc::clone(&consulted),
    }));

    let result = registry
        .solve(&one_fitting_item())
        .expect("a constraint rejection is a valid incomplete result");

    assert!(!result.complete());
    assert_eq!(result.packed_item_count(), 0);
    assert!(
        consulted.load(Ordering::SeqCst) > 0,
        "a hard constraint must steer search before a placement is committed"
    );
}

#[test]
fn the_deadline_entry_point_applies_the_same_precedence() {
    use packvium_core::Deadline;

    let first = Arc::new(AtomicUsize::new(0));
    let second = Arc::new(AtomicUsize::new(0));
    let registry = SolverRegistry::default()
        .with_solver(Arc::new(CountingSolver {
            name: "first",
            calls: Arc::clone(&first),
            complete: true,
        }))
        .with_solver(Arc::new(CountingSolver {
            name: "second",
            calls: Arc::clone(&second),
            complete: true,
        }));

    // Generous on purpose: this asserts precedence, not deadline behaviour, and a budget
    // tight enough to expire would make the test a timing race on a loaded host.
    let result = registry
        .solve_with_deadline(&one_fitting_item(), &Deadline::new(300_000))
        .expect("the first solver answers");

    assert!(result.complete());
    assert_eq!(
        (first.load(Ordering::SeqCst), second.load(Ordering::SeqCst)),
        (1, 0)
    );
}

#[test]
fn an_empty_registry_still_packs_through_the_portfolio() {
    // The registry's own default is the shape every caller starts from; if it could not
    // solve, `with_*` would not be optional and the extension point would be mandatory
    // configuration instead.
    let result = SolverRegistry::default()
        .solve(&one_fitting_item())
        .expect("the portfolio needs no registered extensions");

    assert!(result.complete());
    assert_eq!(
        result.algorithm.solver, "grid",
        "the  guard must not disable the fast path when no constraint is registered"
    );
}

#[test]
fn a_solver_error_propagates_instead_of_falling_through_to_the_portfolio() {
    #[derive(Debug)]
    struct FailingSolver;

    impl Solver for FailingSolver {
        fn name(&self) -> &str {
            "failing"
        }

        fn solve(&self, _request: &PackingRequest) -> PackResult<PackingResult> {
            pack_request(&request(vec![item("a", 10)], vec![]))
        }
    }

    let registry = SolverRegistry::default().with_solver(Arc::new(FailingSolver));

    let error = registry
        .solve(&one_fitting_item())
        .expect_err("a solver's error must not be swallowed by the fallback");

    assert!(
        error
            .to_string()
            .contains("items and containers are required"),
        "{error}"
    );
}
