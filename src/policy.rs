//! Versioned eligibility rules, compiled into the engine's own constraint pipeline.
//!
//! See `docs/POLICY-RULES.md` for the contract. The short version: rules travel in the
//! request as data because an engine is driven over JSON as a subprocess, so a rule
//! registered inside one process has no wire representation and nothing can check that
//! four engines agree about it.
//!
//! Deliberately not a predicate language. `Item::eligible_container_tags`,
//! `Item::incompatible_tags` and `Container::tag_limits` already express the predicates
//! in every engine, so each rule form here compiles to a constraint the pipeline already
//! runs. What a rule adds is only what tags cannot carry: identity, effective dating,
//! priority, and the shipment-scoped facts a request had nowhere to put.

use crate::error::{PackError, PackResult};
use crate::geometry::{Dimensions, Point, Rotation};
use crate::model::{Container, ItemInstance};
use crate::solver::{ConstraintDecision, PlacementConstraint, SolverContext};
use serde_json::Value;
use std::collections::BTreeMap;

/// Facts a rule may select on. Properties of the shipment rather than of any item or
/// container, which is why the request had nowhere to put them before.
pub const SHIPMENT_FACTS: [&str; 4] = ["facility", "customer", "carrier", "service"];

/// Declared facts about the shipment, or the absence of one.
///
/// A fact nobody declared is not a wildcard: a rule naming it simply never participates,
/// so a rule written for one customer cannot silently apply to a shipment that named no
/// customer at all.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShipmentContext {
    facts: BTreeMap<String, String>,
}

impl ShipmentContext {
    fn parse(raw: Option<&Value>, where_: &str) -> PackResult<Self> {
        let Some(raw) = raw else {
            return Ok(Self::default());
        };
        let object = raw
            .as_object()
            .ok_or_else(|| PackError::InvalidInput(format!("{where_} must be an object")))?;
        let mut unknown: Vec<&str> = object
            .keys()
            .map(String::as_str)
            .filter(|key| !SHIPMENT_FACTS.contains(key))
            .collect();
        if !unknown.is_empty() {
            unknown.sort_unstable();
            return Err(PackError::InvalidInput(format!(
                "{where_} names unknown shipment facts: {}",
                unknown.join(", ")
            )));
        }
        let mut facts = BTreeMap::new();
        for (name, value) in object {
            let text = value
                .as_str()
                .filter(|text| !text.is_empty())
                .ok_or_else(|| {
                    PackError::InvalidInput(format!("{where_}.{name} must be a non-empty string"))
                })?;
            facts.insert(name.clone(), text.to_string());
        }
        Ok(Self { facts })
    }

    /// Whether every fact this selector names equals the shipment's own.
    fn satisfied_by(&self, shipment: &Self) -> bool {
        self.facts
            .iter()
            .all(|(name, value)| shipment.facts.get(name) == Some(value))
    }
}

/// What a rule forbids. Exactly three, each compiling to a constraint the pipeline
/// already runs, because the tag fields already express the predicates -- a rule adds
/// identity, dating and priority, not a second vocabulary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuleForm {
    /// No container may hold both tags. Compiles to the compatibility constraint.
    SeparateTags { tag: String, from_tag: String },
    /// An item carrying `item_tag` may only enter a container carrying `container_tag`.
    /// Compiles to the container-eligibility constraint.
    RequireContainerTag {
        item_tag: String,
        container_tag: String,
    },
    /// At most `max` items carrying the tag may share one container. Compiles to the
    /// tag-count constraint.
    LimitTagPerContainer { tag: String, max: i64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicyRule {
    pub id: String,
    pub version: i64,
    pub effective_at: i64,
    pub priority: i64,
    pub applies_to: ShipmentContext,
    pub form: RuleForm,
}

impl PolicyRule {
    pub fn citation(&self) -> String {
        format!("{}@{}", self.id, self.version)
    }
}

fn integer(value: Option<&Value>, where_: &str, minimum: i64) -> PackResult<i64> {
    // `as_i64` already rejects a JSON boolean, which a laxer numeric coercion would
    // silently turn into 1.
    value
        .and_then(Value::as_i64)
        .filter(|number| *number >= minimum)
        .ok_or_else(|| PackError::InvalidInput(format!("{where_} must be an integer >= {minimum}")))
}

fn non_empty_string(value: Option<&Value>, where_: &str) -> PackResult<String> {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .ok_or_else(|| PackError::InvalidInput(format!("{where_} must be a non-empty string")))
}

/// Wire name and the keys its object must carry, in constructor order.
const FORMS: [(&str, [&str; 2]); 3] = [
    ("separate_tags", ["tag", "from_tag"]),
    ("require_container_tag", ["item_tag", "container_tag"]),
    ("limit_tag_per_container", ["tag", "max"]),
];

fn parse_form(name: &str, raw: &Value, where_: &str) -> PackResult<RuleForm> {
    let keys = FORMS
        .iter()
        .find(|(form, _)| *form == name)
        .map(|(_, keys)| keys)
        .expect("caller matched this name against FORMS");
    let object = raw
        .as_object()
        .ok_or_else(|| PackError::InvalidInput(format!("{where_} must be an object")))?;
    let mut unknown: Vec<&str> = object
        .keys()
        .map(String::as_str)
        .filter(|key| !keys.contains(key))
        .collect();
    if !unknown.is_empty() {
        unknown.sort_unstable();
        return Err(PackError::InvalidInput(format!(
            "{where_} has unknown keys: {}",
            unknown.join(", ")
        )));
    }
    let missing: Vec<&str> = keys
        .iter()
        .copied()
        .filter(|key| !object.contains_key(*key))
        .collect();
    if !missing.is_empty() {
        return Err(PackError::InvalidInput(format!(
            "{where_} is missing {}",
            missing.join(", ")
        )));
    }
    Ok(match name {
        "separate_tags" => RuleForm::SeparateTags {
            tag: non_empty_string(object.get("tag"), &format!("{where_}.tag"))?,
            from_tag: non_empty_string(object.get("from_tag"), &format!("{where_}.from_tag"))?,
        },
        "require_container_tag" => RuleForm::RequireContainerTag {
            item_tag: non_empty_string(object.get("item_tag"), &format!("{where_}.item_tag"))?,
            container_tag: non_empty_string(
                object.get("container_tag"),
                &format!("{where_}.container_tag"),
            )?,
        },
        _ => RuleForm::LimitTagPerContainer {
            tag: non_empty_string(object.get("tag"), &format!("{where_}.tag"))?,
            max: integer(object.get("max"), &format!("{where_}.max"), 0)?,
        },
    })
}

fn parse_rule(raw: &Value, index: usize) -> PackResult<PolicyRule> {
    let where_ = format!("policy.rules[{index}]");
    let object = raw
        .as_object()
        .ok_or_else(|| PackError::InvalidInput(format!("{where_} must be an object")))?;
    let named: Vec<&str> = FORMS
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| object.contains_key(*name))
        .collect();
    if named.len() != 1 {
        // A rule naming two forms would have no single meaning for a citation.
        let mut forms: Vec<&str> = FORMS.iter().map(|(name, _)| *name).collect();
        forms.sort_unstable();
        return Err(PackError::InvalidInput(format!(
            "{where_} must name exactly one rule form ({}), not {}",
            forms.join(", "),
            named.len()
        )));
    }
    Ok(PolicyRule {
        id: non_empty_string(object.get("id"), &format!("{where_}.id"))?,
        version: integer(object.get("version"), &format!("{where_}.version"), 1)?,
        effective_at: integer(
            object.get("effective_at"),
            &format!("{where_}.effective_at"),
            0,
        )?,
        priority: integer(object.get("priority"), &format!("{where_}.priority"), 0)?,
        applies_to: ShipmentContext::parse(
            object.get("applies_to"),
            &format!("{where_}.applies_to"),
        )?,
        form: parse_form(
            named[0],
            &object[named[0]],
            &format!("{where_}.{}", named[0]),
        )?,
    })
}

/// The rules that participate in one request, already resolved and ordered.
///
/// Resolution is fixed by the contract and must be identical in every engine, or the
/// same request packs differently depending on which one answered it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PolicyRuleSet {
    pub rules: Vec<PolicyRule>,
}

impl PolicyRuleSet {
    pub fn parse(raw: Option<&Value>) -> PackResult<Self> {
        let Some(raw) = raw else {
            return Ok(Self::default());
        };
        let object = raw
            .as_object()
            .ok_or_else(|| PackError::InvalidInput("policy must be an object".into()))?;
        let mut unknown: Vec<&str> = object
            .keys()
            .map(String::as_str)
            .filter(|key| !matches!(*key, "as_of" | "shipment" | "rules"))
            .collect();
        if !unknown.is_empty() {
            unknown.sort_unstable();
            return Err(PackError::InvalidInput(format!(
                "policy has unknown keys: {}",
                unknown.join(", ")
            )));
        }
        let declared = match object.get("rules") {
            None | Some(Value::Null) => &[][..],
            Some(value) => value
                .as_array()
                .map(Vec::as_slice)
                .ok_or_else(|| PackError::InvalidInput("policy.rules must be an array".into()))?,
        };
        if declared.is_empty() {
            return Ok(Self::default());
        }
        if !object.contains_key("as_of") {
            // No default: a guessed instant silently activates or hides a restriction,
            // and reading a clock here would make one request pack differently on
            // different days.
            return Err(PackError::InvalidInput(
                "policy.as_of is required whenever policy.rules is non-empty".into(),
            ));
        }
        let as_of = integer(object.get("as_of"), "policy.as_of", 0)?;
        let shipment = ShipmentContext::parse(object.get("shipment"), "policy.shipment")?;
        let mut parsed = Vec::with_capacity(declared.len());
        for (index, rule) in declared.iter().enumerate() {
            parsed.push(parse_rule(rule, index)?);
        }
        Ok(Self {
            rules: resolve(parsed, as_of, &shipment),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// The rule that rules this item out of every offered container, if one does.
    ///
    /// Only `RequireContainerTag` can be answered here, and that is not a gap. It is a
    /// statement about the request alone -- this item carries the tag, no offered
    /// container carries the one it requires -- so it holds however the search goes.
    /// Segregation and per-container caps depend on what else was packed, so an item
    /// they leave behind was left behind by the search, and reporting that as proven
    /// would claim more than the engine knows.
    ///
    /// `O(r * c)` for `r` rules and `c` containers, once per unpacked item rather than
    /// per candidate.
    pub fn proves_unplaceable(
        &self,
        item: &ItemInstance,
        containers: &[Container],
    ) -> Option<String> {
        self.rules.iter().find_map(|rule| {
            let RuleForm::RequireContainerTag {
                item_tag,
                container_tag,
            } = &rule.form
            else {
                return None;
            };
            if !item.item.tags.contains(item_tag.as_str()) {
                return None;
            }
            if containers
                .iter()
                .any(|container| container.tags.contains(container_tag.as_str()))
            {
                return None;
            }
            Some(format!(
                "{}: requires a container tagged '{container_tag}', which none of the \
                 containers offered carries",
                rule.citation()
            ))
        })
    }
}

fn resolve(rules: Vec<PolicyRule>, as_of: i64, shipment: &ShipmentContext) -> Vec<PolicyRule> {
    // Append-only per id: among participating versions of one id the highest
    // `effective_at` wins, ties broken by the highest `version`. The same resolution the
    // catalog registry already uses for `as_of` lookups, deliberately, so a reader learns
    // one rule and not two. A BTreeMap rather than a HashMap because the values are read
    // back in key order below and hash order would not be deterministic.
    let mut latest: BTreeMap<String, PolicyRule> = BTreeMap::new();
    for rule in rules {
        if rule.effective_at > as_of || !rule.applies_to.satisfied_by(shipment) {
            continue;
        }
        match latest.get(&rule.id) {
            Some(current)
                if (current.effective_at, current.version) >= (rule.effective_at, rule.version) => {
            }
            _ => {
                latest.insert(rule.id.clone(), rule);
            }
        }
    }
    let mut resolved: Vec<PolicyRule> = latest.into_values().collect();
    // Citation order, not evaluation order: the first rule that rejects a candidate is
    // the one cited, so sorting here is what makes the citation deterministic. Ties go to
    // the lexicographically smallest id -- never to the order the caller happened to
    // write, and never to hash order.
    resolved.sort_by(|a, b| b.priority.cmp(&a.priority).then_with(|| a.id.cmp(&b.id)));
    resolved
}

/// Rejects a candidate placement that any participating rule forbids.
///
/// `O(m + r)` per candidate for `m` placements already in the container and `r` resolved
/// rules: one pass collecting the tags present, then one pass over the rules. That is the
/// same bound class as the built-in compatibility and tag-count checks the forms compile
/// onto, so the published complexity bounds are unchanged.
///
/// Rules arrive in citation order, so the first rejection is already the one the contract
/// says to cite -- highest priority, ties to the smallest id.
#[derive(Debug)]
pub struct PolicyConstraint {
    rules: Vec<PolicyRule>,
}

impl PolicyConstraint {
    pub fn new(rules: Vec<PolicyRule>) -> Self {
        Self { rules }
    }
}

impl PlacementConstraint for PolicyConstraint {
    fn evaluate(
        &self,
        context: &SolverContext<'_>,
        item: &ItemInstance,
        _point: Point,
        _rotation: Rotation,
        _dimensions: Dimensions,
    ) -> ConstraintDecision {
        let item_tags = &item.item.tags;
        let mut present: Option<BTreeMap<&str, usize>> = None;
        for rule in &self.rules {
            match &rule.form {
                RuleForm::RequireContainerTag {
                    item_tag,
                    container_tag,
                } => {
                    if item_tags.contains(item_tag.as_str())
                        && !context.container.tags.contains(container_tag.as_str())
                    {
                        return ConstraintDecision::reject("policy_rule");
                    }
                }
                // Both remaining forms need to know what is already in the container, so
                // the walk happens once and only when a rule of that kind exists at all.
                RuleForm::SeparateTags { tag, from_tag } => {
                    let counts = present.get_or_insert_with(|| tag_counts(context));
                    let shares = |carried: &str, other: &str| {
                        item_tags.contains(carried) && counts.get(other).is_some_and(|n| *n > 0)
                    };
                    if shares(tag, from_tag) || shares(from_tag, tag) {
                        return ConstraintDecision::reject("policy_rule");
                    }
                }
                RuleForm::LimitTagPerContainer { tag, max } => {
                    let counts = present.get_or_insert_with(|| tag_counts(context));
                    let already = counts.get(tag.as_str()).copied().unwrap_or(0);
                    if item_tags.contains(tag.as_str()) && already as i64 >= *max {
                        return ConstraintDecision::reject("policy_rule");
                    }
                }
            }
        }
        ConstraintDecision::allow()
    }
}

fn tag_counts<'a>(context: &SolverContext<'a>) -> BTreeMap<&'a str, usize> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for placement in context.placements {
        for tag in &placement.instance.item.tags {
            *counts.entry(tag.as_str()).or_insert(0) += 1;
        }
    }
    counts
}
