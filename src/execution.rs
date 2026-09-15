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
//! Held to byte-identical output with `packvium.execution` and `Packvium\Execution\Plan`, and
//! written in the RFC 8785 canonical form the operational artifact shares. Until 1.3.0 this
//! was `serde_json`'s compact writer: the same bytes for every golden plan, but not for a key
//! outside the Basic Multilingual Plane or an integral float, where the adapters disagreed.
//!
//! Two rules do the work, and both are about not quietly becoming a decision-maker.
//! Authoritative solver facts and human text are separated in the *output* under `facts`
//! and `presentation`, and every presentation string names the fields it came from. A
//! placement is referenced by what the cross-language contract promises -- container index,
//! `item_type`, `orientation` and `position.*.ticks` -- and never by `item_id`, which
//! `conformance/canonical.py` drops as "an instance count rather than a semantic property".

use serde_json::{Map, Number, Value, json};

use crate::{canonical_json, value_text};

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

/// A score vector as the result carries it. Terms are copied, never filtered: dropping one the
/// adapter cannot read would shift every later index and misname the deciding axis.
fn score_terms(value: Option<&Value>) -> Vec<Value> {
    value.and_then(Value::as_array).cloned().unwrap_or_default()
}

/// Terms compare by value, as the reference compares them: `1` and `1.0` are the same term.
fn same_term(left: &Value, right: &Value) -> bool {
    match (left.as_i64(), right.as_i64()) {
        (Some(left), Some(right)) => left == right,
        _ if left.is_number() && right.is_number() => left.as_f64() == right.as_f64(),
        _ => left == right,
    }
}

/// `alternative - winner`: exact for integers, IEEE for anything with a fraction.
fn term_difference(winner: &Value, alternative: &Value) -> PlanResult<Value> {
    if let (Some(chosen), Some(other)) = (winner.as_i64(), alternative.as_i64()) {
        let difference = i128::from(other) - i128::from(chosen);
        // Beyond i64 a difference is beyond 2^53 - 1 as well, and the writer refuses it.
        let held =
            i64::try_from(difference).unwrap_or(if difference < 0 { i64::MIN } else { i64::MAX });
        return Ok(Value::from(held));
    }
    if !(winner.is_number() && alternative.is_number()) {
        return fail("score terms that are not numbers cannot be subtracted");
    }
    let difference = alternative.as_f64().unwrap_or(0.0) - winner.as_f64().unwrap_or(0.0);
    Ok(Number::from_f64(difference.clamp(-f64::MAX, f64::MAX)).map_or(Value::Null, Value::Number))
}

/// A reference two languages agree on, for one placement in one container.
pub(crate) fn placement_reference(container_index: usize, placement: &Value) -> PlanResult<Value> {
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
    loading_order: Option<&[i64]>,
) -> PlanResult<(String, Vec<Value>)> {
    let Some(order) = loading_order else {
        let mut listed = Vec::with_capacity(placements.len());
        for placement in placements {
            listed.push(json!({"placement": placement_reference(container_index, placement)?}));
        }
        return Ok(("unavailable".to_string(), listed));
    };
    let mut sorted: Vec<i64> = order.to_vec();
    sorted.sort_unstable();
    let expected: Vec<i64> = (0..placements.len() as i64).collect();
    if sorted != expected {
        return Err(not_a_permutation(container_index, placements.len()));
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

/// An injected order as indices. An entry that is not an integer is refused rather than
/// dropped: dropping it could turn `[0, "1"]` into a permutation of one placement.
fn loading_order(
    container_index: usize,
    value: &Value,
    placement_count: usize,
) -> PlanResult<Vec<i64>> {
    value
        .as_array()
        .and_then(|entries| {
            entries
                .iter()
                .map(Value::as_i64)
                .collect::<Option<Vec<i64>>>()
        })
        .ok_or_else(|| not_a_permutation(container_index, placement_count))
}

fn not_a_permutation(container_index: usize, placement_count: usize) -> ExecutionPlanError {
    ExecutionPlanError(format!(
        "loading order for container {container_index} is not a permutation of its {placement_count} placements"
    ))
}

/// The first index at which two score vectors differ, and by how much.
///
/// Never a blended number. The portfolio compared these lexicographically, so the first
/// differing index *is* the decision; weighting the vector would replace a decision that
/// was made with one that was not.
fn first_difference(winner: &[Value], loser: &[Value]) -> PlanResult<Value> {
    for (index, (chosen, other)) in winner.iter().zip(loser).enumerate() {
        if !same_term(chosen, other) {
            return Ok(json!({
                "index": index,
                "winner": chosen,
                "alternative": other,
                "difference": term_difference(chosen, other)?,
            }));
        }
    }
    if winner.len() != loser.len() {
        return fail("score vectors of different length cannot be compared lexicographically");
    }
    Ok(Value::Null)
}

fn alternative(index: usize, winner_score: &[Value], value: &Value) -> PlanResult<Value> {
    let score = score_terms(value.get("score"));
    let difference = first_difference(winner_score, &score)?;
    let summary = if difference.is_null() {
        "This option scored identically to the chosen one on every objective axis; \
         the score does not record why one was taken."
            .to_string()
    } else {
        format!(
            "This option differs first at objective axis {} ({UNNAMED_AXIS}): chosen {}, this {}.",
            difference["index"],
            value_text::text(&difference["winner"]),
            value_text::text(&difference["alternative"])
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
    // Read exactly, not with `serde_json`'s best-effort float parser: a float read one ULP
    // off is written back as different digits.
    let result = match canonical_json::parse(result_json) {
        Ok(value) => value,
        Err(error) => return fail(format!("result is not valid JSON: {}", error.message)),
    };
    let orders = match canonical_json::parse(loading_orders_json) {
        Ok(value) => value,
        Err(error) => {
            return fail(format!(
                "loading orders are not valid JSON: {}",
                error.message
            ));
        }
    };
    let plan = build_plan(&result, &orders)?;
    canonical_json::to_canonical_string(&plan).or_else(|error| {
        fail(format!(
            "the plan has no canonical spelling: {}",
            error.message
        ))
    })
}

/// The plan for one parsed result; the operational artifact wraps exactly this.
///
/// O(P + U + A·S) for P placements, U unplaced items and A alternatives of S score terms, plus
/// O(L log L) to check a loading order of length L is a permutation.
pub(crate) fn build_plan(result: &Value, orders: &Value) -> PlanResult<Value> {
    if result.get("status").is_none_or(Value::is_null) {
        return fail("a result without a status is not a validated result");
    }

    let empty = Vec::new();
    let containers = result
        .get("containers")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let winner_score = score_terms(result.get("score"));

    let mut plan_containers = Vec::with_capacity(containers.len());
    for (index, container) in containers.iter().enumerate() {
        let placements = container
            .get("placements")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        let order = match orders.get(index.to_string()) {
            // An explicit null is no order, as a missing entry is.
            None | Some(Value::Null) => None,
            Some(value) => Some(loading_order(index, value, placements.len())?),
        };
        let (kind, listed) = steps(index, placements, order.as_deref())?;
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
        let details = match item.get("details") {
            None | Some(Value::Null) => json!([]),
            Some(details) => details.clone(),
        };
        // Spelled as the reference interpolates them, so a missing reason reads `None` in
        // every engine instead of vanishing in one.
        let summary = format!(
            "Not packed: {} ({}).",
            value_text::text(&reason),
            value_text::text(&level)
        );
        unplaced.push(json!({
            "facts": {
                "item_type": item.get("item_type").cloned().unwrap_or(Value::Null),
                "reason": reason,
                // Carried through unchanged. Softening `observed` into "could not fit"
                // would turn an honest limit into a false certainty.
                "proof_level": level,
                "details": details,
            },
            "presentation": {
                "summary": summary,
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
    Ok(plan)
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
        // RFC 8785: keys sorted by UTF-16 code unit, no whitespace.
        let plan = build_plan_json(&result(), "{}").unwrap();
        assert!(plan.starts_with(r#"{"alternatives":"#), "{plan}");
        assert!(!plan.contains(", "), "{plan}");
    }

    #[test]
    fn the_plan_is_written_in_rfc_8785_canonical_form() {
        let spelled = result()
            .replace(r#""volume_utilization":0.5"#, r#""volume_utilization":1.0"#)
            .replace(r#""objective":"default""#, "\"objective\":\"a\u{2028}b\"");
        let plan = build_plan_json(&spelled, "{}").unwrap();
        assert!(plan.contains(r#""volume_utilization":1}"#), "{plan}");
        assert!(plan.contains("\"objective\":\"a\u{2028}b\""), "{plan}");
    }

    #[test]
    fn score_terms_are_copied_and_compared_by_value_never_filtered() {
        let with_alternative = result()
            .replace(
                r#""score":[0,1,0,0,1000000]"#,
                r#""score":[0,1.5,0,0,1000000]"#,
            )
            .replace(
                r#""alternatives":[]"#,
                r#""alternatives":[{"status":"feasible","score":[0.0,2,0,0,900000]}]"#,
            );
        let plan: Value =
            serde_json::from_str(&build_plan_json(&with_alternative, "{}").unwrap()).unwrap();
        assert_eq!(plan["facts"]["score"], json!([0, 1.5, 0, 0, 1000000]));
        let difference = &plan["alternatives"][0]["facts"]["first_difference"];
        assert_eq!(difference["index"], 1);
        assert_eq!(difference["difference"], json!(0.5));
        let summary = plan["alternatives"][0]["presentation"]["summary"]
            .as_str()
            .unwrap();
        assert!(summary.contains("chosen 1.5, this 2."), "{summary}");

        let unsubtractable = result().replace(
            r#""alternatives":[]"#,
            r#""alternatives":[{"score":[0,"x",0,0,1000000]}]"#,
        );
        let error = build_plan_json(&unsubtractable, "{}").unwrap_err();
        assert!(error.0.contains("not numbers"), "{}", error.0);
    }

    #[test]
    fn a_result_whose_status_is_null_is_not_a_validated_result() {
        let error = build_plan_json(r#"{"status":null,"containers":[]}"#, "{}").unwrap_err();
        assert!(error.0.contains("validated result"), "{}", error.0);
    }

    #[test]
    fn a_null_order_is_no_order_and_a_non_integer_entry_is_refused() {
        assert_eq!(
            plan(r#"{"0":null}"#)["containers"][0]["order"],
            "unavailable"
        );
        let error = build_plan_json(&result(), r#"{"0":[1,"0"]}"#).unwrap_err();
        assert!(error.0.contains("permutation"), "{}", error.0);
    }

    #[test]
    fn an_unpacked_item_without_a_reason_is_summarised_as_the_reference_writes_it() {
        let with_unpacked = result().replace(
            r#""unpacked_items":[]"#,
            r#""unpacked_items":[{"item_type":"ladder","details":null}]"#,
        );
        let plan: Value =
            serde_json::from_str(&build_plan_json(&with_unpacked, "{}").unwrap()).unwrap();
        let unplaced = &plan["unplaced"][0];
        assert_eq!(
            unplaced["presentation"]["summary"],
            "Not packed: None (None)."
        );
        assert_eq!(unplaced["facts"]["details"], json!([]));
    }

    #[test]
    fn the_error_prints_its_own_message() {
        let error = ExecutionPlanError("a placement must be an object".to_string());
        assert_eq!(error.to_string(), "a placement must be an object");
    }

    #[test]
    fn a_placement_that_is_not_an_object_is_refused() {
        let error = placement_reference(0, &json!("cube")).unwrap_err();
        assert!(error.0.contains("must be an object"), "{}", error.0);
    }

    #[test]
    fn a_placement_without_exact_ticks_is_refused_and_the_path_is_named() {
        let mut placement: Value = serde_json::from_str(&placement("cube", 0)).unwrap();
        placement["position"]["x"]
            .as_object_mut()
            .unwrap()
            .remove("ticks");
        let error = placement_reference(0, &placement).unwrap_err();
        assert!(error.0.contains("position.x.ticks"), "{}", error.0);
    }

    #[test]
    fn an_alternative_with_the_same_score_says_so_and_has_no_first_difference() {
        let with_alternative = result().replace(
            r#""alternatives":[]"#,
            r#""alternatives":[{"status":"feasible","score":[0,1,0,0,1000000]}]"#,
        );
        let plan: Value =
            serde_json::from_str(&build_plan_json(&with_alternative, "{}").unwrap()).unwrap();
        let alternative = &plan["alternatives"][0];
        assert_eq!(alternative["facts"]["first_difference"], Value::Null);
        let summary = alternative["presentation"]["summary"].as_str().unwrap();
        assert!(summary.contains("scored identically"), "{summary}");
    }

    #[test]
    fn text_that_is_not_json_is_refused_for_either_input() {
        let error = build_plan_json("{", "{}").unwrap_err();
        assert!(error.0.contains("result is not valid JSON"), "{}", error.0);
        let error = build_plan_json(&result(), "{").unwrap_err();
        assert!(
            error.0.contains("loading orders are not valid JSON"),
            "{}",
            error.0
        );
    }

    #[test]
    fn a_plan_with_a_number_no_engine_holds_exactly_is_refused() {
        let beyond = result().replace(
            r#""score":[0,1,0,0,1000000]"#,
            r#""score":[0,1,0,0,9007199254740993]"#,
        );
        let error = build_plan_json(&beyond, "{}").unwrap_err();
        assert!(error.0.contains("no canonical spelling"), "{}", error.0);
    }
}
