//! Versioned eligibility/policy rules: a closed predicate vocabulary, effective dating
//! and deterministic conflict resolution.
//!
//! Deny takes precedence, the highest priority wins among equals, and ties break on the
//! lexicographically smallest rule id -- never on map iteration order.

use std::collections::BTreeMap;

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PolicyScope {
    Facility,
    Customer,
    Carrier,
    Material,
    Hazmat,
    Temperature,
    Service,
}

impl PolicyScope {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "facility" => Self::Facility,
            "customer" => Self::Customer,
            "carrier" => Self::Carrier,
            "material" => Self::Material,
            "hazmat" => Self::Hazmat,
            "temperature" => Self::Temperature,
            "service" => Self::Service,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Facility => "facility",
            Self::Customer => "customer",
            Self::Carrier => "carrier",
            Self::Material => "material",
            Self::Hazmat => "hazmat",
            Self::Temperature => "temperature",
            Self::Service => "service",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyOperator {
    Equals,
    NotEquals,
    In,
    NotIn,
    Exists,
    Absent,
}

impl PolicyOperator {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "equals" => Self::Equals,
            "not_equals" => Self::NotEquals,
            "in" => Self::In,
            "not_in" => Self::NotIn,
            "exists" => Self::Exists,
            "absent" => Self::Absent,
            _ => return None,
        })
    }

    pub fn is_unary(self) -> bool {
        matches!(self, Self::Exists | Self::Absent)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyAction {
    Allow,
    Reject,
}

impl PolicyAction {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "allow" => Some(Self::Allow),
            "reject" => Some(Self::Reject),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Reject => "reject",
        }
    }
}

/// One condition over the caller-supplied context.
#[derive(Debug, Clone)]
pub struct Predicate {
    pub scope: PolicyScope,
    pub field: String,
    pub operator: PolicyOperator,
    pub value: Value,
}

impl Predicate {
    pub fn matches(&self, context: &serde_json::Map<String, Value>) -> bool {
        match self.operator {
            PolicyOperator::Exists => context.contains_key(&self.field),
            PolicyOperator::Absent => !context.contains_key(&self.field),
            _ => match context.get(&self.field) {
                None => false,
                Some(actual) => match self.operator {
                    PolicyOperator::Equals => values_equal(actual, &self.value),
                    PolicyOperator::NotEquals => !values_equal(actual, &self.value),
                    PolicyOperator::In => contains(&self.value, actual),
                    PolicyOperator::NotIn => !contains(&self.value, actual),
                    PolicyOperator::Exists | PolicyOperator::Absent => unreachable!(),
                },
            },
        }
    }
}

/// Value equality over the JSON scalar types, matching the reference implementation
/// exactly -- including that a boolean equals the integer it stands for, the one place
/// a naive structural comparison would disagree and quietly change a decision.
fn values_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Bool(a), Value::Number(b)) => b.as_i64() == Some(i64::from(*a)),
        (Value::Number(a), Value::Bool(b)) => a.as_i64() == Some(i64::from(*b)),
        (Value::Number(a), Value::Number(b)) => a == b,
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Null, Value::Null) => true,
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| values_equal(x, y))
        }
        _ => false,
    }
}

fn contains(haystack: &Value, needle: &Value) -> bool {
    match haystack {
        Value::Array(entries) => entries.iter().any(|entry| values_equal(entry, needle)),
        Value::String(text) => match needle {
            Value::String(part) => text.contains(part.as_str()),
            _ => false,
        },
        _ => false,
    }
}

/// One immutable, numbered version of one rule id's history.
#[derive(Debug, Clone)]
pub struct Rule {
    pub rule_id: String,
    pub version: i64,
    pub scope: PolicyScope,
    pub action: PolicyAction,
    pub predicates: Vec<Predicate>,
    pub priority: i64,
    pub effective_at: i64,
    pub reason: String,
}

impl Rule {
    pub fn matches(&self, context: &serde_json::Map<String, Value>) -> bool {
        self.predicates
            .iter()
            .all(|predicate| predicate.matches(context))
    }
}

/// The evidence a decision cites.
#[derive(Debug, Clone)]
pub struct Citation {
    pub rule_id: String,
    pub version: i64,
    pub action: PolicyAction,
    pub priority: i64,
    pub reason: String,
}

/// The outcome of one evaluation.
#[derive(Debug, Clone)]
pub struct Decision {
    pub scope: PolicyScope,
    pub allowed: bool,
    pub citation: Option<Citation>,
}

/// Why an explicit policy snapshot could not be resolved.
#[derive(Debug, Clone)]
pub enum SnapshotError {
    /// The same rule id was pinned twice -- a caller error, not a rejection.
    DuplicateRuleId,
    /// A pin named a rule id or version number the registry does not have.
    Rejected(PolicyRejection),
}

/// A rejection the policy model is entitled to make.
#[derive(Debug, Clone)]
pub enum PolicyRejection {
    RuleNotFound { rule_id: String },
    VersionNotFound { rule_id: String, version: i64 },
}

/// Append-only history of policy rules, one history per rule id.
///
/// Complexity: `evaluate` is `O(n * (h + p))` over `n` rule ids, `h` versions each and
/// `p` predicates; `decide` over an already-resolved set is `O(r * p)` with no lookup.
#[derive(Debug, Default)]
pub struct PolicyRegistry {
    histories: BTreeMap<String, Vec<Rule>>,
}

impl PolicyRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn publish(&mut self, mut rule: Rule) -> i64 {
        let history = self.histories.entry(rule.rule_id.clone()).or_default();
        rule.version = history.len() as i64 + 1;
        let version = rule.version;
        history.push(rule);
        version
    }

    pub fn version(&self, rule_id: &str, number: i64) -> Result<&Rule, PolicyRejection> {
        let history = self
            .histories
            .get(rule_id)
            .ok_or_else(|| PolicyRejection::RuleNotFound {
                rule_id: rule_id.to_owned(),
            })?;
        history
            .iter()
            .find(|rule| rule.version == number)
            .ok_or_else(|| PolicyRejection::VersionNotFound {
                rule_id: rule_id.to_owned(),
                version: number,
            })
    }

    /// Resolve and deterministically order an explicit policy snapshot. The pins are
    /// sorted so the snapshot is order-independent; a repeated rule id is a caller
    /// error rather than a silently-deduplicated pin.
    pub fn resolve_versions(&self, pins: &[(String, i64)]) -> Result<Vec<&Rule>, SnapshotError> {
        let mut ids: Vec<&String> = pins.iter().map(|(rule_id, _)| rule_id).collect();
        ids.sort();
        let pinned = ids.len();
        ids.dedup();
        if ids.len() != pinned {
            return Err(SnapshotError::DuplicateRuleId);
        }
        let mut ordered = pins.to_vec();
        ordered.sort();
        ordered
            .iter()
            .map(|(rule_id, number)| {
                self.version(rule_id, *number)
                    .map_err(SnapshotError::Rejected)
            })
            .collect()
    }

    /// One deterministic decision for `scope`/`context` as of `as_of`.
    pub fn evaluate(
        &self,
        scope: PolicyScope,
        context: &serde_json::Map<String, Value>,
        as_of: i64,
    ) -> Decision {
        let effective: Vec<&Rule> = self
            .histories
            .values()
            .filter_map(|history| {
                history
                    .iter()
                    .filter(|rule| rule.effective_at <= as_of)
                    .max_by_key(|rule| (rule.effective_at, rule.version))
            })
            .collect();
        decide(&effective, scope, context)
    }
}

/// Evaluate an already-resolved immutable rule set.
pub fn decide(
    rules: &[&Rule],
    scope: PolicyScope,
    context: &serde_json::Map<String, Value>,
) -> Decision {
    let (rejects, allows): (Vec<&Rule>, Vec<&Rule>) = rules
        .iter()
        .copied()
        .filter(|rule| rule.scope == scope && rule.matches(context))
        .partition(|rule| rule.action == PolicyAction::Reject);
    // Deny takes precedence: an explicit REJECT always outranks an ALLOW for the same
    // context, so one permissive rule can never quietly override a more specific denial.
    let pool = if rejects.is_empty() { allows } else { rejects };
    let winner = pool.into_iter().min_by(|left, right| {
        (-left.priority, &left.rule_id).cmp(&(-right.priority, &right.rule_id))
    });

    match winner {
        None => Decision {
            scope,
            allowed: true,
            citation: None,
        },
        Some(rule) => Decision {
            scope,
            allowed: rule.action == PolicyAction::Allow,
            citation: Some(Citation {
                rule_id: rule.rule_id.clone(),
                version: rule.version,
                action: rule.action,
                priority: rule.priority,
                reason: rule.reason.clone(),
            }),
        },
    }
}
