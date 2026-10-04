//! A search cut off by its deadline at any point still returns a valid result that accounts
//! for every item.
//!
//! The deadline is driven by a clock that advances one millisecond per observation, so the
//! cut-off lands after a fixed number of deadline checks rather than after a wall-clock
//! interval: the same limit stops the same search at the same place on every host. Sweeping
//! the limit walks the cut-off through every deadline check a solver makes, which is how the
//! time-limit branches -- unreachable on a fast host with a real clock -- are exercised.
//! `parallel` is off so the observation order is the program order.

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use packvium_core::{
    Clock, Container, Deadline, IndependentValidator, Item, PackingConfig, PackingRequest,
    PackingStatus, SolverProfile, SolverRegistry, Weight,
};
use support::{SIDE, container, cube, item};

#[derive(Debug, Default)]
struct StepClock(AtomicU64);

impl Clock for StepClock {
    fn now_ns(&self) -> u64 {
        self.0.fetch_add(1_000_000, Ordering::SeqCst)
    }
}

/// The scene every sweep cuts off. The group is optional because the maximal-space walk
/// stands down for grouped items, so a grouped scene would never run it.
fn items(grouped: bool) -> Vec<Item> {
    let mut bricks = item("brick");
    bricks.quantity = 6;
    let mut set = item("set");
    set.quantity = 3;
    if grouped {
        set.group = Some("kit".into());
    }
    set.dimensions = cube(SIDE / 2);
    let mut heavy = item("heavy");
    heavy.quantity = 2;
    heavy.weight = Weight(40 * Weight::TICKS_PER_G);
    let mut giant = item("giant");
    giant.dimensions = cube(10 * SIDE);
    vec![bricks, set, heavy, giant]
}

fn containers() -> Vec<Container> {
    let mut small = container();
    small.id = "small".into();
    small.inner_dimensions = cube(2 * SIDE);
    small.max_payload = Some(Weight(50 * Weight::TICKS_PER_G));
    small.quantity = Some(2);
    let mut large = container();
    large.id = "large".into();
    large.max_payload = Some(Weight(100 * Weight::TICKS_PER_G));
    vec![small, large]
}

fn request(profile: SolverProfile, solvers: &[&str]) -> PackingRequest {
    PackingRequest {
        items: items(solvers.is_empty()),
        containers: containers(),
        config: PackingConfig {
            profile,
            parallel: false,
            solvers: solvers.iter().map(|name| (*name).to_owned()).collect(),
            container_plan_beam_width: if profile == SolverProfile::Quality {
                4
            } else {
                1
            },
            container_plan_node_limit: if profile == SolverProfile::Quality {
                64
            } else {
                1
            },
            // Every instance in the scene, so the exact-small profile's search is admitted
            // rather than refused; the scene is grouped there, which keeps it small.
            exact_item_limit: 12,
            max_containers: Some(3),
            ..PackingConfig::default()
        },
        output_length_unit: "mm".into(),
        output_weight_unit: "g".into(),
        catalog_versions_used: Vec::new(),
        fixed_placements: Vec::new(),
        fixed_containers: Vec::new(),
    }
}

/// How many deadline observations the search makes when nothing cuts it off.
fn observations_to_finish(request: &PackingRequest) -> u64 {
    let clock = Arc::new(StepClock::default());
    let deadline = Deadline::with_clock(u64::MAX / 1_000_000, clock.clone());
    SolverRegistry::default()
        .solve_with_deadline(request, &deadline)
        .expect("the uncut search finishes");
    clock.0.load(Ordering::SeqCst) / 1_000_000
}

/// Cut the search off at evenly spaced points across its whole run.
fn sweep(profile: SolverProfile, solvers: &[&str]) {
    const CUTS: u64 = 60;
    let request = request(profile, solvers);
    let total = observations_to_finish(&request);
    let mut cut_off = 0;
    for step in 1..=CUTS {
        let limit_ms = (total * step / CUTS).max(1);
        let deadline = Deadline::with_clock(limit_ms, Arc::new(StepClock::default()));
        let result = SolverRegistry::default()
            .solve_with_deadline(&request, &deadline)
            .unwrap_or_else(|error| panic!("{solvers:?} at {limit_ms} ms: {error}"));
        let report = IndependentValidator.validate(&request, &result);
        assert!(
            report.valid,
            "{solvers:?} at {limit_ms} ms: {:?}",
            report.issues
        );
        if result.status == PackingStatus::TimeLimit {
            cut_off += 1;
        }
    }
    assert!(cut_off > 0, "{solvers:?}: no limit cut the search off");
}

#[test]
fn each_solver_cut_off_by_its_deadline_still_returns_a_valid_result() {
    for solver in [
        "extreme_points",
        "maximal_spaces",
        "homogeneous_blocks",
        "grid",
        "layer",
    ] {
        sweep(SolverProfile::Balanced, &[solver]);
    }
}

#[test]
fn the_quality_portfolio_cut_off_by_its_deadline_still_returns_a_valid_result() {
    sweep(SolverProfile::Quality, &[]);
    sweep(SolverProfile::Fast, &[]);
    sweep(SolverProfile::ExactSmall, &[]);
}
