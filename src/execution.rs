//! An execution plan derived from an already validated packing result.
//!
//! `docs/EXECUTION-PLAN.md` is the contract. A packing result answers *what goes where*; an
//! operator needs *what to do first, and why this carton*. This module turns the first into
//! the second and is built so that it cannot do anything else: it calls no solver, holds no
//! registry and reads no clock, so the same request and result yield the same plan forever.
//!
//! ```
//! let result = r#"{"status":"feasible","objective":"default","score":[0,1],
//!   "containers":[],"unpacked_items":[],"alternatives":[]}"#;
//! let plan = packvium_core::execution::build_plan_json(result, "{}").unwrap();
//! assert!(plan.contains("\"format\":\"packvium-execution-plan/v1\""));
//! ```
//!
//! Held to byte-identical output with `packvium.execution` and `Packvium\Execution\Plan`.
//! That is cheaper here than elsewhere: `serde_json`'s object map is a `BTreeMap`, so a
//! document is already in the sorted-key compact form the other two produce deliberately.
//!
//! Two rules do the work, and both are about not quietly becoming a decision-maker.
//! Authoritative solver facts and human text are separated in the *output* under `facts`
//! and `presentation`, and every presentation string names the fields it came from. A
//! placement is referenced by what the cross-language contract promises -- container index,
//! `item_type`, `orientation` and `position.*.ticks` -- and never by `item_id`, which
//! `conformance/canonical.py` drops as "an instance count rather than a semantic property".

use serde_json::{Map, Value, json};

/// The plan's own format tag. Not the packing schema's version, and it does not move with
/// it: a result can gain fields without changing what a plan says.
pub const FORMAT: &str = "packvium-execution-plan/v1";

/// What a `score` index means is a property of the request's objective, which this adapter
/// does not know. Naming an index it cannot explain would be inventing meaning.
pub const UNNAMED_AXIS: &str = "unnamed objective axis";

/// The adapter was handed something it cannot describe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionPlanError(pub String);

impl std::fmt::Display for ExecutionPlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ExecutionPlanError {}

type PlanResult<T> = Result<T, ExecutionPlanError>;

fn fail<T>(message: impl Into<String>) -> PlanResult<T> {
    Err(ExecutionPlanError(message.into()))
}

fn integers(value: Option<&Value>) -> Vec<i64> {
    value
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default()
}

/// A reference two languages agree on, for one placement in one container.
fn placement_reference(container_index: usize, placement: &Value) -> PlanResult<Value> {
    let object = match placement.as_object() {
        Some(object) => object,
        None => return fail("a placement must be an object"),
    };
    let mut ticks = Map::new();
    for axis in ["x", "y", "z"] {
        match placement
            .pointer(&format!("/position/{axis}/ticks"))
            .and_then(Value::as_i64)
        {
            // `ticks` is the exact integer; `value`, which the same `exactScalar` also
            // carries, is a rendering, and a reference built on it would depend on how a
            // number was printed.
            Some(number) => ticks.insert(axis.to_string(), json!(number)),
            None => {
                return fail(format!(
                    "placement is missing a field the reference is built from: 'position.{axis}.ticks'"
                ));
            }
        };
    }
    for required in ["item_type", "orientation"] {
        if !object.contains_key(required) {
            return fail(format!(
                "placement is missing a field the reference is built from: '{required}'"
            ));
        }
    }
    Ok(json!({
        "container_index": container_index,
        "item_type": object["item_type"].clone(),
        "orientation": object["orientation"].clone(),
        "position_ticks": Value::Object(ticks),
    }))
}

/// The operator sequence for one container, or an honest absence of one.
///
/// The engines compute a loading order from geometry this adapter never sees, so it is
/// injected. Falling back to the order placements happen to appear in would present an
/// artifact of how the solver walked its candidates as a safe order to lift boxes in.
fn steps(
    container_index: usize,
    placements: &[Value],
    loading_order: Option<&Vec<i64>>,
) -> PlanResult<(String, Vec<Value>)> {
    let Some(order) = loading_order else {
        let mut listed = Vec::with_capacity(placements.len());
        for placement in placements {
            listed.push(json!({"placement": placement_reference(container_index, placement)?}));
        }
        return Ok(("unavailable".to_string(), listed));
    };
    let mut sorted: Vec<i64> = order.clone();
    sorted.sort_unstable();
    let expected: Vec<i64> = (0..placements.len() as i64).collect();
    if sorted != expected {
        return fail(format!(
            "loading order for container {container_index} is not a permutation of its {} placements",
            placements.len()
        ));
    }
    let mut ordered = Vec::with_capacity(order.len());
    for (step, index) in order.iter().enumerate() {
        ordered.push(json!({
            "sequence": step + 1,
            "placement": placement_reference(container_index, &placements[*index as usize])?,
        }));
    }
    Ok(("loading".to_string(), ordered))
}

/// The first index at which two score vectors differ, and by how much.
///
/// Never a blended number. The portfolio compared these lexicographically, so the first
/// differing index *is* the decision; weighting the vector would replace a decision that
/// was made with one that was not.
fn first_difference(winner: &[i64], loser: &[i64]) -> PlanResult<Value> {
    let shared = winner.len().min(loser.len());
    for index in 0..shared {
        if winner[index] != loser[index] {
            return Ok(json!({
                "index": index,
                "winner": winner[index],
                "alternative": loser[index],
                "difference": loser[index] - winner[index],
            }));
        }
    }
    if winner.len() != loser.len() {
        return fail("score vectors of different length cannot be compared lexicographically");
    }
    Ok(Value::Null)
}

fn alternative(index: usize, winner_score: &[i64], value: &Value) -> PlanResult<Value> {
    let score = integers(value.get("score"));
    let difference = first_difference(winner_score, &score)?;
    let summary = if difference.is_null() {
        "This option scored identically to the chosen one on every objective axis; \
         the score does not record why one was taken."
            .to_string()
    } else {
        format!(
            "This option differs first at objective axis {} ({UNNAMED_AXIS}): chosen {}, this {}.",
            difference["index"], difference["winner"], difference["alternative"]
        )
    };
    Ok(json!({
        "facts": {
            "alternative_index": index,
            "score": score,
            "status": value.get("status").cloned().unwrap_or(Value::Null),
            "first_difference": difference,
        },
        // Deliberately not "it lost because it is taller". The solver recorded a score, not
        // a cause; naming a cause would be a claim nothing in the result supports.
        "presentation": {
            "summary": summary,
            "cites": ["score", "alternatives[].score"],
        },
    }))
}

/// Derive the execution plan for one validated result.
///
/// `result_json` is a packing result document; `loading_orders_json` maps a container index,
/// as a decimal string key, to the engine-computed order its placements load in. Both are
/// strings for the same reason the commerce entry points take one: this is the shape the
/// conformance harness can drive over a pipe.
pub fn build_plan_json(result_json: &str, loading_orders_json: &str) -> PlanResult<String> {
    let result: Value = match serde_json::from_str(result_json) {
        Ok(value) => value,
        Err(error) => return fail(format!("result is not valid JSON: {error}")),
    };
    let orders: Value = match serde_json::from_str(loading_orders_json) {
        Ok(value) => value,
        Err(error) => return fail(format!("loading orders are not valid JSON: {error}")),
    };
    if result.get("status").is_none() {
        return fail("a result without a status is not a validated result");
    }

    let empty = Vec::new();
    let containers = result
        .get("containers")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let winner_score = integers(result.get("score"));

    let mut plan_containers = Vec::with_capacity(containers.len());
    for (index, container) in containers.iter().enumerate() {
        let placements = container
            .get("placements")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let order = orders
            .get(index.to_string())
            .map(|value| integers(Some(value)));
        let (kind, listed) = steps(index, &placements, order.as_ref())?;
        plan_containers.push(json!({
            "container_index": index,
            "facts": {
                "container_type": container.get("container_type").cloned().unwrap_or(Value::Null),
                "placement_count": placements.len(),
                "volume_utilization": container.get("volume_utilization").cloned().unwrap_or(Value::Null),
            },
            "order": kind,
            "steps": listed,
        }));
    }

    let mut unplaced = Vec::new();
    for item in result
        .get("unpacked_items")
        .and_then(Value::as_array)
        .unwrap_or(&empty)
    {
        let level = item.pointer("/proof/level").cloned().unwrap_or(Value::Null);
        let reason = item.get("reason").cloned().unwrap_or(Value::Null);
        unplaced.push(json!({
            "facts": {
                "item_type": item.get("item_type").cloned().unwrap_or(Value::Null),
                "reason": reason,
                // Carried through unchanged. Softening `observed` into "could not fit"
                // would turn an honest limit into a false certainty.
                "proof_level": level,
                "details": item.get("details").cloned().unwrap_or(json!([])),
            },
            "presentation": {
                "summary": format!(
                    "Not packed: {} ({}).",
                    item.get("reason").and_then(Value::as_str).unwrap_or(""),
                    item.pointer("/proof/level").and_then(Value::as_str).unwrap_or(""),
                ),
                "cites": ["unpacked_items[].reason", "unpacked_items[].proof.level"],
            },
        }));
    }

    let mut alternatives = Vec::new();
    for (index, value) in result
        .get("alternatives")
        .and_then(Value::as_array)
        .unwrap_or(&empty)
        .iter()
        .enumerate()
    {
        alternatives.push(alternative(index, &winner_score, value)?);
    }

    let plan = json!({
        "format": FORMAT,
        "objective": result.get("objective").cloned().unwrap_or(Value::Null),
        "facts": {
            "status": result["status"].clone(),
            "score": winner_score,
            "feasibility": result.get("feasibility").cloned().unwrap_or(Value::Null),
            "optimality": result.get("optimality").cloned().unwrap_or(Value::Null),
            "container_count": containers.len(),
        },
        "containers": plan_containers,
        // Often empty, and not for one reason. Four produce an empty list: the `fast`
        // profile runs a single solver, stops the start loop once the grid lattice
        // packs everything, only one start completed, or `alternatives: 1` -- the cap
        // counts the winner. Measured over the corpus, 165 of 399 requests do carry one,
        // so this is not the rare case an earlier draft of this comment claimed.
        // An empty list is well-formed and is never an error.
        "alternatives": alternatives,
        "unplaced": unplaced,
    });
    Ok(plan.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scalar(ticks: i64) -> String {
        format!(
            r#"{{"ticks":{ticks},"value":"{}","unit":"mm"}}"#,
            ticks / 16000
        )
    }

    fn placement(item_type: &str, x: i64) -> String {
        format!(
            r#"{{"item_id":"{item_type}#{x}","item_type":"{item_type}","orientation":"LWH",
               "position":{{"x":{},"y":{},"z":{}}},
               "dimensions":{{"length":{},"width":{},"height":{}}},
               "support_ratio":1.0,"top_load":0}}"#,
            scalar(x),
            scalar(0),
            scalar(0),
            scalar(1600000),
            scalar(1600000),
            scalar(1600000),
        )
    }

    fn result() -> String {
        format!(
            r#"{{"status":"feasible","objective":"default","score":[0,1,0,0,1000000],
               "feasibility":{{"code":"feasible"}},"optimality":{{"code":"not_proven"}},
               "containers":[{{"id":"box#1","container_type":"box","volume_utilization":0.5,
                 "placements":[{},{}]}}],
               "unpacked_items":[],"alternatives":[]}}"#,
            placement("cube", 0),
            placement("cube", 1600000),
        )
    }

    fn plan(orders: &str) -> Value {
        serde_json::from_str(&build_plan_json(&result(), orders).unwrap()).unwrap()
    }

    #[test]
    fn without_an_injected_order_the_plan_says_so() {
        let plan = plan("{}");
        let container = &plan["containers"][0];
        assert_eq!(container["order"], "unavailable");
        // Every placement is still listed; no step numbers, because array position is an
        // artifact of candidate iteration.
        assert_eq!(container["steps"].as_array().unwrap().len(), 2);
        assert!(container["steps"][0].get("sequence").is_none());
    }

    #[test]
    fn an_injected_order_is_used_verbatim() {
        let plan = plan(r#"{"0":[1,0]}"#);
        let container = &plan["containers"][0];
        assert_eq!(container["order"], "loading");
        assert_eq!(container["steps"][0]["sequence"], 1);
        // Step 1 is the placement the caller put first, not the one the result listed first.
        assert_eq!(
            container["steps"][0]["placement"]["position_ticks"]["x"],
            1600000
        );
    }

    #[test]
    fn an_order_that_is_not_a_permutation_is_refused() {
        let error = build_plan_json(&result(), r#"{"0":[0,0]}"#).unwrap_err();
        assert!(error.0.contains("permutation"), "{}", error.0);
    }

    #[test]
    fn the_placement_reference_does_not_use_item_id() {
        let placement: Value = serde_json::from_str(&placement("cube", 0)).unwrap();
        let reference = placement_reference(0, &placement).unwrap();
        assert!(reference.get("item_id").is_none());
        let keys: Vec<&String> = reference.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            [
                "container_index",
                "item_type",
                "orientation",
                "position_ticks"
            ]
        );
    }

    #[test]
    fn the_placement_reference_reads_ticks_and_not_the_rendered_value() {
        let mut placement: Value = serde_json::from_str(&placement("cube", 12345)).unwrap();
        placement["position"]["x"]["value"] = json!("wrong");
        let reference = placement_reference(0, &placement).unwrap();
        assert_eq!(reference["position_ticks"]["x"], 12345);
    }

    #[test]
    fn a_placement_it_cannot_reference_is_refused() {
        let mut placement: Value = serde_json::from_str(&placement("cube", 0)).unwrap();
        placement.as_object_mut().unwrap().remove("orientation");
        let error = placement_reference(0, &placement).unwrap_err();
        assert!(error.0.contains("missing a field"), "{}", error.0);
    }

    #[test]
    fn the_loss_is_the_first_differing_index_and_never_a_blend() {
        let with_alternative = result().replace(
            r#""alternatives":[]"#,
            r#""alternatives":[{"status":"feasible","score":[0,2,0,0,900000]}]"#,
        );
        let plan: Value =
            serde_json::from_str(&build_plan_json(&with_alternative, "{}").unwrap()).unwrap();
        let facts = &plan["alternatives"][0]["facts"];
        assert_eq!(facts["first_difference"]["index"], 1);
        assert_eq!(facts["first_difference"]["difference"], 1);
        assert!(facts.get("total").is_none() && facts.get("weighted").is_none());
    }

    #[test]
    fn the_sentence_names_an_axis_and_claims_no_cause() {
        let with_alternative = result().replace(
            r#""alternatives":[]"#,
            r#""alternatives":[{"status":"feasible","score":[0,2,0,0,900000]}]"#,
        );
        let plan: Value =
            serde_json::from_str(&build_plan_json(&with_alternative, "{}").unwrap()).unwrap();
        let summary = plan["alternatives"][0]["presentation"]["summary"]
            .as_str()
            .unwrap();
        assert!(summary.contains("axis 1"), "{summary}");
        for causal in ["because", "due to", "caused"] {
            assert!(!summary.to_lowercase().contains(causal), "{summary}");
        }
    }

    #[test]
    fn score_vectors_of_different_length_are_refused() {
        let with_alternative = result().replace(
            r#""alternatives":[]"#,
            r#""alternatives":[{"score":[0,1]}]"#,
        );
        let error = build_plan_json(&with_alternative, "{}").unwrap_err();
        assert!(error.0.contains("different length"), "{}", error.0);
    }

    #[test]
    fn an_unpacked_item_keeps_its_proof_level_unsoftened() {
        let with_unpacked = result().replace(
            r#""unpacked_items":[]"#,
            r#""unpacked_items":[{"item_id":"ladder#1","item_type":"ladder",
               "reason":"no_container_fits","details":["too long"],
               "proof":{"level":"observed","observations":[{"code":"too_long"}]}}]"#,
        );
        let plan: Value =
            serde_json::from_str(&build_plan_json(&with_unpacked, "{}").unwrap()).unwrap();
        assert_eq!(plan["unplaced"][0]["facts"]["proof_level"], "observed");
        // The level appears in the sentence too: a reader must not be told "cannot fit"
        // when the engine only observed that it did not.
        let summary = plan["unplaced"][0]["presentation"]["summary"]
            .as_str()
            .unwrap();
        assert!(summary.contains("observed"), "{summary}");
    }

    #[test]
    fn a_result_without_a_status_is_not_a_validated_result() {
        let error = build_plan_json(r#"{"containers":[]}"#, "{}").unwrap_err();
        assert!(error.0.contains("validated result"), "{}", error.0);
    }

    #[test]
    fn the_same_input_produces_the_same_bytes() {
        assert_eq!(
            build_plan_json(&result(), "{}"),
            build_plan_json(&result(), "{}")
        );
        // `serde_json`'s map is a BTreeMap, so the document is already sorted and compact --
        // the same form Python's `sort_keys=True` and PHP's recursive ksort produce.
        let plan = build_plan_json(&result(), "{}").unwrap();
        assert!(plan.starts_with(r#"{"alternatives":"#), "{plan}");
        assert!(!plan.contains(", "), "{plan}");
    }
}
