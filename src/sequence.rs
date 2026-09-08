//! Deterministic loading and unloading dependency graphs.
//!
//! Loading starts from an empty container and follows supporter dependencies.
//! Unloading starts from the final scene and follows the inverse (children)
//! dependencies. Accessibility is checked by sweeping the moving box to one of the
//! six container walls.

use crate::geometry::{self, Aabb, Dimensions};
use crate::model::{Container, PackedContainer, Placement};
use crate::validation::{ground_contact_valid, validate_stack_counts, validate_top_loads};
use std::collections::BTreeSet;
use thiserror::Error;

/// Re-exported: the vocabulary lives in `geometry` so the solver can share it without
/// reaching into this module, which is post-hoc analysis rather than search geometry.
pub const ALL_DIRECTIONS: [&str; 6] = geometry::ALL_DIRECTIONS;

#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum SequenceError {
    #[error("unknown movement direction {0:?}")]
    InvalidDirection(String),
    #[error("no safe order exists; stuck placements: {stuck:?}")]
    Stuck { stuck: Vec<usize> },
    #[error("step {step}: placement {index} is not safe there ({reason})")]
    Replay {
        index: isize,
        step: isize,
        reason: &'static str,
    },
}

impl SequenceError {
    /// The stable, forward-compatible discriminator carried alongside this
    /// error's own fields -- shared with the Python, PHP and JavaScript
    /// implementations.
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidDirection(_) => "invalid_direction",
            Self::Stuck { .. } => "sequence_stuck",
            Self::Replay { .. } => "sequence_replay",
        }
    }

    /// Canonical JSON shape shared with the Python, PHP and JavaScript
    /// implementations, matching
    /// `conformance/scene/sequence-fixtures.json`'s `expected_error`.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Self::InvalidDirection(direction) => serde_json::json!({
                "code": self.code(),
                "direction": direction,
            }),
            Self::Stuck { stuck } => {
                let mut sorted = stuck.clone();
                sorted.sort_unstable();
                serde_json::json!({
                    "code": self.code(),
                    "stuck": sorted,
                })
            }
            Self::Replay {
                index,
                step,
                reason,
            } => serde_json::json!({
                "code": self.code(),
                "index": index,
                "step": step,
                "reason": reason,
            }),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnloadingDependencyGraph {
    pub depends_on: Vec<BTreeSet<usize>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadingDependencyGraph {
    pub depends_on: Vec<BTreeSet<usize>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SequenceStep {
    pub index: usize,
    pub direction: String,
    pub depends_on: Vec<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reachability {
    pub index: usize,
    pub reachable: bool,
    pub blocked_by_support: Vec<usize>,
    pub blocked_by_neighbors: Vec<usize>,
    pub blocked_by_route: Vec<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SequenceWarning {
    pub code: String,
    pub index: usize,
    pub message_key: String,
    pub arguments: std::collections::BTreeMap<String, String>,
}

impl SequenceWarning {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "code": self.code,
            "index": self.index,
            "message_key": self.message_key,
            "arguments": self.arguments,
        })
    }
}

impl Reachability {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "index": self.index,
            "reachable": self.reachable,
            "blocked_by_support": self.blocked_by_support,
            "blocked_by_neighbors": self.blocked_by_neighbors,
            "blocked_by_route": self.blocked_by_route,
        })
    }
}

impl SequenceStep {
    /// Canonical JSON shape shared with the Python, PHP and JavaScript
    /// implementations: `index`, `direction` and `depends_on` as a
    /// sorted list, matching `conformance/scene/sequence-fixtures.json`.
    pub fn to_json(&self) -> serde_json::Value {
        let mut depends_on = self.depends_on.clone();
        depends_on.sort_unstable();
        serde_json::json!({
            "index": self.index,
            "direction": self.direction,
            "depends_on": depends_on,
        })
    }
}

fn validate_directions(directions: &[&str]) -> Result<(), SequenceError> {
    for direction in directions {
        if !ALL_DIRECTIONS.contains(direction) {
            return Err(SequenceError::InvalidDirection((*direction).to_owned()));
        }
    }
    Ok(())
}

fn supporters(boxes: &[Aabb], upper: usize) -> BTreeSet<usize> {
    boxes
        .iter()
        .enumerate()
        .filter_map(|(index, lower)| {
            (index != upper
                && lower.z2() == boxes[upper].origin.z
                && lower.overlap_area_xy(boxes[upper]) > 0)
                .then_some(index)
        })
        .collect()
}

impl LoadingDependencyGraph {
    pub fn build(boxes: &[Aabb]) -> Self {
        Self {
            depends_on: (0..boxes.len())
                .map(|index| supporters(boxes, index))
                .collect(),
        }
    }

    pub fn is_acyclic(&self) -> bool {
        is_acyclic(&self.depends_on)
    }
}

impl UnloadingDependencyGraph {
    pub fn build(boxes: &[Aabb]) -> Self {
        let loading = LoadingDependencyGraph::build(boxes);
        let mut depends_on = vec![BTreeSet::new(); boxes.len()];
        for (upper, lower_indexes) in loading.depends_on.iter().enumerate() {
            for lower in lower_indexes {
                depends_on[*lower].insert(upper);
            }
        }
        Self { depends_on }
    }

    pub fn is_acyclic(&self) -> bool {
        is_acyclic(&self.depends_on)
    }
}

fn is_acyclic(depends_on: &[BTreeSet<usize>]) -> bool {
    fn visit(
        node: usize,
        depends_on: &[BTreeSet<usize>],
        visiting: &mut BTreeSet<usize>,
        visited: &mut BTreeSet<usize>,
    ) -> bool {
        if visited.contains(&node) {
            return true;
        }
        if !visiting.insert(node) {
            return false;
        }
        for dependency in &depends_on[node] {
            if *dependency >= depends_on.len() || !visit(*dependency, depends_on, visiting, visited)
            {
                return false;
            }
        }
        visiting.remove(&node);
        visited.insert(node);
        true
    }

    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    (0..depends_on.len()).all(|node| visit(node, depends_on, &mut visiting, &mut visited))
}

fn swept_volume(
    box_: Aabb,
    container: Dimensions,
    direction: &str,
) -> Result<(i64, i64, i64, i64, i64, i64), SequenceError> {
    geometry::swept_volume(box_, container, direction)
        .ok_or_else(|| SequenceError::InvalidDirection(direction.to_owned()))
}

fn clear_direction(
    index: usize,
    boxes: &[Aabb],
    present: &BTreeSet<usize>,
    container: Dimensions,
    directions: &[&str],
) -> Result<Option<String>, SequenceError> {
    'directions: for direction in directions {
        let (x1, y1, z1, x2, y2, z2) = swept_volume(boxes[index], container, direction)?;
        for other_index in present {
            if *other_index == index {
                continue;
            }
            let other = boxes[*other_index];
            if x1 < other.x2()
                && other.origin.x < x2
                && y1 < other.y2()
                && other.origin.y < y2
                && z1 < other.z2()
                && other.origin.z < z2
            {
                continue 'directions;
            }
        }
        return Ok(Some((*direction).to_owned()));
    }
    Ok(None)
}

fn blocking_indices(
    index: usize,
    boxes: &[Aabb],
    present: &BTreeSet<usize>,
    container: Dimensions,
    direction: &str,
) -> Result<BTreeSet<usize>, SequenceError> {
    let (x1, y1, z1, x2, y2, z2) = swept_volume(boxes[index], container, direction)?;
    Ok(present
        .iter()
        .copied()
        .filter(|other_index| {
            if *other_index == index {
                return false;
            }
            let other = boxes[*other_index];
            x1 < other.x2()
                && other.origin.x < x2
                && y1 < other.y2()
                && other.origin.y < y2
                && z1 < other.z2()
                && other.origin.z < z2
        })
        .collect())
}

fn validate_box(
    index: usize,
    step: usize,
    boxes: &[Aabb],
    present: &BTreeSet<usize>,
    container: Dimensions,
) -> Result<(), SequenceError> {
    let box_ = boxes[index];
    if box_.origin.x < 0
        || box_.origin.y < 0
        || box_.origin.z < 0
        || box_.x2() > container.length.0
        || box_.y2() > container.width.0
        || box_.z2() > container.height.0
    {
        return Err(SequenceError::Replay {
            index: index as isize,
            step: step as isize,
            reason: "placement is outside the container",
        });
    }
    if present
        .iter()
        .any(|other| *other != index && box_.intersects(boxes[*other]))
    {
        return Err(SequenceError::Replay {
            index: index as isize,
            step: step as isize,
            reason: "placement collides with an already present placement",
        });
    }
    Ok(())
}

fn validate_permutation(boxes: &[Aabb], order: &[usize]) -> Result<(), SequenceError> {
    let mut sorted = order.to_vec();
    sorted.sort_unstable();
    if sorted != (0..boxes.len()).collect::<Vec<_>>() {
        return Err(SequenceError::Replay {
            index: -1,
            step: -1,
            reason: "order is not a permutation of every placement index exactly once",
        });
    }
    Ok(())
}

pub fn safe_removal_order(
    boxes: &[Aabb],
    container: Dimensions,
    directions: &[&str],
) -> Result<Vec<usize>, SequenceError> {
    validate_directions(directions)?;
    let graph = UnloadingDependencyGraph::build(boxes);
    let mut present = (0..boxes.len()).collect::<BTreeSet<_>>();
    let mut order = Vec::with_capacity(boxes.len());
    while !present.is_empty() {
        let mut chosen = None;
        for index in &present {
            if graph.depends_on[*index].is_disjoint(&present)
                && clear_direction(*index, boxes, &present, container, directions)?.is_some()
            {
                chosen = Some(*index);
                break;
            }
        }
        let Some(index) = chosen else {
            return Err(SequenceError::Stuck {
                stuck: present.into_iter().collect(),
            });
        };
        order.push(index);
        present.remove(&index);
    }
    replay_removal_order(boxes, container, &order, directions)?;
    Ok(order)
}

pub fn safe_loading_order(
    boxes: &[Aabb],
    container: Dimensions,
    directions: &[&str],
) -> Result<Vec<usize>, SequenceError> {
    let mut order = safe_removal_order(boxes, container, directions)?;
    order.reverse();
    replay_loading_order(boxes, container, &order, directions)?;
    Ok(order)
}

/// Snapshot reachability for the complete scene.
///
/// O(n²) time and O(n²) evidence space in the worst case: each of the fixed six
/// sweeps can inspect every other placement and the returned blocker sets may be dense.
pub fn placement_reachability(
    boxes: &[Aabb],
    container: Dimensions,
    stops: Option<&[Option<i64>]>,
    directions: &[&str],
) -> Result<Vec<Reachability>, SequenceError> {
    validate_directions(directions)?;
    if stops.is_some_and(|values| values.len() != boxes.len()) {
        return Err(SequenceError::Replay {
            index: -1,
            step: -1,
            reason: "stops must contain exactly one entry per placement",
        });
    }
    let graph = UnloadingDependencyGraph::build(boxes);
    let present = (0..boxes.len()).collect::<BTreeSet<_>>();
    let stops = stops
        .map(<[_]>::to_vec)
        .unwrap_or_else(|| vec![None; boxes.len()]);
    let earliest = stops.iter().flatten().copied().min();
    let mut result = Vec::with_capacity(boxes.len());
    for index in 0..boxes.len() {
        let blocked_by_support = graph.depends_on[index]
            .intersection(&present)
            .copied()
            .collect::<Vec<_>>();
        let blocked_by_route = match (stops[index], earliest) {
            (Some(stop), Some(first)) if stop != first => present
                .iter()
                .copied()
                .filter(|other| {
                    *other != index && stops[*other].is_some_and(|other_stop| other_stop < stop)
                })
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        let clear = clear_direction(index, boxes, &present, container, directions)?;
        let blocked_by_neighbors = if clear.is_none() && !directions.is_empty() {
            let mut blockers = BTreeSet::new();
            for direction in directions {
                blockers.extend(blocking_indices(
                    index, boxes, &present, container, direction,
                )?);
            }
            blockers.into_iter().collect()
        } else {
            Vec::new()
        };
        result.push(Reachability {
            index,
            reachable: blocked_by_support.is_empty()
                && blocked_by_route.is_empty()
                && clear.is_some(),
            blocked_by_support,
            blocked_by_neighbors,
            blocked_by_route,
        });
    }
    Ok(result)
}

pub fn safe_loading_order_for_placements(
    placements: &[Placement],
    container: &Container,
    directions: &[&str],
) -> Result<Vec<usize>, SequenceError> {
    let boxes = placements
        .iter()
        .map(Placement::envelope_box)
        .collect::<Vec<_>>();
    let order = safe_loading_order(&boxes, container.inner_dimensions, directions)?;
    verify_loading_prefix_business_rules(placements, &order, container)?;
    Ok(order)
}

pub fn replay_removal_order(
    boxes: &[Aabb],
    container: Dimensions,
    order: &[usize],
    directions: &[&str],
) -> Result<(), SequenceError> {
    validate_directions(directions)?;
    validate_permutation(boxes, order)?;
    let graph = UnloadingDependencyGraph::build(boxes);
    let mut present = (0..boxes.len()).collect::<BTreeSet<_>>();
    for (step, index) in order.iter().copied().enumerate() {
        validate_box(index, step, boxes, &present, container)?;
        if !graph.depends_on[index].is_disjoint(&present) {
            return Err(SequenceError::Replay {
                index: index as isize,
                step: step as isize,
                reason: "something still resting on it has not been removed yet",
            });
        }
        if clear_direction(index, boxes, &present, container, directions)?.is_none() {
            return Err(SequenceError::Replay {
                index: index as isize,
                step: step as isize,
                reason: "no allowed direction is clear of the remaining placements",
            });
        }
        present.remove(&index);
    }
    Ok(())
}

pub fn replay_loading_order(
    boxes: &[Aabb],
    container: Dimensions,
    order: &[usize],
    directions: &[&str],
) -> Result<(), SequenceError> {
    validate_directions(directions)?;
    validate_permutation(boxes, order)?;
    let graph = LoadingDependencyGraph::build(boxes);
    let mut present = BTreeSet::new();
    for (step, index) in order.iter().copied().enumerate() {
        validate_box(index, step, boxes, &present, container)?;
        if !graph.depends_on[index].is_subset(&present) {
            return Err(SequenceError::Replay {
                index: index as isize,
                step: step as isize,
                reason: "a supporter has not been loaded yet",
            });
        }
        if clear_direction(index, boxes, &present, container, directions)?.is_none() {
            return Err(SequenceError::Replay {
                index: index as isize,
                step: step as isize,
                reason: "no allowed direction is clear of what has already been loaded",
            });
        }
        present.insert(index);
    }
    Ok(())
}

/// Independently reuse the exact constraint calculations already proven for
/// a finished scene (`validation::validate_top_loads`, `validate_stack_counts`,
/// `ground_contact_valid`) against every loading *prefix*, not only the final state.
/// Additive to `replay_loading_order` above rather than a change to it or to any of
/// the bare-geometry functions in this module, so every existing caller keeps working
/// unmodified.
///
/// Returns `Err(SequenceError::Replay)` at the first step whose prefix violates a
/// limit -- pinned to that step even though `top_load`/`max_stacked_items`/
/// `stack_density` only ever accumulate as loading proceeds (a violation present at
/// step k is also present in the final scene): identifying *which* addition first
/// broke a limit is strictly more useful than "the finished scene is invalid" alone,
/// and is the reason this walks the prefix sequence instead of checking only the
/// last step. `reason` values match `ValidationIssue::code` from the reused
/// functions ("top_load", "non_stackable", "stack_density_exceeded",
/// "max_stacked_items_exceeded", "ground_contact_violation") rather than inventing a
/// parallel vocabulary for the same finding.
pub fn verify_loading_prefix_business_rules(
    placements: &[Placement],
    order: &[usize],
    container: &Container,
) -> Result<(), SequenceError> {
    let mut sorted = order.to_vec();
    sorted.sort_unstable();
    if order.len() != placements.len() || sorted != (0..placements.len()).collect::<Vec<_>>() {
        return Err(SequenceError::Replay {
            index: -1,
            step: -1,
            reason: "order is not a permutation of every placement index exactly once",
        });
    }
    let mut present: Vec<Placement> = Vec::new();
    for (step, index) in order.iter().copied().enumerate() {
        present.push(placements[index].clone());
        let scratch = PackedContainer {
            container: container.clone(),
            sequence: 0,
            placements: present.clone(),
            lattice_summary: None,
            lattice_items: Vec::new(),
        };
        let mut issues = Vec::new();
        validate_top_loads(&scratch, &mut issues);
        validate_stack_counts(&scratch, &mut issues);
        if !ground_contact_valid(&scratch, present.len() - 1) {
            issues.push(crate::validation::ValidationIssue {
                code: "ground_contact_violation".into(),
                message: present.last().expect("just pushed").instance.id(),
            });
        }
        if let Some(problem) = issues.first() {
            let reason: &'static str = match problem.code.as_str() {
                "top_load" => "top_load",
                "non_stackable" => "non_stackable",
                "stack_density_exceeded" => "stack_density_exceeded",
                "max_stacked_items_exceeded" => "max_stacked_items_exceeded",
                "ground_contact_violation" => "ground_contact_violation",
                _ => "business rule violated",
            };
            return Err(SequenceError::Replay {
                index: index as isize,
                step: step as isize,
                reason,
            });
        }
    }
    Ok(())
}

pub fn safe_loading_order_with_evidence(
    boxes: &[Aabb],
    container: Dimensions,
    directions: &[&str],
) -> Result<Vec<SequenceStep>, SequenceError> {
    let order = safe_loading_order(boxes, container, directions)?;
    let graph = LoadingDependencyGraph::build(boxes);
    let mut present = BTreeSet::new();
    let mut steps = Vec::with_capacity(order.len());
    for index in order {
        let direction = clear_direction(index, boxes, &present, container, directions)?
            .expect("safe loading order was replayed");
        steps.push(SequenceStep {
            index,
            direction,
            depends_on: graph.depends_on[index].iter().copied().collect(),
        });
        present.insert(index);
    }
    Ok(steps)
}

pub fn safe_removal_order_with_evidence(
    boxes: &[Aabb],
    container: Dimensions,
    directions: &[&str],
) -> Result<Vec<SequenceStep>, SequenceError> {
    let order = safe_removal_order(boxes, container, directions)?;
    let graph = UnloadingDependencyGraph::build(boxes);
    let mut present = (0..boxes.len()).collect::<BTreeSet<_>>();
    let mut steps = Vec::with_capacity(order.len());
    for index in order {
        let direction = clear_direction(index, boxes, &present, container, directions)?
            .expect("safe removal order was replayed");
        steps.push(SequenceStep {
            index,
            direction,
            depends_on: graph.depends_on[index].iter().copied().collect(),
        });
        present.remove(&index);
    }
    Ok(steps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Length, Point, Rotation};
    use std::collections::BTreeMap;

    fn dimensions(length: i64, width: i64, height: i64) -> Dimensions {
        Dimensions {
            length: Length(length),
            width: Length(width),
            height: Length(height),
        }
    }

    fn box_at(x: i64, y: i64, z: i64, length: i64, width: i64, height: i64) -> Aabb {
        Aabb {
            origin: Point { x, y, z },
            dimensions: dimensions(length, width, height),
        }
    }

    #[test]
    fn stack_has_distinct_loading_and_unloading_orders() {
        let boxes = [box_at(0, 0, 0, 10, 10, 10), box_at(0, 0, 10, 10, 10, 10)];
        let container = dimensions(20, 20, 20);
        assert_eq!(
            safe_loading_order(&boxes, container, &ALL_DIRECTIONS).unwrap(),
            vec![0, 1]
        );
        assert_eq!(
            safe_removal_order(&boxes, container, &ALL_DIRECTIONS).unwrap(),
            vec![1, 0]
        );
        assert_eq!(
            LoadingDependencyGraph::build(&boxes).depends_on,
            vec![BTreeSet::new(), BTreeSet::from([0])]
        );
    }

    #[test]
    fn restricted_door_order_and_evidence_are_deterministic() {
        let boxes = [box_at(0, 0, 0, 10, 10, 10), box_at(10, 0, 0, 10, 10, 10)];
        let container = dimensions(20, 10, 10);
        assert_eq!(
            safe_loading_order(&boxes, container, &["-x"]).unwrap(),
            vec![1, 0]
        );
        let steps = safe_loading_order_with_evidence(&boxes, container, &["-x"]).unwrap();
        assert_eq!(steps[0].direction, "-x");
        assert_eq!(steps[1].direction, "-x");
    }

    #[test]
    fn invalid_direction_and_bad_replay_are_structured_errors() {
        let boxes = [box_at(0, 0, 0, 10, 10, 10), box_at(0, 0, 10, 10, 10, 10)];
        let container = dimensions(20, 20, 20);
        assert!(matches!(
            safe_loading_order(&boxes, container, &["sideways"]),
            Err(SequenceError::InvalidDirection(_))
        ));
        assert!(matches!(
            replay_loading_order(&boxes, container, &[1, 0], &ALL_DIRECTIONS),
            Err(SequenceError::Replay {
                index: 1,
                step: 0,
                ..
            })
        ));
    }

    #[test]
    fn replay_rejects_geometry_that_was_never_valid() {
        let container = dimensions(20, 20, 20);
        let outside = [box_at(15, 0, 0, 10, 10, 10)];
        assert!(matches!(
            replay_loading_order(&outside, container, &[0], &ALL_DIRECTIONS),
            Err(SequenceError::Replay { .. })
        ));
        let overlapping = [box_at(0, 0, 0, 10, 10, 10), box_at(5, 0, 0, 10, 10, 10)];
        assert!(matches!(
            replay_loading_order(&overlapping, container, &[0, 1], &ALL_DIRECTIONS),
            Err(SequenceError::Replay {
                index: 1,
                step: 1,
                ..
            })
        ));
    }

    #[test]
    fn sequence_step_to_json_matches_the_cross_language_shape() {
        let step = SequenceStep {
            index: 1,
            direction: "+x".to_owned(),
            depends_on: vec![2, 0],
        };
        assert_eq!(
            step.to_json(),
            serde_json::json!({"index": 1, "direction": "+x", "depends_on": [0, 2]}),
        );
    }

    #[test]
    fn sequence_warning_matches_the_shared_cross_language_shape() {
        // A cross-language fixture kept one level above this crate; a published copy
        // does not carry it.
        let shared = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../../conformance/scene/sequence-fixtures.json");
        let Ok(payload_text) = std::fs::read_to_string(&shared) else {
            eprintln!(
                "skipping: the shared cross-language scene fixture is not part of this package"
            );
            return;
        };
        let payload: serde_json::Value = serde_json::from_str(&payload_text).unwrap();
        let warning = SequenceWarning {
            code: "sequence_advisory".into(),
            index: 1,
            message_key: "sequence.advisory".into(),
            arguments: BTreeMap::from([
                ("unit".into(), "mm".into()),
                ("clearance".into(), "2".into()),
            ]),
        };
        assert_eq!(
            warning.to_json(),
            payload["dto_contract"]["sequence_warning"]
        );
    }

    #[test]
    fn sequence_errors_to_json_match_the_cross_language_shape() {
        assert_eq!(
            SequenceError::InvalidDirection("sideways".to_owned()).to_json(),
            serde_json::json!({"code": "invalid_direction", "direction": "sideways"}),
        );
        assert_eq!(
            SequenceError::Stuck { stuck: vec![2, 0] }.to_json(),
            serde_json::json!({"code": "sequence_stuck", "stuck": [0, 2]}),
        );
        assert_eq!(
            SequenceError::Replay {
                index: 3,
                step: 1,
                reason: "no allowed direction is clear of the remaining placements",
            }
            .to_json(),
            serde_json::json!({
                "code": "sequence_replay",
                "index": 3,
                "step": 1,
                "reason": "no allowed direction is clear of the remaining placements",
            }),
        );
    }

    #[test]
    fn shared_fixtures_pin_four_language_graphs_and_evidence() {
        // A cross-language fixture kept one level above this crate; a published copy
        // does not carry it.
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../../conformance/scene/sequence-fixtures.json");
        let Ok(payload_text) = std::fs::read_to_string(&path) else {
            eprintln!(
                "skipping: the shared cross-language scene fixture is not part of this package"
            );
            return;
        };
        let payload: serde_json::Value = serde_json::from_str(&payload_text).unwrap();
        for scene in payload["scenes"].as_array().unwrap() {
            let dimensions_from = |value: &serde_json::Value| {
                dimensions(
                    value["length"].as_i64().unwrap(),
                    value["width"].as_i64().unwrap(),
                    value["height"].as_i64().unwrap(),
                )
            };
            let container = dimensions_from(&scene["container"]);
            let boxes = scene["boxes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|raw| Aabb {
                    origin: Point {
                        x: raw["origin"]["x"].as_i64().unwrap(),
                        y: raw["origin"]["y"].as_i64().unwrap(),
                        z: raw["origin"]["z"].as_i64().unwrap(),
                    },
                    dimensions: dimensions_from(&raw["dimensions"]),
                })
                .collect::<Vec<_>>();
            let owned_directions = scene["directions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap())
                .collect::<Vec<_>>();
            let loading = LoadingDependencyGraph::build(&boxes)
                .depends_on
                .iter()
                .map(|set| set.iter().copied().collect::<Vec<_>>())
                .collect::<Vec<_>>();
            let expected_loading =
                serde_json::from_value::<Vec<Vec<usize>>>(scene["loading_graph"].clone()).unwrap();
            assert_eq!(loading, expected_loading, "{}", scene["id"]);
            let unloading = UnloadingDependencyGraph::build(&boxes)
                .depends_on
                .iter()
                .map(|set| set.iter().copied().collect::<Vec<_>>())
                .collect::<Vec<_>>();
            let expected_unloading =
                serde_json::from_value::<Vec<Vec<usize>>>(scene["unloading_graph"].clone())
                    .unwrap();
            assert_eq!(unloading, expected_unloading, "{}", scene["id"]);
            if scene.get("reachability").is_some() {
                let stops = scene.get("stops").map(|values| {
                    values
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(serde_json::Value::as_i64)
                        .collect::<Vec<_>>()
                });
                let reachability =
                    placement_reachability(&boxes, container, stops.as_deref(), &owned_directions)
                        .unwrap()
                        .into_iter()
                        .map(|entry| entry.to_json())
                        .collect::<Vec<_>>();
                assert_eq!(
                    reachability,
                    scene["reachability"].as_array().unwrap().clone(),
                    "{}",
                    scene["id"],
                );
            }
            if scene.get("expected_error").is_some() {
                let error = safe_loading_order(&boxes, container, &owned_directions)
                    .expect_err("fixture requires a deterministic stuck error");
                assert_eq!(error.to_json(), scene["expected_error"], "{}", scene["id"]);
                continue;
            }
            if scene.get("loading_steps").is_none() {
                continue;
            }
            let loading_steps =
                safe_loading_order_with_evidence(&boxes, container, &owned_directions)
                    .unwrap()
                    .into_iter()
                    .map(|step| step.to_json())
                    .collect::<Vec<_>>();
            assert_eq!(
                loading_steps,
                scene["loading_steps"].as_array().unwrap().clone(),
                "{}",
                scene["id"],
            );
            let unloading_steps =
                safe_removal_order_with_evidence(&boxes, container, &owned_directions)
                    .unwrap()
                    .into_iter()
                    .map(|step| step.to_json())
                    .collect::<Vec<_>>();
            assert_eq!(
                unloading_steps,
                scene["unloading_steps"].as_array().unwrap().clone(),
                "{}",
                scene["id"],
            );
        }
    }

    // ------------------------------- business-rule prefix replay

    use crate::model::{Item, ItemInstance};
    use crate::units::Weight;

    fn business_rule_item(
        id: &str,
        side_mm: i64,
        weight_g: i64,
        max_top_load_g: Option<i64>,
        max_stacked_items: Option<usize>,
        stackable: bool,
        ground_contact_rule: Option<&str>,
    ) -> Item {
        const TICKS_PER_GRAM: i64 = 8_000_000;
        let mm = Length::TICKS_PER_MM;
        Item {
            id: id.to_string(),
            dimensions: dimensions(side_mm * mm, side_mm * mm, side_mm * mm),
            weight: Weight(weight_g * TICKS_PER_GRAM),
            quantity: 1,
            allowed_rotations: vec![Rotation::Lwh],
            stackable,
            must_be_on_floor: false,
            max_top_load: max_top_load_g.map(|g| Weight(g * TICKS_PER_GRAM)),
            minimum_support_ratio: 0.0,
            group: None,
            tags: Default::default(),
            incompatible_tags: Default::default(),
            priority: 0,
            metadata: Default::default(),
            nesting_height: None,
            max_stacked_items,
            ground_contact_rule: ground_contact_rule.map(str::to_string),
            stop_index: None,
            value: None,
            shape_type: crate::geometry::ShapeType::RigidCuboid,
            hull_vertices: None,
            compression_ratio_ppm: None,
            max_compression_pressure_kpa: None,
            eligible_container_tags: Default::default(),
        }
    }

    fn business_rule_container(
        side_mm: i64,
        height_mm: i64,
        max_stack_density_kg: Option<i64>,
    ) -> Container {
        const TICKS_PER_KG: i64 = 8_000_000_000;
        let mm = Length::TICKS_PER_MM;
        Container {
            id: "c".to_string(),
            inner_dimensions: dimensions(side_mm * mm, side_mm * mm, height_mm * mm),
            outer_dimensions: None,
            tare_weight: Weight(0),
            max_payload: None,
            cost_minor: 0,
            quantity: None,
            obstacles: Vec::new(),
            tags: Default::default(),
            max_items: None,
            metadata: Default::default(),
            axles: None,
            void_fill_reserve_ppm: 0,
            tag_limits: Default::default(),
            max_stack_density: max_stack_density_kg.map(|kg| Weight(kg * TICKS_PER_KG)),
            rate_table: None,
            access_directions: Vec::new(),
        }
    }

    fn stacked_placement(item: Item, x_mm: i64, y_mm: i64, z_mm: i64) -> Placement {
        let mm = Length::TICKS_PER_MM;
        let position = Point {
            x: x_mm * mm,
            y: y_mm * mm,
            z: z_mm * mm,
        };
        let dims = item.dimensions;
        Placement {
            instance: ItemInstance { item, sequence: 1 },
            position,
            rotation: Rotation::Lwh,
            dimensions: dims,
            envelope_origin: position,
            envelope_dimensions: dims,
            support_ratio: 1.0,
            top_load: Weight(0),
        }
    }

    #[test]
    fn a_loading_prefix_that_overloads_a_fragile_supporter_is_caught_at_its_step() {
        let fragile = business_rule_item("fragile", 5, 1_000, Some(500), None, true, None);
        let heavy = business_rule_item("heavy", 5, 5_000, None, None, true, None);
        let placements = vec![
            stacked_placement(fragile, 0, 0, 0),
            stacked_placement(heavy, 0, 0, 5),
        ];
        let container = business_rule_container(10, 20, None);
        let error =
            verify_loading_prefix_business_rules(&placements, &[0, 1], &container).unwrap_err();
        assert_eq!(
            error,
            SequenceError::Replay {
                index: 1,
                step: 1,
                reason: "top_load"
            }
        );
    }

    #[test]
    fn a_loading_prefix_that_exceeds_a_stacked_item_limit_is_caught_at_its_step() {
        let base = business_rule_item("base", 5, 0, None, Some(1), true, None);
        let first = business_rule_item("first", 5, 0, None, None, true, None);
        let second = business_rule_item("second", 5, 0, None, None, true, None);
        let placements = vec![
            stacked_placement(base, 0, 0, 0),
            stacked_placement(first, 0, 0, 5),
            stacked_placement(second, 0, 0, 10),
        ];
        let container = business_rule_container(10, 20, None);
        let error =
            verify_loading_prefix_business_rules(&placements, &[0, 1, 2], &container).unwrap_err();
        assert_eq!(
            error,
            SequenceError::Replay {
                index: 2,
                step: 2,
                reason: "max_stacked_items_exceeded"
            }
        );
    }

    #[test]
    fn a_loading_prefix_that_crushes_a_containers_floor_density_limit_is_caught() {
        let heavy = business_rule_item("heavy", 10, 10_000, None, None, true, None);
        let container = business_rule_container(10, 10, Some(1));
        let placements = vec![stacked_placement(heavy, 0, 0, 0)];
        let error =
            verify_loading_prefix_business_rules(&placements, &[0], &container).unwrap_err();
        assert_eq!(
            error,
            SequenceError::Replay {
                index: 0,
                step: 0,
                reason: "stack_density_exceeded"
            }
        );
    }

    #[test]
    fn a_loading_prefix_that_stacks_onto_a_non_stackable_item_is_caught() {
        let base = business_rule_item("base", 5, 1_000, None, None, false, None);
        let rider = business_rule_item("rider", 5, 1_000, None, None, true, None);
        let placements = vec![
            stacked_placement(base, 0, 0, 0),
            stacked_placement(rider, 0, 0, 5),
        ];
        let container = business_rule_container(10, 20, None);
        let error =
            verify_loading_prefix_business_rules(&placements, &[0, 1], &container).unwrap_err();
        assert_eq!(
            error,
            SequenceError::Replay {
                index: 1,
                step: 1,
                reason: "non_stackable"
            }
        );
    }

    #[test]
    fn a_loading_prefix_that_violates_a_ground_contact_rule_is_caught() {
        // "single" requires resting on exactly one supporter; two half-width bases
        // side by side under one full-width rider violate it.
        let left = business_rule_item("left", 5, 0, None, None, true, None);
        let right = business_rule_item("right", 5, 0, None, None, true, None);
        let left_p = stacked_placement(left, 0, 0, 0);
        let right_p = stacked_placement(right, 5, 0, 0);
        let mut rider = business_rule_item("rider", 5, 0, None, None, true, Some("single"));
        let mm = Length::TICKS_PER_MM;
        rider.dimensions = dimensions(10 * mm, 5 * mm, 5 * mm); // 10mm long: spans both bases
        let rider_p = stacked_placement(rider, 0, 0, 5);
        let placements = vec![left_p, right_p, rider_p];
        let container = business_rule_container(10, 20, None);
        let error =
            verify_loading_prefix_business_rules(&placements, &[0, 1, 2], &container).unwrap_err();
        assert_eq!(
            error,
            SequenceError::Replay {
                index: 2,
                step: 2,
                reason: "ground_contact_violation"
            }
        );
    }

    #[test]
    fn loading_prefix_ground_rules_use_the_direct_nested_predecessor() {
        let mut nested = business_rule_item("crate", 10, 0, None, None, true, Some("single"));
        nested.nesting_height = Some(Length(5 * Length::TICKS_PER_MM));
        let placements = vec![
            stacked_placement(nested.clone(), 0, 0, 0),
            stacked_placement(nested.clone(), 0, 0, 5),
        ];
        let container = business_rule_container(10, 20, None);

        verify_loading_prefix_business_rules(&placements, &[0, 1], &container).unwrap();

        nested.ground_contact_rule = Some("multiple".into());
        let placements = vec![
            stacked_placement(nested.clone(), 0, 0, 0),
            stacked_placement(nested, 0, 0, 5),
        ];
        assert_eq!(
            verify_loading_prefix_business_rules(&placements, &[0, 1], &container).unwrap_err(),
            SequenceError::Replay {
                index: 1,
                step: 1,
                reason: "ground_contact_violation",
            }
        );
    }

    #[test]
    fn a_loading_prefix_that_respects_every_business_rule_is_accepted() {
        let fragile = business_rule_item("fragile", 5, 1_000, Some(5_000), Some(2), true, None);
        let light = business_rule_item("light", 5, 1_000, None, None, true, None);
        // No max_stack_density set: this scenario's tiny 100mm^2 footprint under even
        // a light load is already a very high kg/m^2 figure, and this test's purpose
        // is the other rules -- density has its own dedicated test above.
        let container = business_rule_container(10, 20, None);
        let placements = vec![
            stacked_placement(fragile, 0, 0, 0),
            stacked_placement(light, 0, 0, 5),
        ];
        verify_loading_prefix_business_rules(&placements, &[0, 1], &container).unwrap();
    }

    #[test]
    fn composed_safe_loading_api_cannot_return_an_overloaded_order() {
        let fragile = business_rule_item("fragile", 5, 1_000, Some(500), None, true, None);
        let heavy = business_rule_item("heavy", 5, 5_000, None, None, true, None);
        let placements = vec![
            stacked_placement(fragile, 0, 0, 0),
            stacked_placement(heavy, 0, 0, 5),
        ];
        let container = business_rule_container(5, 10, None);
        assert!(matches!(
            safe_loading_order_for_placements(&placements, &container, &ALL_DIRECTIONS),
            Err(SequenceError::Replay { .. })
        ));
    }

    #[test]
    fn a_malformed_business_rule_order_is_rejected_before_any_rule_check() {
        let item = business_rule_item("a", 10, 0, None, None, true, None);
        let placements = vec![stacked_placement(item, 0, 0, 0)];
        let container = business_rule_container(10, 10, None);
        let error =
            verify_loading_prefix_business_rules(&placements, &[0, 0], &container).unwrap_err();
        assert_eq!(
            error,
            SequenceError::Replay {
                index: -1,
                step: -1,
                reason: "order is not a permutation of every placement index exactly once",
            }
        );
    }
}
