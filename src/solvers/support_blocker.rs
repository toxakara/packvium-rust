//! Whether the support rules alone left an item out of a finished layout.
//!
//! Python's and PHP's `support_is_the_blocker`: replay each packed container's final scene
//! and ask whether the item fits once the minimum support ratio and the ground contact rule
//! are set aside, but not with them. It is a fact observed of this one layout, not a proof
//! that no layout would hold the item, so the reason it names carries proof `observed`.

use super::extreme::{ContainerState, PointSet, find_candidates};
use crate::deadline::Deadline;
use crate::model::*;
use crate::solver::PlacementConstraint;
use std::sync::Arc;

/// True when a support rule can refuse `item` at all: an effective minimum ratio above 0,
/// or a ground contact rule other than `free`. Without one the replay cannot find a support
/// refusal, so it is not run. O(1).
pub(crate) fn support_can_refuse(item: &Item, config: &PackingConfig) -> bool {
    item.minimum_support_ratio.max(config.minimum_support_ratio) > 0.0
        || !matches!(item.ground_contact_rule.as_deref(), None | Some("free"))
}

/// One finished layout, replayed to explain the items the search left out.
pub(crate) struct SupportReplay<'a> {
    request: &'a PackingRequest,
    /// `request` with no global support ratio, built once for every item asked about.
    relaxed: PackingRequest,
    scenes: Vec<ContainerState>,
    constraints: &'a [Arc<dyn PlacementConstraint>],
}

impl<'a> SupportReplay<'a> {
    /// Rebuilds the final scene of every packed container: O(P) placements committed in
    /// all, each at the cost the solvers commit one, plus one copy of the request.
    pub(crate) fn new(
        containers: &[PackedContainer],
        request: &'a PackingRequest,
        constraints: &'a [Arc<dyn PlacementConstraint>],
    ) -> Self {
        let scenes = containers
            .iter()
            .map(|packed| {
                let mut container = packed.container.clone();
                container.preloaded = packed.expand_placements();
                ContainerState::new(container, packed.sequence)
            })
            .collect();
        let mut relaxed = request.clone();
        relaxed.config.minimum_support_ratio = 0.0;
        Self {
            request,
            relaxed,
            scenes,
            constraints,
        }
    }

    /// True when some scene admits `item` with the support rules set aside and refuses it
    /// with them. Two single-candidate scans per scene, `O(S * K)` for S scenes and K the
    /// cost of one scan; neither reads the clock, so every host gives the same answer.
    pub(crate) fn support_is_the_blocker(&self, item: &ItemInstance) -> bool {
        let mut unsupported = item.clone();
        unsupported.item.minimum_support_ratio = 0.0;
        unsupported.item.ground_contact_rule = None;
        self.scenes.iter().any(|state| {
            self.fits(state, &unsupported, &self.relaxed) && !self.fits(state, item, self.request)
        })
    }

    fn fits(&self, state: &ContainerState, item: &ItemInstance, request: &PackingRequest) -> bool {
        !find_candidates(
            state,
            item,
            request,
            self.constraints,
            &[],
            1,
            PointSet::Sparse,
            &Deadline::new(u64::MAX),
            &mut SolverMetrics::default(),
        )
        .is_empty()
    }
}
