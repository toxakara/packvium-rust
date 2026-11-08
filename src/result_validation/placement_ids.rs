//! Every reported placement names itself (1.10): `placement_id` is the canonical JSON text of
//! `[container id, x, y, z, item type, orientation]`, with the position in ticks.
//!
//! The id is recomputed here from the reported fields rather than with the engine's writer, so
//! the validator shares no code with what it judges. A Rust string holds no lone UTF-16
//! surrogate, so the reference's lowercase `\udxxx` escape for one never applies.

use std::collections::HashSet;
use std::fmt::Write as _;

use super::Issue;
use super::json::{Json, Res};
use super::pyfmt::py_str;

/// RFC 8785's spelling of a string, as the reference's `_id_string` writes it.
fn id_string(text: &str) -> String {
    let mut spelled = String::with_capacity(text.len() + 2);
    spelled.push('"');
    for character in text.chars() {
        match character {
            '"' => spelled.push_str("\\\""),
            '\\' => spelled.push_str("\\\\"),
            '\u{8}' => spelled.push_str("\\b"),
            '\t' => spelled.push_str("\\t"),
            '\n' => spelled.push_str("\\n"),
            '\u{c}' => spelled.push_str("\\f"),
            '\r' => spelled.push_str("\\r"),
            control if control < '\u{20}' => {
                let _ = write!(spelled, "\\u{:04x}", control as u32);
            }
            other => spelled.push(other),
        }
    }
    spelled.push('"');
    spelled
}

/// The id a placement in `container_id` must carry, each field read as the reference's
/// `str()` reads it.
fn expected_id(container_id: &Json, placement: &Json) -> Res<String> {
    let position = placement.at("position")?;
    let mut parts = vec![id_string(&py_str(container_id))];
    for axis in ["x", "y", "z"] {
        parts.push(py_str(position.at(axis)?.at("ticks")?));
    }
    for name in ["item_type", "orientation"] {
        parts.push(id_string(&py_str(placement.at(name)?)));
    }
    Ok(format!("[{}]", parts.join(",")))
}

/// Every reported placement carries its `placement_id` and no two carry the same one. Read
/// from the result as reported: a lattice summary has no placement objects, so it has no ids
/// to check. O(placements).
pub(crate) fn check_placement_ids(result: &Json, issues: &mut Vec<Issue>) -> Res<()> {
    let mut seen: HashSet<String> = HashSet::new();
    let blank = Json::str_of("");
    for container in result.at("containers")?.list("result.containers")? {
        let container_id = container.get_or("id", &blank)?;
        for placement in container.at("placements")?.list("placements")? {
            let expected = expected_id(container_id, placement)?;
            let reported = placement.get_null("placement_id")?;
            let place = format!(
                "{} in {}",
                py_str(placement.at("item_id")?),
                py_str(container_id)
            );
            let Json::Str(reported) = reported else {
                issues.push(Issue::new(
                    "placement_id_mismatch",
                    format!("{place}: no placement_id, expected {expected}"),
                ));
                continue;
            };
            if *reported != expected {
                issues.push(Issue::new(
                    "placement_id_mismatch",
                    format!("{place}: reported {reported}, expected {expected}"),
                ));
            }
            if !seen.insert(reported.clone()) {
                issues.push(Issue::new("duplicate_placement_id", reported.clone()));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placement(id: Option<&str>) -> Json {
        let mut text = String::from(
            r#"{"item_id":"cube#1","item_type":"cube","orientation":"LWH","position":{"x":{"ticks":1},"y":{"ticks":0},"z":{"ticks":2}}"#,
        );
        if let Some(id) = id {
            let _ = write!(text, r#","placement_id":{}"#, id_string(id));
        }
        text.push('}');
        super::super::json::parse(&text, "a placement").unwrap()
    }

    fn result(container_id: &str, placements: Vec<Json>) -> Json {
        Json::Dict(vec![(
            "containers".into(),
            Json::List(vec![Json::Dict(vec![
                ("id".into(), Json::str_of(container_id)),
                ("placements".into(), Json::List(placements)),
            ])]),
        )])
    }

    fn issues(result: &Json) -> Vec<(String, String)> {
        let mut found = Vec::new();
        check_placement_ids(result, &mut found).unwrap();
        found
            .into_iter()
            .map(|issue| (issue.code, issue.detail))
            .collect()
    }

    #[test]
    fn a_control_character_and_a_quote_are_escaped_as_rfc_8785_escapes_them() {
        assert_eq!(
            id_string("a\"\\\u{8}\t\n\u{c}\r\u{1}é"),
            "\"a\\\"\\\\\\b\\t\\n\\f\\r\\u0001é\""
        );
    }

    #[test]
    fn a_missing_a_wrong_and_a_repeated_id_are_each_reported() {
        let id = r#"["box#1",1,0,2,"cube","LWH"]"#;
        assert!(issues(&result("box#1", vec![placement(Some(id))])).is_empty());
        assert_eq!(
            issues(&result("box#1", vec![placement(None)])),
            vec![(
                "placement_id_mismatch".into(),
                format!("cube#1 in box#1: no placement_id, expected {id}")
            )]
        );
        assert_eq!(
            issues(&result(
                "box#1",
                vec![placement(Some("x")), placement(Some("x"))]
            )),
            vec![
                (
                    "placement_id_mismatch".into(),
                    format!("cube#1 in box#1: reported x, expected {id}")
                ),
                (
                    "placement_id_mismatch".into(),
                    format!("cube#1 in box#1: reported x, expected {id}")
                ),
                ("duplicate_placement_id".into(), "x".into()),
            ]
        );
    }

    #[test]
    fn a_placement_without_a_position_is_not_judged() {
        let bare = super::super::json::parse(r#"{"item_id":"a"}"#, "a placement").unwrap();
        let mut found = Vec::new();
        assert!(check_placement_ids(&result("box#1", vec![bare]), &mut found).is_err());
    }
}
