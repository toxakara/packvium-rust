use std::collections::{BTreeMap, HashSet};
use std::iter;
use std::rc::Rc;

use crate::{
    Container, Item, PackError, PackResult, PackedContainer, PackingRequest, PackingResult,
    pack_request,
};

#[derive(Clone, Debug)]
pub struct NestedLevel {
    pub name: String,
    pub request: PackingRequest,
}

#[derive(Clone, Debug)]
pub struct NestedPackingRequest {
    pub levels: Vec<NestedLevel>,
    pub beam_width: usize,
}

#[derive(Clone, Debug)]
pub struct NestedPackingResult {
    pub levels: Vec<(String, PackingResult)>,
}

/// [`pack_nested_with`] at a level beam width of one: the greedy chain.
pub fn pack_nested(request: &NestedPackingRequest) -> PackResult<NestedPackingResult> {
    pack_nested_with(request, 1)
}

/// Executes explicitly supplied levels in order, carrying the `level_beam_width` best chains
/// from one level to the next, as Python's `NestedPacker` does.
///
/// A level whose request lists no items packs the previous level's containers instead, each
/// as one upright item (`PackedContainer::as_item`); a level that lists its own items packs
/// those for every chain. A chain extends by packing against the level's whole container
/// list and, when the width is above one and the level offers more than one container,
/// against each container alone. Chains rank by their newest result's score, then each
/// earlier level's outward in, then generation order; two chains handing up the same
/// multiset of containers (type, billed size, gross weight) keep only the better. A level
/// with no complete candidate ends every chain and the best incomplete one is the answer --
/// unless the next level lists its own items, which then still runs on that best chain
/// alone. A width of one is exactly the greedy chain.
///
/// A non-zero `request.beam_width` sets every level's `container_plan_beam_width`.
///
/// At most `L * K * (T + 1)` level packs for `L` levels, width `K` and at most `T` containers
/// per level; ranking adds `O(E log E * L)` score comparisons for `E <= K * (T + 1)`
/// extensions a level. Chains share their earlier levels, so the kept beam holds `O(L * K)`
/// results.
///
/// Locks stay fixed: singleton offers omitting a locked container type are skipped. At
/// width above one, a handoff missing a locked item instance is an incompatible branch;
/// compatible siblings continue, and if none exists the first receives the flat packer's
/// ordinary refusal. Offer filtering is O(F + T), handoff filtering O((H + F) log H) per
/// chain, with O(H) temporary space for F locks and H handed-up item types.
pub fn pack_nested_with(
    request: &NestedPackingRequest,
    level_beam_width: usize,
) -> PackResult<NestedPackingResult> {
    if level_beam_width == 0 {
        return Err(PackError::InvalidInput(
            "level_beam_width must be a positive integer".into(),
        ));
    }
    let mut beam = vec![Chain::empty()];
    for (index, level) in request.levels.iter().enumerate() {
        let ranked = ranked_extensions(level, &beam, level_beam_width, request.beam_width)?;
        let next_lists_items = request
            .levels
            .get(index + 1)
            .is_some_and(|next| !next.request.items.is_empty());
        match next_beam(ranked, level_beam_width, next_lists_items) {
            Step::Continue(chains) => beam = chains,
            Step::Stop(chains) => {
                beam = chains;
                break;
            }
        }
    }
    let levels = beam
        .first()
        .map_or_else(Vec::new, |chain| chain.named(&request.levels));
    Ok(NestedPackingResult { levels })
}

/// One level result and the chain it extends; chains share their earlier levels.
struct Link {
    result: PackingResult,
    previous: Option<Rc<Link>>,
}

impl Link {
    /// The chain's scores newest level first: the ranking key.
    fn scores_outward_in(&self) -> impl Iterator<Item = &Vec<i128>> {
        iter::successors(Some(self), |link| link.previous.as_deref()).map(|link| &link.result.score)
    }
}

/// A beam entry: its levels so far and the items it hands up to the next level.
struct Chain {
    last: Option<Rc<Link>>,
    handoff: Vec<Item>,
}

impl Chain {
    fn empty() -> Self {
        Self {
            last: None,
            handoff: Vec::new(),
        }
    }

    fn extended(link: Link) -> Self {
        Self {
            handoff: link
                .result
                .containers
                .iter()
                .map(PackedContainer::as_item)
                .collect(),
            last: Some(Rc::new(link)),
        }
    }

    fn named(&self, levels: &[NestedLevel]) -> Vec<(String, PackingResult)> {
        let mut results: Vec<PackingResult> =
            iter::successors(self.last.as_deref(), |link| link.previous.as_deref())
                .map(|link| link.result.clone())
                .collect();
        results.reverse();
        levels
            .iter()
            .map(|level| level.name.clone())
            .zip(results)
            .collect()
    }
}

enum Step {
    Continue(Vec<Chain>),
    Stop(Vec<Chain>),
}

/// Every chain extended by every offer, best first; the sort is stable, so generation
/// order is the final tie-break.
fn ranked_extensions(
    level: &NestedLevel,
    beam: &[Chain],
    level_beam_width: usize,
    plan_beam_width: usize,
) -> PackResult<Vec<Link>> {
    let offers = offers(&level.request, level_beam_width);
    let mut extensions = Vec::with_capacity(beam.len() * offers.len());
    let mut first_incompatible = None;
    for chain in beam {
        let items = if level.request.items.is_empty() {
            &chain.handoff
        } else {
            &level.request.items
        };
        if level_beam_width > 1
            && level.request.items.is_empty()
            && !handoff_supports_locks(&level.request, items)
        {
            first_incompatible.get_or_insert(chain);
            continue;
        }
        for offer in &offers {
            let result = pack_request(&level_request(level, items, offer, plan_beam_width))?;
            extensions.push(Link {
                result,
                previous: chain.last.clone(),
            });
        }
    }
    // With no compatible branch, retain the ordinary packer's original fixed-placement
    // refusal instead of fabricating an incomplete packing or dropping any lock.
    if extensions.is_empty()
        && let Some(chain) = first_incompatible
    {
        return ranked_extensions(level, std::slice::from_ref(chain), 1, plan_beam_width);
    }
    extensions.sort_by(|a, b| a.scores_outward_in().cmp(b.scores_outward_in()));
    Ok(extensions)
}

/// A lock consumes one handed-up item instance. O((H + F) log H) for H handed-up item
/// types and F fixed placements, O(H) space; no quantity expansion or geometry is needed.
fn handoff_supports_locks(request: &PackingRequest, items: &[Item]) -> bool {
    if request.fixed_placements.is_empty() {
        return true;
    }
    let mut available: BTreeMap<&str, usize> = items
        .iter()
        .map(|item| (item.id.as_str(), item.quantity))
        .collect();
    request.fixed_placements.iter().all(|fixed| {
        let Some(left) = available.get_mut(fixed.item_id.as_str()) else {
            return false;
        };
        if *left == 0 {
            return false;
        }
        *left -= 1;
        true
    })
}

/// The whole container list, then -- for a beam wider than one -- each container that can
/// carry every fixed placement alone. Locks stay unchanged; incompatible offers are omitted.
/// O(F + T) for F fixed placements and T container types, O(T) offer references.
fn offers(request: &PackingRequest, level_beam_width: usize) -> Vec<&[Container]> {
    let containers = request.containers.as_slice();
    let mut offers = vec![containers];
    if level_beam_width > 1 && containers.len() > 1 {
        let locked = request
            .fixed_placements
            .first()
            .map(|fixed| fixed.container_id.as_str());
        if request
            .fixed_placements
            .iter()
            .any(|fixed| Some(fixed.container_id.as_str()) != locked)
        {
            return offers;
        }
        offers.extend(
            containers
                .iter()
                .filter(|container| locked.is_none_or(|id| container.id == id))
                .map(std::slice::from_ref),
        );
    }
    offers
}

fn level_request(
    level: &NestedLevel,
    items: &[Item],
    offer: &[Container],
    plan_beam_width: usize,
) -> PackingRequest {
    let mut request = level.request.clone();
    request.items = items.to_vec();
    request.containers = offer.to_vec();
    if plan_beam_width > 0 {
        request.config.container_plan_beam_width = plan_beam_width;
    }
    request
}

fn next_beam(ranked: Vec<Link>, level_beam_width: usize, next_lists_items: bool) -> Step {
    if !ranked.iter().any(|link| link.result.complete()) {
        let best: Vec<Chain> = ranked.into_iter().take(1).map(Chain::extended).collect();
        return if next_lists_items {
            Step::Continue(best)
        } else {
            Step::Stop(best)
        };
    }
    Step::Continue(distinct_handoffs(ranked, level_beam_width))
}

/// The first `level_beam_width` complete chains whose handed-up containers differ.
fn distinct_handoffs(ranked: Vec<Link>, level_beam_width: usize) -> Vec<Chain> {
    let mut seen: HashSet<Vec<HandoffEntry>> = HashSet::new();
    ranked
        .into_iter()
        .filter(|link| link.result.complete())
        .filter(|link| seen.insert(handoff_key(&link.result)))
        .take(level_beam_width)
        .map(Chain::extended)
        .collect()
}

/// One handed-up container: its type, billed length, width and height, and gross weight.
type HandoffEntry = (String, i64, i64, i64, i64);

/// The handed-up containers as a multiset: instance numbers and emission order are not part
/// of a packing's contract (`conformance/canonical.py`), so every engine collapses the same
/// chains. `O(c log c)` for `c` containers.
fn handoff_key(result: &PackingResult) -> Vec<HandoffEntry> {
    let mut key: Vec<HandoffEntry> = result
        .containers
        .iter()
        .map(|packed| {
            let billed = packed.billed_dimensions();
            (
                packed.container.id.clone(),
                billed.length.0,
                billed.width.0,
                billed.height.0,
                packed.gross_weight().0,
            )
        })
        .collect();
    key.sort_unstable();
    key
}

/// The cross-engine nested-beam scene, `conformance/scene/nested-beam.json`.
///
/// In-crate rather than under `tests/` because each level's JSON request has to become a
/// typed `PackingRequest` through the parser `pack_json` uses, which is crate-private; the
/// behaviour tests that measure this file's coverage from outside are in
/// `tests/registry_and_nested.rs`.
#[cfg(test)]
mod beam_scene {
    use super::{NestedLevel, NestedPackingRequest, pack_nested_with};
    use crate::PackingResult;
    use crate::api::request_from_json;
    use serde_json::{Value, json};
    use std::path::Path;

    fn scene() -> Option<Value> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../../conformance/scene/nested-beam.json");
        read_scene(&path)
    }

    fn read_scene(path: &Path) -> Option<Value> {
        let Ok(text) = std::fs::read_to_string(path) else {
            eprintln!("skipping: the shared nested-beam scene is not part of this package");
            return None;
        };
        Some(serde_json::from_str(&text).expect("the scene is JSON"))
    }

    #[test]
    fn a_public_package_without_the_shared_scene_still_runs_its_other_tests() {
        let missing = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/not-in-the-package/missing-nested-beam-scene.json");
        assert!(read_scene(&missing).is_none());
    }

    fn level(entry: &Value) -> (NestedLevel, String, String) {
        let request = request_from_json(&entry["request"]).expect("a scene level is valid");
        let units = (
            request.output_length_unit.clone(),
            request.output_weight_unit.clone(),
        );
        let name = entry["name"].as_str().expect("a level is named").to_owned();
        (NestedLevel { name, request }, units.0, units.1)
    }

    /// A handed-up item is named after its packed container (`quad#2`); only its type is
    /// contract. Scene item ids carry no `#`, so first-level types pass through.
    fn kind(item_type: &Value) -> String {
        let text = item_type.as_str().expect("an item type is text");
        text.split('#').next().unwrap_or(text).to_owned()
    }

    fn placement(placement: &Value) -> (String, i64, i64, i64, String) {
        let ticks = |axis: &str| {
            placement["position"][axis]["ticks"]
                .as_i64()
                .expect("ticks")
        };
        let orientation = placement["orientation"].as_str().expect("orientation");
        (
            kind(&placement["item_type"]),
            ticks("x"),
            ticks("y"),
            ticks("z"),
            orientation.to_owned(),
        )
    }

    fn container(container: &Value) -> Value {
        let mut placements: Vec<_> = container["placements"]
            .as_array()
            .expect("placements")
            .iter()
            .map(placement)
            .collect();
        placements.sort();
        json!({"container_type": container["container_type"], "placements": placements})
    }

    /// Containers as a multiset. The generator orders them by Python's `json.dumps`; ordering
    /// both sides by one serialisation here is the same equality, exactly as strict.
    fn sorted_containers(containers: &[Value]) -> Vec<Value> {
        let mut sorted = containers.to_vec();
        sorted.sort_by_cached_key(Value::to_string);
        sorted
    }

    /// The generator's `projection()`: the contract's multiset view of one level.
    fn projection(name: &str, result: &PackingResult, length: &str, weight: &str) -> Value {
        let document = result.to_json(length, weight, true);
        let containers: Vec<Value> = document["containers"]
            .as_array()
            .expect("containers")
            .iter()
            .map(container)
            .collect();
        let mut unpacked: Vec<String> = document["unpacked_items"]
            .as_array()
            .expect("unpacked_items")
            .iter()
            .map(|unpacked| kind(&unpacked["item_type"]))
            .collect();
        unpacked.sort();
        json!({
            "name": name,
            "complete": document["complete"],
            "score": document["score"],
            "containers": sorted_containers(&containers),
            "unpacked": unpacked,
        })
    }

    /// The expected levels with their containers in this module's order.
    fn expected(case: &Value) -> Vec<Value> {
        let levels = case["expected"].as_array().expect("expected");
        levels
            .iter()
            .map(|level| {
                let mut level = level.clone();
                let containers = level["containers"].as_array().expect("containers");
                level["containers"] = Value::Array(sorted_containers(containers));
                level
            })
            .collect()
    }

    fn actual(case: &Value) -> Vec<Value> {
        let parsed: Vec<_> = case["levels"]
            .as_array()
            .expect("levels")
            .iter()
            .map(level)
            .collect();
        let request = NestedPackingRequest {
            levels: parsed.iter().map(|(level, _, _)| level.clone()).collect(),
            beam_width: 0,
        };
        let width = case["level_beam_width"].as_u64().expect("width") as usize;
        let result = pack_nested_with(&request, width).expect("a scene case packs");
        result
            .levels
            .iter()
            .zip(&parsed)
            .map(|((name, packed), (_, length, weight))| projection(name, packed, length, weight))
            .collect()
    }

    #[test]
    fn every_scene_case_matches_pythons_nested_packer() {
        let Some(scene) = scene() else {
            return;
        };
        let cases = scene["cases"].as_array().expect("cases");
        let fixed_cases = scene["fixed_cases"]
            .as_array()
            .expect("fixed cases for full-request levels");
        assert!(!cases.is_empty());
        assert!(!fixed_cases.is_empty());
        for case in cases.iter().chain(fixed_cases) {
            assert_eq!(actual(case), expected(case), "case {}", case["name"]);
        }
    }
}
