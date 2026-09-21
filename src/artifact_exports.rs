//! JSON, CSV and print-ready work orders from an operational artifact.
//!
//! Held to byte-identical output with `packvium.artifact_exports`. Each export is a pure
//! function of the artifact: no solver, carrier, renderer or clock, so an export is replayable
//! from the artifact it came from.
//!
//! - JSON is the artifact's RFC 8785 canonical form.
//! - CSV has one row per packing step and one per unplaced item. It is RFC 4180: a header row,
//!   CRLF after every row, and a field quoted only when it holds a comma, quote, CR or LF.
//! - The work order is one self-contained HTML document: inline print styles, no script, no
//!   external resource, ASCII source with entities for the few typographic characters.
//!
//! Values are copied, never re-rendered or reinterpreted. That includes a CSV field beginning
//! with `=`: prefixing it would change an identifier a warehouse system matches on, so opening
//! the file in a spreadsheet is the reader's decision (`docs/OPERATIONAL-ARTIFACTS.md`).
//!
//! Every export is O(N) in the artifact's size.

use std::borrow::Cow;
use std::fmt::Write as _;

use serde_json::Value;

use crate::artifacts::{
    ArtifactError, ArtifactErrorCode, FORMAT, canonical_artifact_json, read_document,
};

pub const CSV_COLUMNS: [&str; 18] = [
    "record",
    "container_index",
    "container_type",
    "sequence",
    "item_type",
    "orientation",
    "x_ticks",
    "y_ticks",
    "z_ticks",
    "x",
    "y",
    "z",
    "length",
    "width",
    "height",
    "length_unit",
    "reason",
    "proof_level",
];

const STYLE: &str = concat!(
    "body{font-family:system-ui,sans-serif;margin:24px;color:#111}",
    "h1{font-size:20px}h2{font-size:16px;margin-top:24px}",
    "table{border-collapse:collapse;width:100%;margin:8px 0 16px}",
    "th,td{border:1px solid #999;padding:4px 6px;text-align:left;font-size:12px;vertical-align:top}",
    "th{background:#eee}",
    ".facts td:first-child{width:28%;font-weight:600}",
    "@media print{body{margin:0}section.container{break-after:page}tr{break-inside:avoid}}",
);

const DASH: &str = "&mdash;";

type ExportResult<T> = Result<T, ArtifactError>;

pub fn export_json(artifact_json: &str) -> Result<String, ArtifactError> {
    canonical_artifact_json(&require(artifact_json)?)
}

pub fn export_csv(artifact_json: &str) -> Result<String, ArtifactError> {
    let document = require(artifact_json)?;
    let work_order = key(&document, "work_order")?;
    let length_unit = key(work_order, "length_unit")?;
    let mut out = String::new();
    let mut header = CsvRow::new(&mut out);
    for column in CSV_COLUMNS {
        header.field(column);
    }
    header.end();

    for container in items(key(work_order, "containers")?)? {
        for line in items(key(container, "lines")?)? {
            let reference = key(line, "placement")?;
            let ticks = key(reference, "position_ticks")?;
            let position = key(line, "position")?;
            let dimensions = key(line, "dimensions")?;
            let mut row = CsvRow::new(&mut out);
            row.field("step");
            row.value(key(reference, "container_index")?)?;
            row.value(key(container, "container_type")?)?;
            row.value(line.get("sequence").unwrap_or(&Value::Null))?;
            row.value(key(reference, "item_type")?)?;
            row.value(key(reference, "orientation")?)?;
            for axis in ["x", "y", "z"] {
                row.value(key(ticks, axis)?)?;
            }
            for axis in ["x", "y", "z"] {
                row.value(key(position, axis)?)?;
            }
            for axis in ["length", "width", "height"] {
                row.value(key(dimensions, axis)?)?;
            }
            row.value(length_unit)?;
            row.skip(2);
            row.end();
        }
    }
    for unplaced in items(key(key(&document, "plan")?, "unplaced")?)? {
        let facts = key(unplaced, "facts")?;
        let mut row = CsvRow::new(&mut out);
        row.field("unplaced");
        row.skip(3);
        row.value(key(facts, "item_type")?)?;
        row.skip(11);
        row.value(key(facts, "reason")?)?;
        row.value(key(facts, "proof_level")?)?;
        row.end();
    }
    Ok(out)
}

pub fn export_work_order_html(artifact_json: &str) -> Result<String, ArtifactError> {
    let document = require(artifact_json)?;
    let plan = key(&document, "plan")?;
    let work_order = key(&document, "work_order")?;
    let provenance = key(&document, "provenance")?;
    let mut out = String::new();
    for line in [
        "<!DOCTYPE html>",
        "<html lang=\"en\">",
        "<head>",
        "<meta charset=\"utf-8\">",
        "<title>Packing work order</title>",
        &format!("<style>{STYLE}</style>"),
        "</head>",
        "<body>",
        "<h1>Packing work order</h1>",
        "<table class=\"facts\">",
    ] {
        push_line(&mut out, line);
    }
    fact_rows(&mut out, &document, plan, provenance)?;
    push_line(&mut out, "</table>");
    let plan_containers = items(key(plan, "containers")?)?;
    let containers = items(key(work_order, "containers")?)?;
    for (plan_container, container) in plan_containers.iter().zip(containers) {
        container_section(&mut out, plan_container, container, work_order)?;
    }
    unplaced_section(&mut out, items(key(plan, "unplaced")?)?)?;
    push_line(&mut out, "</body>");
    push_line(&mut out, "</html>");
    Ok(out)
}

// ------------------------------------------------------------------------------------ HTML

fn fact_rows(
    out: &mut String,
    document: &Value,
    plan: &Value,
    provenance: &Value,
) -> ExportResult<()> {
    let facts = key(plan, "facts")?;
    let replay = key(provenance, "replay")?;
    let solver = key(provenance, "solver")?;
    let feasibility = key(facts, "feasibility")?;

    let mut replay_text = escape(key(replay, "level")?)?;
    let because = key(replay, "because")?;
    if truthy(because) {
        let _ = write!(replay_text, " ({})", escape(because)?);
    }
    let solver_text = if truthy(solver) {
        format!(
            "{} / {} / seed {}",
            escape(key(solver, "profile")?)?,
            escape(key(solver, "solver")?)?,
            escape(key(solver, "seed")?)?
        )
    } else {
        DASH.to_string()
    };
    let catalogs = items(key(provenance, "catalog_versions_used")?)?
        .iter()
        .map(catalog_text)
        .collect::<ExportResult<Vec<String>>>()?
        .join(", ");
    let score = items(key(facts, "score")?)?
        .iter()
        .map(escape)
        .collect::<ExportResult<Vec<String>>>()?
        .join(", ");
    let code = feasibility
        .as_object()
        .and_then(|object| object.get("code"))
        .unwrap_or(&Value::Null);

    let rows = [
        ("Status", escape(key(facts, "status")?)?),
        ("Objective", or_dash(key(plan, "objective")?)?),
        ("Containers", escape(key(facts, "container_count")?)?),
        ("Score", format!("[{score}]")),
        ("Feasibility", or_dash(code)?),
        ("Replay", replay_text),
        ("Solver", solver_text),
        (
            "Catalogs",
            if catalogs.is_empty() {
                DASH.to_string()
            } else {
                catalogs
            },
        ),
        ("Packvium", escape(key(document, "suite_version")?)?),
    ];
    for (label, value) in rows {
        push_line(out, &format!("<tr><td>{label}</td><td>{value}</td></tr>"));
    }
    Ok(())
}

fn catalog_text(catalog: &Value) -> ExportResult<String> {
    let Some(object) = catalog.as_object() else {
        return malformed("catalog_versions_used[]");
    };
    let spelled = |name: &str| escape(object.get(name).unwrap_or(&Value::Null));
    Ok(format!(
        "{} v{}",
        spelled("catalog_id")?,
        spelled("version")?
    ))
}

fn container_section(
    out: &mut String,
    plan_container: &Value,
    container: &Value,
    work_order: &Value,
) -> ExportResult<()> {
    let facts = key(plan_container, "facts")?;
    let weight_unit = escape(key(work_order, "weight_unit")?)?;
    let length_unit = escape(key(work_order, "length_unit")?)?;
    push_line(out, "<section class=\"container\">");
    push_line(
        out,
        &format!(
            "<h2>Container {}: {}</h2>",
            ordinal(key(container, "container_index")?)?,
            or_dash(key(container, "container_type")?)?
        ),
    );
    push_line(
        out,
        &format!(
            "<p>Payload {} {weight_unit} &middot; Gross {} {weight_unit} &middot; Utilization {}</p>",
            escape(key(container, "payload_weight")?)?,
            escape(key(container, "gross_weight")?)?,
            or_dash(key(facts, "volume_utilization")?)?
        ),
    );
    if *key(plan_container, "order")? == "loading" {
        push_line(out, "<p>Order: loading. Follow the steps in sequence.</p>");
    } else {
        push_line(
            out,
            "<p>Order: unavailable. No safe loading order was supplied, so the steps are not numbered.</p>",
        );
    }
    push_line(out, "<table>");
    push_line(
        out,
        &format!(
            "<thead><tr><th>Step</th><th>Item</th><th>Orientation</th><th>Position x, y, z ({length_unit})</th>\
             <th>Size l &times; w &times; h ({length_unit})</th><th>Done</th></tr></thead>"
        ),
    );
    push_line(out, "<tbody>");
    for line in items(key(container, "lines")?)? {
        let reference = key(line, "placement")?;
        let position = key(line, "position")?;
        let size = key(line, "dimensions")?;
        push_line(
            out,
            &format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}, {}, {}</td>\
                 <td>{} &times; {} &times; {}</td><td>&#9744;</td></tr>",
                escape(line.get("sequence").unwrap_or(&Value::Null))?,
                escape(key(reference, "item_type")?)?,
                escape(key(reference, "orientation")?)?,
                escape(key(position, "x")?)?,
                escape(key(position, "y")?)?,
                escape(key(position, "z")?)?,
                escape(key(size, "length")?)?,
                escape(key(size, "width")?)?,
                escape(key(size, "height")?)?,
            ),
        );
    }
    for closing in ["</tbody>", "</table>", "</section>"] {
        push_line(out, closing);
    }
    Ok(())
}

fn unplaced_section(out: &mut String, entries: &[Value]) -> ExportResult<()> {
    push_line(out, "<section>");
    push_line(out, "<h2>Not packed</h2>");
    if entries.is_empty() {
        push_line(out, "<p>Every item was packed.</p>");
        push_line(out, "</section>");
        return Ok(());
    }
    push_line(out, "<table>");
    push_line(
        out,
        "<thead><tr><th>Item</th><th>Reason</th><th>Proof</th></tr></thead>",
    );
    push_line(out, "<tbody>");
    for entry in entries {
        let facts = key(entry, "facts")?;
        push_line(
            out,
            &format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td></tr>",
                or_dash(key(facts, "item_type")?)?,
                or_dash(key(facts, "reason")?)?,
                or_dash(key(facts, "proof_level")?)?
            ),
        );
    }
    for closing in ["</tbody>", "</table>", "</section>"] {
        push_line(out, closing);
    }
    Ok(())
}

/// A container's 1-based number on the sheet. Computed, so it is checked like the shared
/// rendering: `0.5 + 1` would print `1.5` in one engine and be refused in another.
fn ordinal(index: &Value) -> ExportResult<String> {
    if let Some(signed) = index.as_i64() {
        return Ok((i128::from(signed) + 1).to_string());
    }
    if let Some(unsigned) = index.as_u64() {
        return Ok((u128::from(unsigned) + 1).to_string());
    }
    Err(ArtifactError::new(
        ArtifactErrorCode::InvalidValue,
        format!("container index {index} is not an integer"),
    ))
}

fn push_line(out: &mut String, line: &str) {
    out.push_str(line);
    out.push('\n');
}

/// The one rendering every engine shares: a string as itself, an integer in decimal, null as
/// nothing. A boolean, a float or a compound value has a different default spelling in each
/// language, so it is refused rather than printed four ways.
fn rendering(value: &Value) -> ExportResult<Cow<'_, str>> {
    match value {
        Value::Null => Ok(Cow::Borrowed("")),
        Value::String(text) => Ok(Cow::Borrowed(text)),
        Value::Number(number) if number.is_i64() || number.is_u64() => {
            Ok(Cow::Owned(number.to_string()))
        }
        other => Err(ArtifactError::new(
            ArtifactErrorCode::InvalidValue,
            format!("{other} has no single rendering in a work order"),
        )),
    }
}

/// The shared rendering with the five HTML-significant characters escaped.
fn escape(value: &Value) -> ExportResult<String> {
    let text = rendering(value)?;
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            other => escaped.push(other),
        }
    }
    Ok(escaped)
}

fn or_dash(value: &Value) -> ExportResult<String> {
    if value.is_null() {
        return Ok(DASH.to_string());
    }
    escape(value)
}

/// Whether the reference's `if value:` takes the branch.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|float| float != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(elements) => !elements.is_empty(),
        Value::Object(object) => !object.is_empty(),
    }
}

// ------------------------------------------------------------------------------------- CSV

/// One RFC 4180 row being written: fields joined by commas, CRLF at the end.
struct CsvRow<'a> {
    out: &'a mut String,
    started: bool,
}

impl<'a> CsvRow<'a> {
    fn new(out: &'a mut String) -> Self {
        Self {
            out,
            started: false,
        }
    }

    fn separate(&mut self) {
        if self.started {
            self.out.push(',');
        }
        self.started = true;
    }

    fn field(&mut self, text: &str) {
        self.separate();
        if text.contains([',', '"', '\r', '\n']) {
            self.out.push('"');
            self.out.push_str(&text.replace('"', "\"\""));
            self.out.push('"');
        } else {
            self.out.push_str(text);
        }
    }

    /// A copied value in the shared rendering; null is an empty field.
    fn value(&mut self, value: &Value) -> ExportResult<()> {
        let text = rendering(value)?;
        self.field(&text);
        Ok(())
    }

    fn skip(&mut self, count: usize) {
        for _ in 0..count {
            self.separate();
        }
    }

    fn end(self) {
        self.out.push_str("\r\n");
    }
}

// --------------------------------------------------------------------------------- reading

/// An export reads only a document it knows how to read, and says so when it cannot.
fn require(artifact_json: &str) -> ExportResult<Value> {
    let document = read_document(artifact_json)?;
    let found = document.get("format").unwrap_or(&Value::Null);
    if *found != FORMAT {
        return Err(ArtifactError::new(
            ArtifactErrorCode::UnknownFormat,
            format!("cannot export format {found}; this exporter reads {FORMAT}"),
        ));
    }
    Ok(document)
}

/// A field a `v1` artifact always has. A document that names the format and lacks one is not
/// a `v1` artifact, so it is refused as an unknown format.
fn key<'a>(value: &'a Value, name: &str) -> ExportResult<&'a Value> {
    match value.as_object().and_then(|object| object.get(name)) {
        Some(found) => Ok(found),
        None => malformed(name),
    }
}

fn items(value: &Value) -> ExportResult<&[Value]> {
    match value {
        Value::Array(elements) => Ok(elements),
        _ => malformed("a list"),
    }
}

fn malformed<T>(what: &str) -> ExportResult<T> {
    Err(ArtifactError::new(
        ArtifactErrorCode::UnknownFormat,
        format!("the document names {FORMAT} but has no well-formed {what}"),
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::artifacts::fixtures::{artifact_text, placement, result, result_with};

    type Export = fn(&str) -> Result<String, ArtifactError>;

    const EXPORTS: [Export; 3] = [export_json, export_csv, export_work_order_html];

    fn built(result: &Value, orders: &str) -> String {
        artifact_text(result, orders).unwrap()
    }

    fn ordered() -> String {
        built(&result(), r#"{"0":[1,0]}"#)
    }

    #[test]
    fn the_json_export_is_the_canonical_artifact() {
        let artifact = ordered();
        assert_eq!(export_json(&artifact).unwrap(), artifact);
    }

    #[test]
    fn every_export_refuses_a_document_it_cannot_read() {
        let mut foreign: Value = serde_json::from_str(&ordered()).unwrap();
        foreign["format"] = json!("packvium-operational-artifact/v2");
        for export in EXPORTS {
            for document in [
                foreign.to_string(),
                r#"["not","an","artifact"]"#.to_string(),
            ] {
                assert_eq!(export(&document).unwrap_err().code(), "unknown_format");
            }
            assert_eq!(export("{").unwrap_err().code(), "invalid_json");
        }
    }

    #[test]
    fn a_document_that_names_the_format_but_lacks_its_fields_is_not_read_as_one() {
        let truncated = format!(r#"{{"format":"{FORMAT}"}}"#);
        // JSON is the document's own spelling and needs no field of it.
        assert_eq!(export_json(&truncated).unwrap(), truncated);
        for export in [export_csv, export_work_order_html] {
            assert_eq!(export(&truncated).unwrap_err().code(), "unknown_format");
        }
    }

    #[test]
    fn the_csv_has_a_header_one_row_per_step_in_order_and_one_per_unplaced_item() {
        let mut result = result();
        result["unpacked_items"] = json!([{
            "item_id": "jack#1", "item_type": "jack",
            "reason": "no_compatible_container_dimensions", "details": [],
            "proof": {"level": "proven"},
        }]);
        let csv = export_csv(&built(&result, r#"{"0":[1,0]}"#)).unwrap();
        let rows: Vec<&str> = csv.split("\r\n").collect();
        assert_eq!(rows[0], CSV_COLUMNS.join(","));
        assert_eq!(
            rows[1],
            "step,0,crate,1,tin,LWH,160000,0,0,10,0,0,5,10,10,mm,,"
        );
        assert_eq!(rows[2], "step,0,crate,2,box,LWH,0,0,0,0,0,0,10,10,10,mm,,");
        assert_eq!(
            rows[3],
            "unplaced,,,,jack,,,,,,,,,,,,no_compatible_container_dimensions,proven"
        );
        assert_eq!(rows[4], "");
        assert_eq!(rows.len(), 5);
    }

    #[test]
    fn a_csv_field_is_quoted_only_when_it_must_be_and_never_rewritten() {
        let result = result_with(vec![
            placement("a,\"b\"", 0, 10),
            placement("=SUM(A1)", 10, 10),
            placement("two\nlines", 20, 10),
        ]);
        let csv = export_csv(&built(&result, "{}")).unwrap();
        assert!(csv.contains(",\"a,\"\"b\"\"\","), "{csv}");
        assert!(csv.contains(",=SUM(A1),"), "{csv}");
        assert!(csv.contains(",\"two\nlines\","), "{csv}");
    }

    #[test]
    fn the_work_order_is_self_contained_escaped_and_lists_every_step() {
        let result = result_with(vec![placement("<b>&'\"", 0, 10), placement("tin", 10, 5)]);
        let html = export_work_order_html(&built(&result, r#"{"0":[1,0]}"#)).unwrap();
        assert!(html.starts_with("<!DOCTYPE html>\n") && html.ends_with("</html>\n"));
        assert!(!html.contains("<script") && !html.contains("http") && !html.contains(" src="));
        assert!(html.contains("&lt;b&gt;&amp;&#39;&quot;") && !html.contains("<b>&'"));
        assert_eq!(html.matches("&#9744;").count(), 2);
        assert!(html.contains("Order: loading."));
        assert!(html.contains("extreme_point") && html.contains("cartons v3"));
        assert!(html.is_ascii());
    }

    #[test]
    fn the_work_order_says_when_there_is_no_order_and_when_everything_was_packed() {
        let html = export_work_order_html(&built(&result(), "{}")).unwrap();
        assert!(html.contains("Order: unavailable."));
        assert!(html.contains("<p>Every item was packed.</p>"));
    }

    #[test]
    fn a_value_with_no_single_rendering_is_refused_rather_than_printed_four_ways() {
        // Python would print `True`, PHP `1`, JavaScript `true`: none of them is the answer.
        let mut result = result();
        result["containers"][0]["container_type"] = json!(true);
        let artifact = built(&result, "{}");
        for export in [export_csv, export_work_order_html] {
            assert_eq!(export(&artifact).unwrap_err().code(), "invalid_value");
        }
        assert!(export_json(&artifact).is_ok());
    }

    #[test]
    fn a_value_only_the_work_order_prints_refuses_only_the_work_order() {
        for (pointer, value) in [
            ("/feasibility/code", json!(true)),
            ("/containers/0/volume_utilization", json!(0.5)),
            ("/algorithm/seed", json!(1.5)),
            ("/score/1", json!({"nested": 1})),
        ] {
            let mut result = result();
            *result.pointer_mut(pointer).unwrap() = value;
            let artifact = built(&result, "{}");
            assert!(export_csv(&artifact).is_ok(), "{pointer}");
            let error = export_work_order_html(&artifact).unwrap_err();
            assert_eq!(error.code(), "invalid_value", "{pointer}");
        }
    }

    #[test]
    fn a_hand_edited_container_index_is_refused_rather_than_numbered() {
        let mut artifact: Value = serde_json::from_str(&ordered()).unwrap();
        for index in [json!(0.5), json!(true), json!("0")] {
            artifact["work_order"]["containers"][0]["container_index"] = index;
            let error = export_work_order_html(&artifact.to_string()).unwrap_err();
            assert_eq!(error.code(), "invalid_value");
        }
    }

    #[test]
    fn strings_integers_and_null_are_the_rendering_every_engine_shares() {
        let mut result = result();
        result["containers"][0]["container_type"] = Value::Null;
        let artifact = built(&result, r#"{"0":[1,0]}"#);
        let html = export_work_order_html(&artifact).unwrap();
        assert!(html.contains("<h2>Container 1: &mdash;</h2>"), "{html}");
        assert!(html.contains("seed 7</td>"), "{html}");
        assert!(
            html.contains("<tr><td>Score</td><td>[1, 0, 250]</td></tr>"),
            "{html}"
        );
        let csv = export_csv(&artifact).unwrap();
        assert!(csv.contains("\r\nstep,0,,1,tin,"), "{csv}");
    }

    #[test]
    fn the_facts_say_why_a_replay_is_not_exact_and_dash_what_the_result_lacks() {
        let mut result = result();
        result.as_object_mut().unwrap().remove("algorithm");
        result["catalog_versions_used"] = json!([]);
        let html = export_work_order_html(&built(&result, "{}")).unwrap();
        for row in [
            "<tr><td>Replay</td><td>not_guaranteed (provenance.solver)</td></tr>",
            "<tr><td>Solver</td><td>&mdash;</td></tr>",
            "<tr><td>Catalogs</td><td>&mdash;</td></tr>",
        ] {
            assert!(html.contains(row), "{row}\n{html}");
        }
    }

    #[test]
    fn the_not_packed_section_lists_every_unplaced_item_with_its_proof() {
        let mut result = result();
        result["unpacked_items"] = json!([
            {"item_type": "jack", "reason": "no_compatible_container_dimensions",
             "details": [], "proof": {"level": "proven"}},
            {"reason": "no_feasible_placement", "details": []},
        ]);
        let html = export_work_order_html(&built(&result, "{}")).unwrap();
        for row in [
            "<thead><tr><th>Item</th><th>Reason</th><th>Proof</th></tr></thead>",
            "<tr><td>jack</td><td>no_compatible_container_dimensions</td><td>proven</td></tr>",
            "<tr><td>&mdash;</td><td>no_feasible_placement</td><td>&mdash;</td></tr>",
        ] {
            assert!(html.contains(row), "{row}\n{html}");
        }
        assert!(!html.contains("Every item was packed."));
    }

    fn edited(change: impl FnOnce(&mut Value)) -> String {
        let mut artifact: Value = serde_json::from_str(&ordered()).unwrap();
        change(&mut artifact);
        artifact.to_string()
    }

    #[test]
    fn a_hand_edited_artifact_is_refused_where_it_is_no_longer_v1() {
        let catalog = edited(|artifact| {
            artifact["provenance"]["catalog_versions_used"] = json!(["cartons"]);
        });
        let error = export_work_order_html(&catalog).unwrap_err();
        assert_eq!(error.code(), "unknown_format");

        let containers = edited(|artifact| artifact["work_order"]["containers"] = json!({}));
        assert_eq!(
            export_csv(&containers).unwrap_err().code(),
            "unknown_format"
        );

        let solver = edited(|artifact| artifact["provenance"]["solver"] = json!(["balanced"]));
        let error = export_work_order_html(&solver).unwrap_err();
        assert_eq!(error.code(), "unknown_format");
    }

    #[test]
    fn a_hand_edited_replay_reason_or_solver_is_read_with_the_reference_truthiness() {
        let replay_with = |because: Value| {
            export_work_order_html(&edited(|artifact| {
                artifact["provenance"]["replay"]["because"] = because;
            }))
        };
        for falsy in [
            json!(false),
            json!(0),
            json!(0.0),
            json!(""),
            json!([]),
            json!({}),
        ] {
            let html = replay_with(falsy.clone()).unwrap();
            assert!(
                html.contains("<tr><td>Replay</td><td>exact</td></tr>"),
                "{falsy}"
            );
        }
        let html = replay_with(json!(1)).unwrap();
        assert!(html.contains("<td>exact (1)</td>"), "{html}");
        for unprintable in [json!(true), json!(0.5), json!(["x"])] {
            let error = replay_with(unprintable.clone()).unwrap_err();
            assert_eq!(error.code(), "invalid_value", "{unprintable}");
        }
        for falsy in [json!([]), json!({})] {
            let artifact = edited(|artifact| artifact["provenance"]["solver"] = falsy);
            let html = export_work_order_html(&artifact).unwrap();
            assert!(html.contains("<tr><td>Solver</td><td>&mdash;</td></tr>"));
        }
    }

    #[test]
    fn exports_are_deterministic() {
        let artifact = ordered();
        for export in EXPORTS {
            assert_eq!(export(&artifact), export(&artifact));
        }
    }
}
