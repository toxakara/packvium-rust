#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

mod api;
pub mod commerce;
mod contact_graph;
mod deadline;
mod error;
mod explain;
mod geometry;
mod model;
mod nested;
mod policy;
mod rebalance;
mod sequence;
mod solver;
mod solvers;
mod spatial_index;
mod units;
mod validation;

pub use api::{pack_json, pack_request, rebalance_json};
pub use deadline::{Clock, Deadline};
pub use error::{PackError, PackResult};
pub use explain::{
    Explanation, RejectionCode, UnknownReasonError, explain_reason, explain_unpacked_item,
    explanation_for_unpacked_item,
};
pub use geometry::{Aabb, Dimensions, Point, Rotation};
pub use model::{
    AlgorithmReport, Container, Item, ItemInstance, Obstacle, PackedContainer, PackingConfig,
    PackingRequest, PackingResult, PackingStatus, Placement, ReasonProof, RejectionObservation,
    ResultFact, SolverMetrics, SolverProfile, StartRecord, UnpackedItem, aggregate_termination,
};
pub use nested::{NestedLevel, NestedPackingRequest, NestedPackingResult, pack_nested};
pub use policy::{PolicyConstraint, PolicyRule, PolicyRuleSet, RuleForm, ShipmentContext};
pub use rebalance::{RebalanceResult, WeightMove, rebalance_weight};
pub use sequence::{
    ALL_DIRECTIONS, LoadingDependencyGraph, Reachability, SequenceError, SequenceStep,
    SequenceWarning, UnloadingDependencyGraph, placement_reachability, replay_loading_order,
    replay_removal_order, safe_loading_order, safe_loading_order_for_placements,
    safe_loading_order_with_evidence, safe_removal_order, safe_removal_order_with_evidence,
    verify_loading_prefix_business_rules,
};
pub use solver::{
    CandidateScorer, ConstraintDecision, ItemOrderStrategy, PlacementConstraint, Solver,
    SolverContext, SolverRegistry,
};
pub use solvers::Candidate;
pub use units::{Length, Weight};
pub use validation::{IndependentValidator, ValidationIssue, ValidationReport};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
