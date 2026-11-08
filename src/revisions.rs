//! Plan revisions: an append-only chain of exceptions against approved plans.
//!
//! `docs/PLAN-REVISIONS.md` is the contract, and `packvium.revisions` the reference. A
//! revision records what differed from an approved plan -- a missing item, a substituted
//! carton, a lock, a verified placement -- and derives the request the next plan solves. It is
//! a pure function of its parent, the approved artifact and its events: it calls no solver,
//! validator or clock, so four builders emit the same bytes.
//!
//! Every entry point takes and returns JSON text, the shape a conformance probe drives over a
//! pipe, and the same reason the artifact builder takes text.

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap, HashSet};

use crate::artifacts;
use crate::canonical_json::{
    self, CanonicalJsonError, CanonicalJsonErrorCode, json_integer, json_spelling, spell_names,
};
use crate::fixed::{hull_item_types, point_shape_problem, unknown_keys};
use crate::made_to_measure::{per_square_metre, surface_area};
use crate::request_errors::orientation_refusal;
use crate::units::{Length, Weight};

pub const FORMAT: &str = "packvium-plan-revision/v1";

pub const EVENT_TYPES: [&str; 4] = [
    "item_missing",
    "container_substituted",
    "placement_locked",
    "placement_verified",
];

const PLACEMENT_FIELDS: [&str; 6] = [
    "item_type",
    "container_type",
    "container_instance",
    "position",
    "orientation",
    "placement_id",
];

/// The closed set of refusals, shared by all four engines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RevisionErrorCode {
    InvalidRevision,
    InvalidEvent,
    EventConflict,
    InvalidArtifact,
    InvalidJson,
    NumberOutOfRange,
    InvalidString,
}

impl RevisionErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRevision => "invalid_revision",
            Self::InvalidEvent => "invalid_event",
            Self::EventConflict => "event_conflict",
            Self::InvalidArtifact => "invalid_artifact",
            Self::InvalidJson => "invalid_json",
            Self::NumberOutOfRange => "number_out_of_range",
            Self::InvalidString => "invalid_string",
        }
    }
}

/// A revision could not be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionError {
    kind: RevisionErrorCode,
    message: String,
}

impl RevisionError {
    fn new(kind: RevisionErrorCode, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> RevisionErrorCode {
        self.kind
    }

    /// The closed code, e.g. `event_conflict`: what a caller in any engine matches on.
    pub fn code(&self) -> &'static str {
        self.kind.as_str()
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for RevisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code(), self.message)
    }
}

impl std::error::Error for RevisionError {}

impl From<CanonicalJsonError> for RevisionError {
    fn from(error: CanonicalJsonError) -> Self {
        let kind = match error.code {
            CanonicalJsonErrorCode::NumberOutOfRange => RevisionErrorCode::NumberOutOfRange,
            CanonicalJsonErrorCode::InvalidString => RevisionErrorCode::InvalidString,
            CanonicalJsonErrorCode::InvalidJson => RevisionErrorCode::InvalidJson,
        };
        Self::new(kind, error.message)
    }
}

type RevisionResult<T> = Result<T, RevisionError>;

fn refuse<T>(kind: RevisionErrorCode, message: impl Into<String>) -> RevisionResult<T> {
    Err(RevisionError::new(kind, message))
}

/// Revision 0: the request as first approved, with nothing recorded against it.
pub fn root_revision_json(request_json: &str) -> Result<String, RevisionError> {
    let request = read(request_json)?;
    if !request.is_object() {
        return refuse(
            RevisionErrorCode::InvalidRevision,
            "a request is a JSON object",
        );
    }
    canonical(&document(0, Value::Null, Value::Null, Vec::new(), request))
}

/// The next revision: `events_json`, observed against `artifact_json`, applied to
/// `parent_json`.
///
/// Copies and hashes the request/artifact in their serialized size. Placement-only replay
/// indexes canonical keys; missing-item and carton events still scan request lists.
pub fn derive_revision_json(
    parent_json: &str,
    artifact_json: &str,
    events_json: &str,
) -> Result<String, RevisionError> {
    let parent = read(parent_json)?;
    let artifact = read(artifact_json)?;
    let events = read(events_json)?;
    require_revision(&parent)?;
    require_artifact(&artifact, &parent["request"])?;
    let Some(events) = events.as_array().filter(|events| !events.is_empty()) else {
        return refuse(
            RevisionErrorCode::InvalidEvent,
            "a revision records at least one event",
        );
    };
    // `require_revision` bounded the parent's number and sequences by 2^53 - 1, so neither
    // `+ 1` below can overflow.
    let first = last_sequence(&parent)? + 1;
    let hulls = hull_item_types(parent["request"].get("items"));
    let recorded = events
        .iter()
        .zip(first..)
        .map(|(event, sequence)| checked_event(event, sequence, &hulls))
        .collect::<RevisionResult<Vec<_>>>()?;
    let number = revision_number(&parent) + 1;
    let request = stock_cut_boxes(
        &apply_events(&parent["request"], &recorded)?,
        &artifact,
        number,
    )?;
    let approved = json!({
        "artifact": digest(&artifact)?,
        "replay": artifact["provenance"]["replay"].clone(),
    });
    canonical(&document(
        number,
        Value::String(digest(&parent)?),
        approved,
        recorded,
        request,
    ))
}

/// The request these events derive from `request_json`, as canonical JSON. Refuses a
/// contradiction, never physics: whether the new fixed set can hold is the engine's
/// admission check when it is solved.
pub fn apply_events_json(request_json: &str, events_json: &str) -> Result<String, RevisionError> {
    let request = read(request_json)?;
    let events = read(events_json)?;
    if !request.is_object() {
        return refuse(
            RevisionErrorCode::InvalidRevision,
            "a request is a JSON object",
        );
    }
    let Some(events) = events.as_array() else {
        return refuse(RevisionErrorCode::InvalidEvent, "events are a JSON array");
    };
    canonical(&apply_events(&request, events)?)
}

/// `request_json` with every made-to-measure box that holds a fixed placement made a stock
/// container, as canonical JSON: [`stock_cut_boxes`] on text.
pub fn stock_cut_boxes_json(
    request_json: &str,
    artifact_json: &str,
    revision: i64,
) -> Result<String, RevisionError> {
    canonical(&stock_cut_boxes(
        &read(request_json)?,
        &read(artifact_json)?,
        revision,
    )?)
}

/// Every way the chain fails to be what its root and events derive, without stopping at the
/// first, as a canonical JSON array of `{code, revision, detail}`. `artifacts_json`, when
/// given, is an array whose entry `k` is the artifact revision `k` names as approved.
pub fn verify_revision_chain_json(
    revisions_json: &str,
    artifacts_json: Option<&str>,
) -> Result<String, RevisionError> {
    let revisions = read(revisions_json)?;
    let Some(revisions) = revisions.as_array() else {
        return refuse(
            RevisionErrorCode::InvalidRevision,
            "a chain is a JSON array",
        );
    };
    let artifacts = artifacts_json
        .map(read)
        .transpose()?
        .filter(|artifacts| !artifacts.is_null());
    let artifacts = match &artifacts {
        None => None,
        Some(Value::Array(list)) => Some(list.as_slice()),
        Some(_) => {
            return refuse(
                RevisionErrorCode::InvalidArtifact,
                "artifacts is a JSON array",
            );
        }
    };
    let issues = verify_chain(revisions, artifacts)?;
    canonical(&Value::Array(
        issues
            .into_iter()
            .map(|(code, revision, detail)| {
                json!({"code": code, "revision": revision, "detail": detail})
            })
            .collect(),
    ))
}

/// `sha256:` and the hex SHA-256 of a document's canonical bytes.
pub fn document_digest_json(document_json: &str) -> Result<String, RevisionError> {
    digest(&read(document_json)?)
}

/// A document's RFC 8785 canonical form, the bytes a digest and four engines compare.
pub fn canonical_revision_json(document_json: &str) -> Result<String, RevisionError> {
    canonical(&read(document_json)?)
}

// ------------------------------------------------------------------------------ building

fn read(text: &str) -> RevisionResult<Value> {
    Ok(canonical_json::parse(text)?)
}

fn canonical(value: &Value) -> RevisionResult<String> {
    Ok(canonical_json::to_canonical_string(value)?)
}

fn digest(value: &Value) -> RevisionResult<String> {
    let bytes = canonical(value)?;
    let hash = Sha256::digest(bytes.as_bytes());
    Ok(format!(
        "sha256:{}",
        hash.iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
}

fn document(
    number: i64,
    parent: Value,
    approved: Value,
    events: Vec<Value>,
    request: Value,
) -> Value {
    json!({
        "format": FORMAT,
        "suite_version": artifacts::SUITE_VERSION,
        "revision": number,
        "parent": parent,
        "approved": approved,
        "events": events,
        "request": request,
    })
}

fn require_revision(document: &Value) -> RevisionResult<()> {
    if document.get("format").and_then(Value::as_str) != Some(FORMAT) {
        return refuse(
            RevisionErrorCode::InvalidRevision,
            format!("not a {FORMAT} document"),
        );
    }
    if integer(&document["revision"]).is_none_or(|number| number < 0) {
        return refuse(
            RevisionErrorCode::InvalidRevision,
            "revision is a non-negative integer",
        );
    }
    if !document["request"].is_object() {
        return refuse(
            RevisionErrorCode::InvalidRevision,
            "a revision carries its request",
        );
    }
    let Some(events) = document["events"].as_array() else {
        return refuse(
            RevisionErrorCode::InvalidRevision,
            "a revision carries its events",
        );
    };
    if !events
        .iter()
        .all(|event| event.is_object() && integer(&event["sequence"]).is_some())
    {
        return refuse(
            RevisionErrorCode::InvalidRevision,
            "a revision's events each carry an integer sequence",
        );
    }
    Ok(())
}

/// A revision's number, once `require_revision` has admitted it.
fn revision_number(revision: &Value) -> i64 {
    integer(&revision["revision"]).unwrap_or_default()
}

/// An admitted event's sequence: `require_revision` or `apply_events` vouched for it.
fn sequence_of(event: &Value) -> i64 {
    integer(&event["sequence"]).unwrap_or_default()
}

fn require_artifact(artifact: &Value, request: &Value) -> RevisionResult<()> {
    if artifact.get("format").and_then(Value::as_str) != Some(artifacts::FORMAT) {
        return refuse(
            RevisionErrorCode::InvalidArtifact,
            format!("not a {} document", artifacts::FORMAT),
        );
    }
    let provenance = &artifact["provenance"];
    if !provenance.is_object() || !provenance["replay"].is_object() {
        return refuse(
            RevisionErrorCode::InvalidArtifact,
            "the artifact carries no provenance.replay",
        );
    }
    if !same(&provenance["request"], request)? {
        return refuse(
            RevisionErrorCode::InvalidArtifact,
            "the artifact was built from a different request than the parent's",
        );
    }
    Ok(())
}

fn last_sequence(revision: &Value) -> RevisionResult<i64> {
    match revision["events"]
        .as_array()
        .and_then(|events| events.last())
    {
        Some(event) => Ok(sequence_of(event)),
        None if revision_number(revision) == 0 => Ok(0),
        None => refuse(
            RevisionErrorCode::InvalidRevision,
            "only the root revision records no events",
        ),
    }
}

fn checked_event(raw: &Value, sequence: i64, hulls: &BTreeSet<&str>) -> RevisionResult<Value> {
    require_shape(raw, hulls)?;
    if integer(&raw["sequence"]) != Some(sequence) {
        return refuse(
            RevisionErrorCode::InvalidEvent,
            format!(
                "event sequence {} does not continue the chain at {sequence}",
                json_spelling(&raw["sequence"])
            ),
        );
    }
    Ok(raw.clone())
}

fn event_fields(kind: &str) -> Option<&'static [&'static str]> {
    match kind {
        "item_missing" => Some(&["item_type", "quantity"]),
        "container_substituted" => Some(&["container_type", "replacement"]),
        "placement_locked" | "placement_verified" => Some(&["placement"]),
        _ => None,
    }
}

/// An event of a known type carrying exactly its fields, each by its own rule. `hulls` names
/// the request's hull item types, which alone may be placed in a signed orientation (1.10).
fn require_shape(raw: &Value, hulls: &BTreeSet<&str>) -> RevisionResult<()> {
    let Some(event) = raw.as_object() else {
        return refuse(RevisionErrorCode::InvalidEvent, "an event is a JSON object");
    };
    let kind = &raw["type"];
    let Some((kind, fields)) = kind
        .as_str()
        .and_then(|kind| event_fields(kind).map(|fields| (kind, fields)))
    else {
        return refuse(
            RevisionErrorCode::InvalidEvent,
            format!("unknown event type {}", json_spelling(kind)),
        );
    };
    let allowed = ["sequence", "type"]
        .into_iter()
        .chain(fields.iter().copied())
        .collect::<Vec<_>>();
    let unknown = unknown_keys(event, &allowed);
    if !unknown.is_empty() {
        return refuse(
            RevisionErrorCode::InvalidEvent,
            format!("{kind} does not carry {}", spell_names(&unknown)),
        );
    }
    let missing = fields
        .iter()
        .copied()
        .filter(|field| !event.contains_key(*field))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return refuse(
            RevisionErrorCode::InvalidEvent,
            format!("{kind} needs {}", spell_names(&missing)),
        );
    }
    match kind {
        "item_missing" => validate_item_missing(event),
        "container_substituted" => validate_container_substituted(event),
        _ => validate_placement(event, hulls),
    }
}

fn validate_item_missing(event: &Map<String, Value>) -> RevisionResult<()> {
    require_name(&event["item_type"], "item_type")?;
    if integer(&event["quantity"]).is_none_or(|quantity| quantity < 1) {
        return refuse(
            RevisionErrorCode::InvalidEvent,
            "item_missing.quantity is a positive integer",
        );
    }
    Ok(())
}

fn validate_container_substituted(event: &Map<String, Value>) -> RevisionResult<()> {
    require_name(&event["container_type"], "container_type")?;
    let Some(replacement) = event["replacement"].as_object() else {
        return refuse(
            RevisionErrorCode::InvalidEvent,
            "a replacement is a container object",
        );
    };
    require_name(
        replacement.get("id").unwrap_or(&Value::Null),
        "replacement.id",
    )
}

fn validate_placement(event: &Map<String, Value>, hulls: &BTreeSet<&str>) -> RevisionResult<()> {
    let Some(placement) = event["placement"].as_object() else {
        return refuse(
            RevisionErrorCode::InvalidEvent,
            "a placement is a fixed-placement object",
        );
    };
    let unknown = unknown_keys(placement, &PLACEMENT_FIELDS);
    if !unknown.is_empty() {
        return refuse(
            RevisionErrorCode::InvalidEvent,
            format!("a placement does not carry {}", spell_names(&unknown)),
        );
    }
    let field = |name: &str| placement.get(name).unwrap_or(&Value::Null);
    require_name(field("item_type"), "placement.item_type")?;
    require_name(field("container_type"), "placement.container_type")?;
    let hull = field("item_type")
        .as_str()
        .is_some_and(|item_type| hulls.contains(item_type));
    if let Some((_, detail)) =
        orientation_refusal(field("orientation"), hull, "placement.orientation")
    {
        return refuse(RevisionErrorCode::InvalidEvent, detail);
    }
    let instance = placement.get("container_instance").map_or(Some(1), integer);
    if instance.is_none_or(|instance| instance < 1) {
        return refuse(
            RevisionErrorCode::InvalidEvent,
            "placement.container_instance counts from 1",
        );
    }
    if let Some(problem) = placement
        .get("position")
        .and_then(|position| point_shape_problem(position, "placement.position"))
    {
        return refuse(RevisionErrorCode::InvalidEvent, problem);
    }
    // Only its type is the event's to judge: whether it names this placement needs the
    // request's unit, and is the engine's admission rule when the revision is solved (1.10).
    if placement
        .get("placement_id")
        .is_some_and(|identifier| !(identifier.is_null() || identifier.is_string()))
    {
        return refuse(
            RevisionErrorCode::InvalidEvent,
            "placement.placement_id is a string",
        );
    }
    Ok(())
}

fn require_name(value: &Value, field: &str) -> RevisionResult<()> {
    if value.as_str().is_none_or(str::is_empty) {
        return refuse(
            RevisionErrorCode::InvalidEvent,
            format!("{field} is a non-empty string"),
        );
    }
    Ok(())
}

// ------------------------------------------------------------------------------ applying

fn apply_events(request: &Value, events: &[Value]) -> RevisionResult<Value> {
    let mut derived = request.clone();
    let hulls = hull_item_types(request.get("items"));
    let mut placement_index = PlacementIndex::new();
    for event in events {
        require_shape(event, &hulls)?;
        if integer(&event["sequence"]).is_none() {
            return refuse(
                RevisionErrorCode::InvalidEvent,
                format!(
                    "event sequence {} is not an integer",
                    json_spelling(&event["sequence"])
                ),
            );
        }
        match event["type"].as_str() {
            Some("item_missing") => apply_item_missing(&mut derived, event)?,
            Some("container_substituted") => apply_container_substituted(&mut derived, event)?,
            _ => apply_placement(&mut derived, event, &mut placement_index)?,
        }
    }
    Ok(derived)
}

fn apply_item_missing(request: &mut Value, event: &Value) -> RevisionResult<()> {
    let item_type = event["item_type"].as_str().unwrap_or_default();
    let sequence = sequence_of(event);
    let items = entries(request, "items")?;
    let index = index_of(items, item_type, "item")?;
    let quantity = match items[index].get("quantity") {
        None => 1,
        Some(quantity) => integer(quantity).ok_or_else(|| {
            RevisionError::new(
                RevisionErrorCode::InvalidRevision,
                format!("request.items[{index}].quantity is an integer"),
            )
        })?,
    };
    // Both are within 2^53 - 1 (`require_shape` checked the event's), so this cannot wrap.
    let remaining = quantity - integer(&event["quantity"]).unwrap_or_default();
    let fixed = fixed_entries(request)?
        .iter()
        .filter(|entry| entry["item_type"].as_str() == Some(item_type))
        .count() as i64;
    let items = entries(request, "items")?;
    if remaining < fixed {
        return refuse(
            RevisionErrorCode::EventConflict,
            format!(
                "event {sequence}: {fixed} {item_type} are fixed, {} would remain",
                remaining.max(0)
            ),
        );
    }
    if remaining > 0 {
        items[index]["quantity"] = json!(remaining);
        return Ok(());
    }
    if items.len() == 1 {
        return refuse(
            RevisionErrorCode::EventConflict,
            format!("event {sequence}: no item would remain to pack"),
        );
    }
    items.remove(index);
    Ok(())
}

fn apply_container_substituted(request: &mut Value, event: &Value) -> RevisionResult<()> {
    let container_type = event["container_type"].as_str().unwrap_or_default();
    let sequence = sequence_of(event);
    let index = index_of(entries(request, "containers")?, container_type, "container")?;
    let named_by_fixed = fixed_entries(request)?
        .iter()
        .any(|entry| entry["container_type"].as_str() == Some(container_type));
    let containers = entries(request, "containers")?;
    if named_by_fixed {
        return refuse(
            RevisionErrorCode::EventConflict,
            format!("event {sequence}: fixed placements are in {container_type}"),
        );
    }
    let replacement_id = event["replacement"]["id"].as_str().unwrap_or_default();
    if containers.iter().enumerate().any(|(position, container)| {
        position != index && container["id"].as_str() == Some(replacement_id)
    }) {
        return refuse(
            RevisionErrorCode::EventConflict,
            format!("event {sequence}: another container is already {replacement_id}"),
        );
    }
    containers[index] = event["replacement"].clone();
    Ok(())
}

struct PlacementIndex {
    initialized: bool,
    indexable: bool,
    keys: BTreeSet<String>,
}

impl PlacementIndex {
    fn new() -> Self {
        Self {
            initialized: false,
            indexable: true,
            keys: BTreeSet::new(),
        }
    }
}

fn apply_placement(
    request: &mut Value,
    event: &Value,
    index: &mut PlacementIndex,
) -> RevisionResult<()> {
    let placement = &event["placement"];
    let wanted = identity(placement);
    if !index.initialized {
        index.initialized = true;
        for entry in fixed_entries(request)? {
            match canonical(&identity(entry)) {
                Ok(key) => {
                    index.keys.insert(key);
                }
                Err(_) => {
                    index.indexable = false;
                    break;
                }
            }
        }
    }
    // One box cannot be two fixed items: a lock that is later verified in place is recorded
    // twice in the chain and once in the request.
    if index.indexable {
        match canonical(&wanted) {
            Ok(key) => {
                if index.keys.contains(&key) {
                    return Ok(());
                }
                append_fixed(request, placement);
                index.keys.insert(key);
                return Ok(());
            }
            Err(_) => index.indexable = false,
        }
    }
    // An uncanonicalizable direct-call value must keep the former first-match/error order.
    for entry in fixed_entries(request)? {
        if same(&identity(entry), &wanted)? {
            return Ok(());
        }
    }
    append_fixed(request, placement);
    Ok(())
}

fn append_fixed(request: &mut Value, placement: &Value) {
    // Every entry point admits only an object request, and `fixed_entries` an absent, null or
    // list value; the first two start a list.
    if let Some(object) = request.as_object_mut() {
        match object.get_mut("fixed_placements") {
            Some(Value::Array(list)) => list.push(placement.clone()),
            _ => {
                object.insert(
                    "fixed_placements".into(),
                    Value::Array(vec![placement.clone()]),
                );
            }
        }
    }
}

/// What makes two fixed entries one box: everything but `placement_id`, which only restates
/// the container and position (1.10), with the default instance spelled out.
fn identity(placement: &Value) -> Value {
    let mut copy = placement.clone();
    if let Some(object) = copy.as_object_mut() {
        object.remove("placement_id");
        object
            .entry("container_instance")
            .or_insert_with(|| json!(1));
    }
    copy
}

/// The request's fixed placements; absent or null is none, and anything else that is not a
/// list of objects is a request no event can safely edit.
fn fixed_entries(request: &Value) -> RevisionResult<&[Value]> {
    match &request["fixed_placements"] {
        Value::Null => Ok(&[]),
        Value::Array(list) if list.iter().all(Value::is_object) => Ok(list),
        _ => refuse(
            RevisionErrorCode::InvalidRevision,
            "request.fixed_placements is a list of objects",
        ),
    }
}

fn entries<'a>(request: &'a mut Value, key: &str) -> RevisionResult<&'a mut Vec<Value>> {
    match request.get_mut(key).and_then(Value::as_array_mut) {
        Some(list) if list.iter().all(Value::is_object) => Ok(list),
        _ => refuse(
            RevisionErrorCode::InvalidRevision,
            format!("request.{key} is a list of objects"),
        ),
    }
}

fn index_of(entries: &[Value], identifier: &str, kind: &str) -> RevisionResult<usize> {
    entries
        .iter()
        .position(|entry| entry["id"].as_str() == Some(identifier))
        .map_or_else(
            || {
                refuse(
                    RevisionErrorCode::EventConflict,
                    format!(
                        "the request has no {kind} {}",
                        json_spelling(&Value::String(identifier.to_owned()))
                    ),
                )
            },
            Ok,
        )
}

// --------------------------------------------------------------------------------- audit

type Issue = (&'static str, usize, String);

fn verify_chain(revisions: &[Value], artifacts: Option<&[Value]>) -> RevisionResult<Vec<Issue>> {
    let mut issues = Vec::new();
    let mut last = 0_i64;
    for (position, revision) in revisions.iter().enumerate() {
        if let Err(error) = require_revision(revision) {
            issues.push(("invalid_revision", position, error.message));
            return Ok(issues);
        }
        let number = revision_number(revision);
        if number != position as i64 {
            issues.push((
                "revision_number",
                position,
                format!("revision {number} at position {position}"),
            ));
        }
        let parent = position.checked_sub(1).map(|index| &revisions[index]);
        let expected = parent.map(digest).transpose()?;
        let recorded = revision["parent"].as_str().map(str::to_owned);
        if recorded != expected || (recorded.is_none() && !revision["parent"].is_null()) {
            issues.push((
                "parent_mismatch",
                position,
                format!(
                    "parent {}, expected {}",
                    plain(&revision["parent"]),
                    expected.as_deref().unwrap_or("null")
                ),
            ));
        }
        last = check_sequences(revision, position, last, &mut issues);
        let Some(parent) = parent else {
            continue;
        };
        let artifact = artifacts
            .and_then(|artifacts| artifacts.get(position))
            .filter(|artifact| !artifact.is_null());
        check_request(revision, parent, artifact, position, &mut issues)?;
        if let Some(artifact) = artifact {
            check_artifact(revision, parent, artifact, position, &mut issues)?;
        }
    }
    Ok(issues)
}

fn check_sequences(
    revision: &Value,
    position: usize,
    mut last: i64,
    issues: &mut Vec<Issue>,
) -> i64 {
    let events = revision["events"].as_array().map_or(&[][..], Vec::as_slice);
    if position == 0 && !events.is_empty() {
        issues.push((
            "sequence_gap",
            0,
            "the root revision records no events".into(),
        ));
    }
    if position > 0 && events.is_empty() {
        issues.push((
            "sequence_gap",
            position,
            "a revision records at least one event".into(),
        ));
    }
    // `require_revision` admitted every sequence, bounded by 2^53 - 1, so `last + 1` cannot
    // overflow.
    for event in events {
        let sequence = sequence_of(event);
        if sequence != last + 1 {
            issues.push((
                "sequence_gap",
                position,
                format!("event {sequence} where {} was next", last + 1),
            ));
        }
        last = sequence;
    }
    last
}

fn check_request(
    revision: &Value,
    parent: &Value,
    artifact: Option<&Value>,
    position: usize,
    issues: &mut Vec<Issue>,
) -> RevisionResult<()> {
    let events = revision["events"].as_array().map_or(&[][..], Vec::as_slice);
    let derived = apply_events(&parent["request"], events);
    let derived = match (derived, artifact) {
        (Ok(derived), None) if holds_cut_boxes(&derived) => {
            // The cut sizes live only in the approved artifact.
            issues.push((
                "artifact_required",
                position,
                "fixed items in a made-to-measure box are checked against the approved artifact"
                    .into(),
            ));
            return Ok(());
        }
        (Ok(derived), Some(artifact)) if holds_cut_boxes(&derived) => {
            stock_cut_boxes(&derived, artifact, revision_number(revision))
        }
        (derived, _) => derived,
    };
    match derived {
        Err(error) => issues.push((
            "request_mismatch",
            position,
            format!("the events do not apply: {}", error.message),
        )),
        Ok(derived) if !same(&derived, &revision["request"])? => issues.push((
            "request_mismatch",
            position,
            "the request is not what the parent's request and these events derive".into(),
        )),
        Ok(_) => {}
    }
    Ok(())
}

fn check_artifact(
    revision: &Value,
    parent: &Value,
    artifact: &Value,
    position: usize,
    issues: &mut Vec<Issue>,
) -> RevisionResult<()> {
    let approved = &revision["approved"];
    // An artifact with no canonical form has no digest, so no approval can name it.
    let matches =
        digest(artifact).is_ok_and(|found| approved["artifact"].as_str() == Some(found.as_str()));
    if !approved.is_object() || !matches {
        issues.push((
            "artifact_mismatch",
            position,
            "the artifact's digest is not the one this revision approved".into(),
        ));
        return Ok(());
    }
    let provenance = &artifact["provenance"];
    if !provenance.is_object() || !same(&provenance["request"], &parent["request"])? {
        issues.push((
            "artifact_mismatch",
            position,
            "the artifact was built from a different request than the parent's".into(),
        ));
    } else if !same(&provenance["replay"], &approved["replay"])? {
        issues.push((
            "artifact_mismatch",
            position,
            "approved.replay is not the artifact's provenance.replay".into(),
        ));
    }
    Ok(())
}

// ------------------------------------------------------------------------- cut-to-measure

const SIDES: [&str; 3] = ["length", "width", "height"];

/// `request` with every made-to-measure box that holds a fixed placement made a stock container
///. The box was cut for the approved plan, so a replan cannot recut it: instance `n`
/// of made-to-measure type `T` -- the `n`-th container of type `T` in the artifact, in result
/// order -- becomes container `T@<revision>#<n>`, a copy of `T` without `made_to_measure`,
/// quantity 1, at the cut inner and outer size, price and tare, each in ticks; it follows `T`
/// in the container list. Its fixed placements move to its instance 1, and `T`'s own
/// `quantity`, if stated, loses the boxes taken (`T` is dropped at zero). New boxes are still
/// cut to measure. Pure; `O(C + F + A)` for containers, fixed placements and the artifact's
/// containers, beside the copy.
pub(crate) fn stock_cut_boxes(
    request: &Value,
    artifact: &Value,
    revision: i64,
) -> RevisionResult<Value> {
    let mut derived = request.clone();
    let (Some(fixed), Some(containers)) = (
        derived["fixed_placements"].as_array(),
        derived["containers"].as_array(),
    ) else {
        return Ok(derived);
    };
    let machines: HashMap<&str, &Value> = containers
        .iter()
        .filter(|entry| !entry["made_to_measure"].is_null())
        .filter_map(|entry| Some((entry["id"].as_str()?, entry)))
        .collect();
    // Maps are lookup-only; request/lock order, never hash iteration, orders the output.
    let mut named = HashSet::new();
    let mut stocked: HashMap<&str, Vec<Value>> = HashMap::new();
    let mut cuts = None;
    let mut moved = Vec::with_capacity(fixed.len());
    for entry in fixed {
        let mut entry = entry.clone();
        if let Some(machine_id) = entry
            .as_object()
            .and_then(|map| map.get("container_type"))
            .and_then(Value::as_str)
            .filter(|id| machines.contains_key(id))
            .map(str::to_owned)
        {
            let instance = entry.get("container_instance").map_or(Some(1), integer);
            let stock_id = format!(
                "{machine_id}@{revision}#{}",
                instance.map_or_else(|| "None".to_owned(), |n| n.to_string())
            );
            if named.insert(stock_id.clone()) {
                let inner = cut_inner(
                    cuts.get_or_insert_with(|| cut_boxes(artifact)),
                    &machine_id,
                    instance,
                )?;
                let (machine_key, machine) = machines
                    .get_key_value(machine_id.as_str())
                    .expect("a machine was found above");
                let box_ = stock_box(request, machine, &stock_id, inner)?;
                stocked.entry(machine_key).or_default().push(box_);
            }
            entry["container_type"] = Value::String(stock_id);
            entry["container_instance"] = json!(1);
            // A stated id named the cut box's instance of the machine, which this request no
            // longer has; admission would refuse it as a mismatch.
            if let Some(object) = entry.as_object_mut() {
                object.remove("placement_id");
            }
        }
        moved.push(entry);
    }
    if named.is_empty() {
        return Ok(derived);
    }
    let existing: HashSet<&str> = containers
        .iter()
        .filter_map(|container| container["id"].as_str())
        .collect();
    if let Some(clash) = named
        .iter()
        .map(String::as_str)
        .filter(|id| existing.contains(id))
        .min()
    {
        return refuse(
            RevisionErrorCode::EventConflict,
            format!(
                "a container is already named {}",
                json_spelling(&Value::String(clash.to_owned()))
            ),
        );
    }
    let mut rebuilt = Vec::with_capacity(containers.len() + named.len());
    for container in containers {
        let boxes = container["id"].as_str().and_then(|id| stocked.get(id));
        let Some(boxes) = boxes.filter(|boxes| !boxes.is_empty()) else {
            rebuilt.push(container.clone());
            continue;
        };
        let taken = boxes.len() as i64;
        match container
            .get("quantity")
            .filter(|quantity| !quantity.is_null())
            .and_then(integer)
        {
            Some(quantity) if quantity <= taken => {}
            Some(quantity) => {
                let mut kept = container.clone();
                kept["quantity"] = json!(quantity - taken);
                rebuilt.push(kept);
            }
            None => rebuilt.push(container.clone()),
        }
        rebuilt.extend(boxes.iter().cloned());
    }
    derived["fixed_placements"] = Value::Array(moved);
    derived["containers"] = Value::Array(rebuilt);
    Ok(derived)
}

/// Index raw shapes in artifact order; only selected dimensions are parsed. `O(A)`.
fn cut_boxes(artifact: &Value) -> HashMap<&str, Vec<&Value>> {
    let mut cuts: HashMap<&str, Vec<&Value>> = HashMap::new();
    if let (Some(orders), Some(shapes)) = (
        artifact["work_order"]["containers"].as_array(),
        artifact["geometry"]["containers"].as_array(),
    ) {
        for (order, shape) in orders.iter().zip(shapes) {
            if let Some(machine_id) = order["container_type"].as_str() {
                cuts.entry(machine_id).or_default().push(shape);
            }
        }
    }
    cuts
}

/// The selected box's ticks, read only when it is stocked. Expected `O(1)`.
fn cut_inner(
    cuts: &HashMap<&str, Vec<&Value>>,
    machine_id: &str,
    instance: Option<i64>,
) -> RevisionResult<[i64; 3]> {
    if let Some(instance) = instance
        && instance >= 1
        && let Some(shape) = cuts
            .get(machine_id)
            .and_then(|shapes| shapes.get(usize::try_from(instance - 1).unwrap_or(usize::MAX)))
    {
        let mut sides = [0_i64; 3];
        for (axis, side) in SIDES.into_iter().enumerate() {
            sides[axis] = artifact_ticks(shape, side)?;
        }
        return Ok(sides);
    }
    refuse(
        RevisionErrorCode::InvalidArtifact,
        format!(
            "the approved plan holds no {machine_id} instance {}",
            instance.map_or_else(|| "null".to_owned(), |n| n.to_string())
        ),
    )
}

fn artifact_ticks(shape: &Value, axis: &str) -> RevisionResult<i64> {
    shape["inner_dimensions"][axis]
        .as_str()
        .filter(|text| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|text| text.parse::<i64>().ok())
        .filter(|ticks| *ticks <= canonical_json::MAX_EXACT_MAGNITUDE as i64)
        .map_or_else(
            || {
                refuse(
                    RevisionErrorCode::InvalidArtifact,
                    format!("a cut box's {axis} is not a tick count"),
                )
            },
            Ok,
        )
}

/// The stock container a cut box becomes, priced as `Container::fabricate` prices it.
fn stock_box(
    request: &Value,
    machine: &Value,
    stock_id: &str,
    inner: [i64; 3],
) -> RevisionResult<Value> {
    let units = &request["units"];
    if !units.is_null() && !units.is_object() {
        return refuse(
            RevisionErrorCode::InvalidRevision,
            "a made-to-measure container's units is not an object",
        );
    }
    let unit = match &units["length"] {
        Value::Null => "mm",
        Value::String(unit) => unit,
        _ => {
            return refuse(
                RevisionErrorCode::InvalidRevision,
                "a made-to-measure container's units.length is not a string",
            );
        }
    };
    let spec = &machine["made_to_measure"];
    if !spec.is_object() {
        return refuse(
            RevisionErrorCode::InvalidRevision,
            "a made-to-measure container's made_to_measure is not an object",
        );
    }
    let allowance = &spec["outer_allowance"];
    if !allowance.is_null() && !allowance.is_object() {
        return refuse(
            RevisionErrorCode::InvalidRevision,
            "a made-to-measure container's outer_allowance is not an object",
        );
    }
    let unreadable = |field: &str| {
        RevisionError::new(
            RevisionErrorCode::InvalidRevision,
            format!("a made-to-measure container's {field} is not a measure"),
        )
    };
    let length = |value: &Value, field: &str| {
        Length::parse(value, unit)
            .map(|length| length.0)
            .map_err(|_| unreadable(field))
    };
    let weight = |value: &Value, field: &str| -> RevisionResult<i128> {
        if value.is_null() {
            return Ok(0);
        }
        Weight::parse(value, "g")
            .map(|weight| i128::from(weight.0))
            .map_err(|_| unreadable(field))
    };
    let mut outer = inner;
    for (axis, side) in SIDES.into_iter().enumerate() {
        let side_allowance = &allowance[side];
        if !side_allowance.is_null() {
            outer[axis] += length(side_allowance, "outer_allowance")?;
        }
    }
    let area = surface_area(inner);
    let price = i128::from(stock_integer(
        &spec["cost_per_square_metre_minor"],
        "cost_per_square_metre_minor",
    )?);
    let tare = weight(&machine["tare_weight"], "tare_weight")?
        + per_square_metre(
            weight(&spec["tare_per_square_metre"], "tare_per_square_metre")?,
            area,
        );
    let cost = i128::from(stock_integer(&machine["cost_minor"], "cost_minor")?)
        + per_square_metre(price, area);
    let mut box_ = machine.clone();
    let fields = box_
        .as_object_mut()
        .expect("a machine is a container object");
    fields.remove("made_to_measure");
    fields.insert("id".into(), Value::String(stock_id.to_owned()));
    fields.insert("quantity".into(), json!(1));
    fields.insert("inner_dimensions".into(), tick_box(inner));
    fields.insert("outer_dimensions".into(), tick_box(outer));
    // Past `i64` the price is past 2^53 - 1 too, so the canonical form refuses it either way.
    fields.insert(
        "cost_minor".into(),
        json!(i64::try_from(cost).unwrap_or(i64::MAX)),
    );
    fields.insert(
        "tare_weight".into(),
        json!({"value": tare.to_string(), "unit": "ticks"}),
    );
    Ok(box_)
}

fn stock_integer(value: &Value, name: &str) -> RevisionResult<i64> {
    if value.is_null() {
        return Ok(0);
    }
    integer(value).ok_or_else(|| {
        RevisionError::new(
            RevisionErrorCode::InvalidRevision,
            format!("a made-to-measure container's {name} is not an integer"),
        )
    })
}

fn tick_box(sides: [i64; 3]) -> Value {
    Value::Object(
        SIDES
            .into_iter()
            .zip(sides)
            .map(|(axis, side)| {
                (
                    axis.to_owned(),
                    json!({"value": side.to_string(), "unit": "ticks"}),
                )
            })
            .collect(),
    )
}

/// Whether a fixed placement names a made-to-measure container type.
fn holds_cut_boxes(request: &Value) -> bool {
    let (Some(containers), Some(fixed)) = (
        request["containers"].as_array(),
        request["fixed_placements"].as_array(),
    ) else {
        return false;
    };
    let machines: Vec<&Value> = containers
        .iter()
        .filter(|entry| entry.is_object() && !entry["made_to_measure"].is_null())
        .map(|entry| &entry["id"])
        .collect();
    fixed
        .iter()
        .any(|entry| entry.is_object() && machines.iter().any(|id| **id == entry["container_type"]))
}

// -------------------------------------------------------------------------------- values

/// Equality as the canonical form sees it, so `1.0` and `1` are one value, as in every engine.
fn same(left: &Value, right: &Value) -> RevisionResult<bool> {
    Ok(canonical(left)? == canonical(right)?)
}

/// A JSON integer judged by value: `1.0` is one, `true` and `1.5` are not.
fn integer(value: &Value) -> Option<i64> {
    json_integer(value)
}

/// A digest as itself, and anything a tampered chain puts in its place as JSON.
fn plain(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| json_spelling(value), str::to_owned)
}
