use super::extreme::pack_order;
use super::portfolio::ordering_lead;
use crate::deadline::Deadline;
use crate::model::*;
use crate::solver::{CandidateScorer, PlacementConstraint};
use std::cmp::Reverse;
use std::sync::Arc;

pub fn pack_layer_order(
    request: &PackingRequest,
    items: &[ItemInstance],
    constraints: &[Arc<dyn PlacementConstraint>],
    scorers: &[Arc<dyn CandidateScorer>],
    deadline: &Deadline,
) -> PackingResult {
    let mut ordered = items.to_vec();
    ordered.sort_by_key(|item| {
        (
            ordering_lead(item, &request.config.objective),
            Reverse(item.item.dimensions.base_area()),
            Reverse(item.item.dimensions.volume()),
            Reverse(item.item.weight.0),
            item.id(),
        )
    });
    pack_order(
        request,
        &ordered,
        constraints,
        scorers,
        "layer:base_area",
        deadline,
    )
}
