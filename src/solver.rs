use crate::deadline::Deadline;
use crate::error::PackResult;
use crate::geometry::{Dimensions, Point, Rotation};
use crate::model::*;
use crate::solvers::{Candidate, solve_portfolio, solve_portfolio_with_deadline};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct SolverContext<'a> {
    pub request: &'a PackingRequest,
    pub container: &'a Container,
    pub placements: &'a [Placement],
}

#[derive(Clone, Debug)]
pub struct ConstraintDecision {
    pub allowed: bool,
    pub code: Option<String>,
}

impl ConstraintDecision {
    pub fn allow() -> Self {
        Self {
            allowed: true,
            code: None,
        }
    }

    pub fn reject(code: impl Into<String>) -> Self {
        Self {
            allowed: false,
            code: Some(code.into()),
        }
    }
}

pub trait PlacementConstraint: Send + Sync {
    fn evaluate(
        &self,
        context: &SolverContext<'_>,
        item: &ItemInstance,
        point: Point,
        rotation: Rotation,
        dimensions: Dimensions,
    ) -> ConstraintDecision;
}

pub trait CandidateScorer: Send + Sync {
    fn score(
        &self,
        context: &SolverContext<'_>,
        item: &ItemInstance,
        candidate: &Candidate,
    ) -> i128;
}

pub trait ItemOrderStrategy: Send + Sync {
    fn name(&self) -> &str;
    fn order(&self, items: &mut [ItemInstance]);
}

pub trait Solver: Send + Sync {
    fn name(&self) -> &str;
    fn solve(&self, request: &PackingRequest) -> PackResult<PackingResult>;
}

#[derive(Default)]
pub struct SolverRegistry {
    pub constraints: Vec<Arc<dyn PlacementConstraint>>,
    pub scorers: Vec<Arc<dyn CandidateScorer>>,
    pub orders: Vec<Arc<dyn ItemOrderStrategy>>,
    pub solvers: Vec<Arc<dyn Solver>>,
}

impl std::fmt::Debug for SolverRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SolverRegistry")
            .field("constraints", &self.constraints.len())
            .field("scorers", &self.scorers.len())
            .field("orders", &self.orders.len())
            .field("solvers", &self.solvers.len())
            .finish()
    }
}

impl SolverRegistry {
    pub fn solve(&self, request: &PackingRequest) -> PackResult<PackingResult> {
        for solver in &self.solvers {
            let result = solver.solve(request)?;
            if result.complete() {
                return Ok(result);
            }
        }
        solve_portfolio(request, &self.constraints, &self.scorers, &self.orders)
    }

    pub fn solve_with_deadline(
        &self,
        request: &PackingRequest,
        deadline: &Deadline,
    ) -> PackResult<PackingResult> {
        for solver in &self.solvers {
            let result = solver.solve(request)?;
            if result.complete() {
                return Ok(result);
            }
        }
        solve_portfolio_with_deadline(
            request,
            &self.constraints,
            &self.scorers,
            &self.orders,
            deadline,
        )
    }

    pub fn with_constraint(mut self, constraint: Arc<dyn PlacementConstraint>) -> Self {
        self.constraints.push(constraint);
        self
    }

    pub fn with_scorer(mut self, scorer: Arc<dyn CandidateScorer>) -> Self {
        self.scorers.push(scorer);
        self
    }

    pub fn with_order(mut self, order: Arc<dyn ItemOrderStrategy>) -> Self {
        self.orders.push(order);
        self
    }

    pub fn with_solver(mut self, solver: Arc<dyn Solver>) -> Self {
        self.solvers.push(solver);
        self
    }
}
