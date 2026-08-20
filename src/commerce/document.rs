//! Parse the canonical commerce document and the three request shapes.
//!
//! Shape validation only: nothing here computes a price, a decision or a version
//! resolution. Parsing is strict in both directions -- a missing required key and an
//! unrecognised extra key are both input errors, because a field the contract does not
//! define must never be silently ignored.
//!
//! Complexity: one pass, `O(size of the document)` in time and space.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::catalog::{
    CartonMaster, CatalogRegistry, Catalogs, ExclusionRule, ExclusionScope, FacilityOverride,
    ItemMaster, OverrideEntry, PalletMaster, Snapshot,
};
use super::policy::{PolicyAction, PolicyOperator, PolicyRegistry, PolicyScope, Predicate, Rule};
use super::rating::{AccessorialCharge, CarrierRegistry, Tariff};

/// The supplied document or request is not well formed -- a caller bug, kept apart from
/// a rejection the commercial model is entitled to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommerceInputError {
    pub path: String,
    pub message: String,
}

impl std::fmt::Display for CommerceInputError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.path, self.message)
    }
}

impl std::error::Error for CommerceInputError {}

pub type Parsed<T> = Result<T, CommerceInputError>;

pub fn fail<T>(path: &str, message: impl Into<String>) -> Parsed<T> {
    Err(CommerceInputError {
        path: path.to_owned(),
        message: message.into(),
    })
}

pub fn object<'a>(value: &'a Value, path: &str) -> Parsed<&'a Map<String, Value>> {
    value
        .as_object()
        .map_or_else(|| fail(path, "expected an object"), Ok)
}

pub fn list<'a>(value: &'a Value, path: &str) -> Parsed<&'a Vec<Value>> {
    value
        .as_array()
        .map_or_else(|| fail(path, "expected a list"), Ok)
}

pub fn integer(value: &Value, path: &str) -> Parsed<i64> {
    // A JSON boolean where an exact integer belongs is a caller mistake, not a 0 or 1.
    match value {
        Value::Number(number) => number
            .as_i64()
            .map_or_else(|| fail(path, "expected an exact integer"), Ok),
        _ => fail(path, "expected an exact integer"),
    }
}

pub fn text(value: &Value, path: &str) -> Parsed<String> {
    value.as_str().map_or_else(
        || fail(path, "expected a string"),
        |found| Ok(found.to_owned()),
    )
}

pub fn keys(
    value: &Map<String, Value>,
    path: &str,
    required: &[&str],
    optional: &[&str],
) -> Parsed<()> {
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|key| !value.contains_key(*key))
        .collect();
    if !missing.is_empty() {
        return fail(path, format!("missing required key(s) {missing:?}"));
    }
    let unknown: Vec<&str> = value
        .keys()
        .map(String::as_str)
        .filter(|key| !required.contains(key) && !optional.contains(key))
        .collect();
    if !unknown.is_empty() {
        return fail(path, format!("unrecognised key(s) {unknown:?}"));
    }
    Ok(())
}

/// Required-or-default lookup: an explicit JSON `null` reads as absent, matching how the
/// other implementations treat an omitted optional field.
pub fn optional<'a>(value: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    match value.get(key) {
        None | Some(Value::Null) => None,
        Some(found) => Some(found),
    }
}

pub fn axes<const N: usize>(value: &Value, path: &str) -> Parsed<[i64; N]> {
    let entries = list(value, path)?;
    if entries.len() != N {
        return fail(path, format!("expected exactly {N} axes"));
    }
    let mut parsed = [0i64; N];
    for (index, entry) in entries.iter().enumerate() {
        parsed[index] = integer(entry, &format!("{path}[{index}]"))?;
    }
    Ok(parsed)
}

/// The three append-only histories a request is answered against.
#[derive(Debug, Default)]
pub struct Document {
    pub carriers: CarrierRegistry,
    pub policies: PolicyRegistry,
    pub catalogs: Catalogs,
}

pub fn load(value: &Value) -> Parsed<Document> {
    let root = object(value, "document")?;
    keys(
        root,
        "document",
        &[],
        &["tariffs", "policy_rules", "catalogs"],
    )?;
    Ok(Document {
        carriers: load_tariffs(root.get("tariffs"))?,
        policies: load_policy_rules(root.get("policy_rules"))?,
        catalogs: load_catalogs(root.get("catalogs"))?,
    })
}

/// An absent or explicitly-null optional list reads as empty rather than as an error,
/// so a document only needs to spell out the collections it actually has.
fn entries<'a>(value: Option<&'a Value>, path: &str) -> Parsed<&'a [Value]> {
    match value {
        None | Some(Value::Null) => Ok(&[]),
        Some(found) => Ok(list(found, path)?.as_slice()),
    }
}

// ------------------------------------------------------------------------------ tariffs

fn load_tariffs(value: Option<&Value>) -> Parsed<CarrierRegistry> {
    let mut registry = CarrierRegistry::new();
    for (index, entry) in entries(value, "document.tariffs")?.iter().enumerate() {
        let path = format!("document.tariffs[{index}]");
        let fields = object(entry, &path)?;
        keys(
            fields,
            &path,
            &["carrier_id", "service_id", "versions"],
            &[],
        )?;
        let carrier_id = text(&fields["carrier_id"], &format!("{path}.carrier_id"))?;
        let service_id = text(&fields["service_id"], &format!("{path}.service_id"))?;
        if registry.has(&carrier_id, &service_id) {
            return fail(
                &path,
                format!("duplicate tariff history for {carrier_id}/{service_id}"),
            );
        }
        let versions = list(&fields["versions"], &format!("{path}.versions"))?;
        if versions.is_empty() {
            return fail(
                &format!("{path}.versions"),
                "a tariff history needs at least one version",
            );
        }
        for (number, version) in versions.iter().enumerate() {
            let path = format!("{path}.versions[{number}]");
            registry.publish(parse_tariff(version, &path, &carrier_id, &service_id)?);
        }
    }
    Ok(registry)
}

fn parse_tariff(value: &Value, path: &str, carrier_id: &str, service_id: &str) -> Parsed<Tariff> {
    let fields = object(value, path)?;
    keys(
        fields,
        path,
        &[
            "effective_at",
            "dimensional_weight_divisor",
            "cost_per_dimensional_kg_minor",
        ],
        &[
            "minimum_charge_minor",
            "fuel_surcharge_permille",
            "accessorials",
        ],
    )?;
    let zones_path = format!("{path}.cost_per_dimensional_kg_minor");
    let mut cost_per_dimensional_kg_minor = BTreeMap::new();
    for (zone, cost) in object(&fields["cost_per_dimensional_kg_minor"], &zones_path)? {
        let cost = integer(cost, &format!("{zones_path}[{zone}]"))?;
        if cost < 0 {
            return fail(
                &zones_path,
                "cost_per_dimensional_kg_minor entries cannot be negative",
            );
        }
        cost_per_dimensional_kg_minor.insert(zone.clone(), cost);
    }
    let effective_at = integer(&fields["effective_at"], &format!("{path}.effective_at"))?;
    let dimensional_weight_divisor = integer(
        &fields["dimensional_weight_divisor"],
        &format!("{path}.dimensional_weight_divisor"),
    )?;
    let minimum_charge_minor = optional_integer(fields, "minimum_charge_minor", path, 0)?;
    let fuel_surcharge_permille = optional_integer(fields, "fuel_surcharge_permille", path, 0)?;
    if effective_at < 0 {
        return fail(path, "effective_at cannot be negative");
    }
    if dimensional_weight_divisor <= 0 {
        return fail(path, "dimensional_weight_divisor must be positive");
    }
    if minimum_charge_minor < 0 || fuel_surcharge_permille < 0 {
        return fail(
            path,
            "minimum_charge_minor and fuel_surcharge_permille cannot be negative",
        );
    }
    Ok(Tariff {
        carrier_id: carrier_id.to_owned(),
        service_id: service_id.to_owned(),
        version: 0, // assigned by CarrierRegistry::publish from its position in the history
        effective_at,
        dimensional_weight_divisor,
        cost_per_dimensional_kg_minor,
        minimum_charge_minor,
        fuel_surcharge_permille,
        accessorials: parse_accessorials(optional(fields, "accessorials"), path)?,
    })
}

fn parse_accessorials(
    value: Option<&Value>,
    parent: &str,
) -> Parsed<BTreeMap<String, AccessorialCharge>> {
    let path = format!("{parent}.accessorials");
    let mut charges = BTreeMap::new();
    for (index, entry) in entries(value, &path)?.iter().enumerate() {
        let path = format!("{path}[{index}]");
        let fields = object(entry, &path)?;
        keys(
            fields,
            &path,
            &["accessorial_id"],
            &["flat_charge_minor", "permille_of_base"],
        )?;
        let accessorial_id = text(&fields["accessorial_id"], &format!("{path}.accessorial_id"))?;
        if charges.contains_key(&accessorial_id) {
            return fail(
                &path,
                format!("duplicate accessorial_id '{accessorial_id}'"),
            );
        }
        let flat = optional(fields, "flat_charge_minor")
            .map(|found| integer(found, &format!("{path}.flat_charge_minor")))
            .transpose()?;
        let permille = optional(fields, "permille_of_base")
            .map(|found| integer(found, &format!("{path}.permille_of_base")))
            .transpose()?;
        if flat.is_some() == permille.is_some() {
            return fail(
                &path,
                "an accessorial must set exactly one of flat_charge_minor or permille_of_base",
            );
        }
        if flat.unwrap_or(0) < 0 || permille.unwrap_or(0) < 0 {
            return fail(&path, "an accessorial charge cannot be negative");
        }
        charges.insert(
            accessorial_id.clone(),
            AccessorialCharge {
                accessorial_id,
                flat_charge_minor: flat,
                permille_of_base: permille,
            },
        );
    }
    Ok(charges)
}

fn optional_integer(
    fields: &Map<String, Value>,
    key: &str,
    parent: &str,
    default: i64,
) -> Parsed<i64> {
    match optional(fields, key) {
        None => Ok(default),
        Some(found) => integer(found, &format!("{parent}.{key}")),
    }
}

// ------------------------------------------------------------------------- policy rules

fn load_policy_rules(value: Option<&Value>) -> Parsed<PolicyRegistry> {
    let mut registry = PolicyRegistry::new();
    let mut seen: Vec<String> = Vec::new();
    for (index, entry) in entries(value, "document.policy_rules")?.iter().enumerate() {
        let path = format!("document.policy_rules[{index}]");
        let fields = object(entry, &path)?;
        keys(fields, &path, &["rule_id", "versions"], &[])?;
        let rule_id = text(&fields["rule_id"], &format!("{path}.rule_id"))?;
        if seen.contains(&rule_id) {
            return fail(&path, format!("duplicate rule history for '{rule_id}'"));
        }
        seen.push(rule_id.clone());
        let versions = list(&fields["versions"], &format!("{path}.versions"))?;
        if versions.is_empty() {
            return fail(
                &format!("{path}.versions"),
                "a rule history needs at least one version",
            );
        }
        for (number, version) in versions.iter().enumerate() {
            let path = format!("{path}.versions[{number}]");
            registry.publish(parse_rule(version, &path, &rule_id)?);
        }
    }
    Ok(registry)
}

fn parse_rule(value: &Value, path: &str, rule_id: &str) -> Parsed<Rule> {
    let fields = object(value, path)?;
    keys(
        fields,
        path,
        &["scope", "action", "predicates", "priority", "effective_at"],
        &["reason"],
    )?;
    let scope_text = text(&fields["scope"], &format!("{path}.scope"))?;
    let scope = PolicyScope::parse(&scope_text).ok_or_else(|| CommerceInputError {
        path: format!("{path}.scope"),
        message: format!("unsupported policy scope '{scope_text}'"),
    })?;
    let action_text = text(&fields["action"], &format!("{path}.action"))?;
    let action = PolicyAction::parse(&action_text).ok_or_else(|| CommerceInputError {
        path: format!("{path}.action"),
        message: format!("unsupported policy action '{action_text}'"),
    })?;
    let predicates = parse_predicates(&fields["predicates"], &format!("{path}.predicates"), scope)?;
    if predicates.is_empty() {
        return fail(path, "a rule must have at least one predicate");
    }
    let effective_at = integer(&fields["effective_at"], &format!("{path}.effective_at"))?;
    if effective_at < 0 {
        return fail(path, "effective_at cannot be negative");
    }
    Ok(Rule {
        rule_id: rule_id.to_owned(),
        version: 0, // assigned by PolicyRegistry::publish from its position in the history
        scope,
        action,
        predicates,
        priority: integer(&fields["priority"], &format!("{path}.priority"))?,
        effective_at,
        reason: match optional(fields, "reason") {
            None => String::new(),
            Some(found) => text(found, &format!("{path}.reason"))?,
        },
    })
}

fn parse_predicates(value: &Value, path: &str, scope: PolicyScope) -> Parsed<Vec<Predicate>> {
    let mut predicates = Vec::new();
    for (index, entry) in list(value, path)?.iter().enumerate() {
        let path = format!("{path}[{index}]");
        let fields = object(entry, &path)?;
        keys(fields, &path, &["scope", "field", "operator"], &["value"])?;
        let scope_text = text(&fields["scope"], &format!("{path}.scope"))?;
        let predicate_scope =
            PolicyScope::parse(&scope_text).ok_or_else(|| CommerceInputError {
                path: format!("{path}.scope"),
                message: format!("unsupported policy scope '{scope_text}'"),
            })?;
        if predicate_scope != scope {
            return fail(
                &path,
                "every predicate of a rule must share the rule's own scope",
            );
        }
        let operator_text = text(&fields["operator"], &format!("{path}.operator"))?;
        let operator = PolicyOperator::parse(&operator_text).ok_or_else(|| CommerceInputError {
            path: format!("{path}.operator"),
            message: format!("unsupported policy operator '{operator_text}'"),
        })?;
        let value = optional(fields, "value").cloned().unwrap_or(Value::Null);
        if !operator.is_unary() && value.is_null() {
            return fail(
                &path,
                format!("operator '{operator_text}' requires a value"),
            );
        }
        let field = text(&fields["field"], &format!("{path}.field"))?;
        if field.is_empty() {
            return fail(&path, "field is required");
        }
        predicates.push(Predicate {
            scope: predicate_scope,
            field,
            operator,
            value,
        });
    }
    Ok(predicates)
}

// ----------------------------------------------------------------------------- catalogs

fn load_catalogs(value: Option<&Value>) -> Parsed<Catalogs> {
    let mut catalogs = Catalogs::default();
    for (index, entry) in entries(value, "document.catalogs")?.iter().enumerate() {
        let path = format!("document.catalogs[{index}]");
        let fields = object(entry, &path)?;
        keys(fields, &path, &["catalog_id", "versions"], &[])?;
        let catalog_id = text(&fields["catalog_id"], &format!("{path}.catalog_id"))?;
        if catalogs.contains(&catalog_id) {
            return fail(
                &path,
                format!("duplicate catalog history for '{catalog_id}'"),
            );
        }
        if catalog_id.is_empty() {
            return fail(&path, "catalog_id is required");
        }
        let versions = list(&fields["versions"], &format!("{path}.versions"))?;
        if versions.is_empty() {
            return fail(
                &format!("{path}.versions"),
                "a catalog history needs at least one version",
            );
        }
        let mut registry = CatalogRegistry::new(catalog_id.clone());
        for (number, version) in versions.iter().enumerate() {
            publish_catalog_version(
                &mut registry,
                version,
                &format!("{path}.versions[{number}]"),
            )?;
        }
        catalogs.insert(registry);
    }
    Ok(catalogs)
}

fn publish_catalog_version(
    registry: &mut CatalogRegistry,
    value: &Value,
    path: &str,
) -> Parsed<()> {
    let fields = object(value, path)?;
    if fields.contains_key("rollback_to") {
        keys(
            fields,
            path,
            &["rollback_to", "published_at"],
            &["effective_at", "note"],
        )?;
        let to_version = integer(&fields["rollback_to"], &format!("{path}.rollback_to"))?;
        let published_at = integer(&fields["published_at"], &format!("{path}.published_at"))?;
        let effective_at = optional(fields, "effective_at")
            .map(|found| integer(found, &format!("{path}.effective_at")))
            .transpose()?;
        let note = match optional(fields, "note") {
            None => String::new(),
            Some(found) => text(found, &format!("{path}.note"))?,
        };
        return registry
            .rollback(to_version, published_at, effective_at, note)
            .map_err(|_| CommerceInputError {
                path: path.to_owned(),
                message: format!(
                    "rollback_to names version {to_version}, which is not published yet"
                ),
            });
    }
    keys(
        fields,
        path,
        &["effective_at", "published_at", "snapshot"],
        &["note"],
    )?;
    let effective_at = integer(&fields["effective_at"], &format!("{path}.effective_at"))?;
    let published_at = integer(&fields["published_at"], &format!("{path}.published_at"))?;
    if effective_at < 0 || published_at < 0 {
        return fail(path, "effective_at and published_at cannot be negative");
    }
    let note = match optional(fields, "note") {
        None => String::new(),
        Some(found) => text(found, &format!("{path}.note"))?,
    };
    registry.publish(
        parse_snapshot(&fields["snapshot"], &format!("{path}.snapshot"))?,
        effective_at,
        published_at,
        note,
    );
    Ok(())
}

fn parse_snapshot(value: &Value, path: &str) -> Parsed<Snapshot> {
    let fields = object(value, path)?;
    keys(
        fields,
        path,
        &[],
        &["items", "cartons", "pallets", "exclusions", "overrides"],
    )?;
    let mut snapshot = Snapshot::default();
    for (index, entry) in entries(fields.get("items"), &format!("{path}.items"))?
        .iter()
        .enumerate()
    {
        snapshot
            .items
            .push(parse_item(entry, &format!("{path}.items[{index}]"))?);
    }
    for (index, entry) in entries(fields.get("cartons"), &format!("{path}.cartons"))?
        .iter()
        .enumerate()
    {
        snapshot
            .cartons
            .push(parse_carton(entry, &format!("{path}.cartons[{index}]"))?);
    }
    for (index, entry) in entries(fields.get("pallets"), &format!("{path}.pallets"))?
        .iter()
        .enumerate()
    {
        snapshot
            .pallets
            .push(parse_pallet(entry, &format!("{path}.pallets[{index}]"))?);
    }
    for (index, entry) in entries(fields.get("exclusions"), &format!("{path}.exclusions"))?
        .iter()
        .enumerate()
    {
        snapshot.exclusions.push(parse_exclusion(
            entry,
            &format!("{path}.exclusions[{index}]"),
        )?);
    }
    for (index, entry) in entries(fields.get("overrides"), &format!("{path}.overrides"))?
        .iter()
        .enumerate()
    {
        snapshot.overrides.push(parse_override(
            entry,
            &format!("{path}.overrides[{index}]"),
        )?);
    }
    require_unique(
        snapshot.items.iter().map(|entry| entry.id.as_str()),
        "item",
        path,
    )?;
    require_unique(
        snapshot.cartons.iter().map(|entry| entry.id.as_str()),
        "carton",
        path,
    )?;
    require_unique(
        snapshot.pallets.iter().map(|entry| entry.id.as_str()),
        "pallet",
        path,
    )?;
    require_unique(
        snapshot.exclusions.iter().map(|entry| entry.id.as_str()),
        "exclusion",
        path,
    )?;
    require_unique(
        snapshot.overrides.iter().map(|entry| entry.id.as_str()),
        "facility override",
        path,
    )?;
    Ok(snapshot)
}

fn require_unique<'a>(ids: impl Iterator<Item = &'a str>, label: &str, path: &str) -> Parsed<()> {
    let mut collected: Vec<&str> = ids.collect();
    let total = collected.len();
    collected.sort_unstable();
    collected.dedup();
    if collected.len() != total {
        return fail(path, format!("duplicate {label} ids in catalog snapshot"));
    }
    Ok(())
}

fn parse_item(value: &Value, path: &str) -> Parsed<ItemMaster> {
    let fields = object(value, path)?;
    keys(
        fields,
        path,
        &["id", "dimensions_mm", "weight_g"],
        &["description"],
    )?;
    let dimensions_mm: [i64; 3] = axes(&fields["dimensions_mm"], &format!("{path}.dimensions_mm"))?;
    let weight_g = integer(&fields["weight_g"], &format!("{path}.weight_g"))?;
    if dimensions_mm.iter().any(|axis| *axis <= 0) {
        return fail(path, "item dimensions must be positive");
    }
    if weight_g <= 0 {
        return fail(path, "item weight must be positive");
    }
    Ok(ItemMaster {
        id: identifier(fields, path, "item")?,
        dimensions_mm,
        weight_g,
        description: match optional(fields, "description") {
            None => String::new(),
            Some(found) => text(found, &format!("{path}.description"))?,
        },
    })
}

fn parse_carton(value: &Value, path: &str) -> Parsed<CartonMaster> {
    let fields = object(value, path)?;
    keys(
        fields,
        path,
        &["id", "inner_dimensions_mm", "max_payload_g"],
        &["cost_minor"],
    )?;
    let inner_dimensions_mm: [i64; 3] = axes(
        &fields["inner_dimensions_mm"],
        &format!("{path}.inner_dimensions_mm"),
    )?;
    let max_payload_g = integer(&fields["max_payload_g"], &format!("{path}.max_payload_g"))?;
    let cost_minor = optional_integer(fields, "cost_minor", path, 0)?;
    if inner_dimensions_mm.iter().any(|axis| *axis <= 0) {
        return fail(path, "carton dimensions must be positive");
    }
    if max_payload_g <= 0 {
        return fail(path, "carton max_payload_g must be positive");
    }
    if cost_minor < 0 {
        return fail(path, "cost_minor cannot be negative");
    }
    Ok(CartonMaster {
        id: identifier(fields, path, "carton")?,
        inner_dimensions_mm,
        max_payload_g,
        cost_minor,
    })
}

fn parse_pallet(value: &Value, path: &str) -> Parsed<PalletMaster> {
    let fields = object(value, path)?;
    keys(
        fields,
        path,
        &["id", "deck_dimensions_mm", "max_payload_g"],
        &["max_stack_height_mm"],
    )?;
    let deck_dimensions_mm: [i64; 2] = axes(
        &fields["deck_dimensions_mm"],
        &format!("{path}.deck_dimensions_mm"),
    )?;
    let max_payload_g = integer(&fields["max_payload_g"], &format!("{path}.max_payload_g"))?;
    let max_stack_height_mm = optional(fields, "max_stack_height_mm")
        .map(|found| integer(found, &format!("{path}.max_stack_height_mm")))
        .transpose()?;
    if deck_dimensions_mm.iter().any(|axis| *axis <= 0) {
        return fail(path, "pallet dimensions must be positive");
    }
    if max_payload_g <= 0 {
        return fail(path, "pallet max_payload_g must be positive");
    }
    if max_stack_height_mm.is_some_and(|height| height <= 0) {
        return fail(path, "max_stack_height_mm must be positive");
    }
    Ok(PalletMaster {
        id: identifier(fields, path, "pallet")?,
        deck_dimensions_mm,
        max_payload_g,
        max_stack_height_mm,
    })
}

fn parse_exclusion(value: &Value, path: &str) -> Parsed<ExclusionRule> {
    let fields = object(value, path)?;
    keys(
        fields,
        path,
        &["id", "scope", "subject_id", "excluded_id"],
        &["reason"],
    )?;
    let scope_text = text(&fields["scope"], &format!("{path}.scope"))?;
    let scope = ExclusionScope::parse(&scope_text).ok_or_else(|| CommerceInputError {
        path: format!("{path}.scope"),
        message: format!("unsupported exclusion scope '{scope_text}'"),
    })?;
    let subject_id = text(&fields["subject_id"], &format!("{path}.subject_id"))?;
    let excluded_id = text(&fields["excluded_id"], &format!("{path}.excluded_id"))?;
    if subject_id.is_empty() || excluded_id.is_empty() {
        return fail(
            path,
            "an exclusion rule must reference both a subject and an excluded id",
        );
    }
    Ok(ExclusionRule {
        id: identifier(fields, path, "exclusion")?,
        scope,
        subject_id,
        excluded_id,
        reason: match optional(fields, "reason") {
            None => String::new(),
            Some(found) => text(found, &format!("{path}.reason"))?,
        },
    })
}

fn parse_override(value: &Value, path: &str) -> Parsed<FacilityOverride> {
    let fields = object(value, path)?;
    keys(
        fields,
        path,
        &["id", "facility_id", "entry_id", "kind", "override"],
        &[],
    )?;
    let kind = text(&fields["kind"], &format!("{path}.kind"))?;
    let override_path = format!("{path}.override");
    let entry = match kind.as_str() {
        "item" => OverrideEntry::Item(parse_item(&fields["override"], &override_path)?),
        "carton" => OverrideEntry::Carton(parse_carton(&fields["override"], &override_path)?),
        "pallet" => OverrideEntry::Pallet(parse_pallet(&fields["override"], &override_path)?),
        _ => {
            return fail(
                &format!("{path}.kind"),
                "expected one of [\"carton\", \"item\", \"pallet\"]",
            );
        }
    };
    let facility_id = text(&fields["facility_id"], &format!("{path}.facility_id"))?;
    let entry_id = text(&fields["entry_id"], &format!("{path}.entry_id"))?;
    if facility_id.is_empty() {
        return fail(path, "facility_id is required");
    }
    if entry.id() != entry_id {
        return fail(
            path,
            "a facility override's entry_id must match override.id",
        );
    }
    Ok(FacilityOverride {
        id: identifier(fields, path, "facility override")?,
        facility_id,
        entry_id,
        entry,
    })
}

fn identifier(fields: &Map<String, Value>, path: &str, label: &str) -> Parsed<String> {
    let id = text(&fields["id"], &format!("{path}.id"))?;
    if id.is_empty() {
        return fail(path, format!("{label} id is required"));
    }
    Ok(id)
}
