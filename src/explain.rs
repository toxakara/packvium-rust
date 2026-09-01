//! Deterministic, localization-ready explanations for structured rejection codes.

use crate::UnpackedItem;
use std::collections::BTreeMap;
use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RejectionCode {
    NoCompatibleContainer,
    DimensionExceeded,
    PayloadExceeded,
    RotationForbidden,
    ObstacleCollision,
    InsufficientSupport,
    ContainerInventoryExhausted,
    TimeLimit,
    SearchExhausted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Explanation {
    pub message_key: String,
    pub arguments: BTreeMap<String, String>,
    pub default_message: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnknownReasonError {
    pub reason: String,
}

impl fmt::Display for UnknownReasonError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "no explanation registered for reason code {:?}",
            self.reason
        )
    }
}

impl std::error::Error for UnknownReasonError {}

pub fn explain_reason(reason: &str) -> Result<&'static str, UnknownReasonError> {
    let message = match reason {
        "no_compatible_container_dimensions" => {
            "does not fit inside any offered container in any rotation"
        }
        "rotation_restricted" => {
            "would fit in some container with more rotations allowed, but not with the rotations this item permits"
        }
        "payload_exceeded" => "exceeds the maximum payload of every offered container",
        "policy_rule" => {
            "is forbidden from every offered container by a policy rule the request \
             declared -- the rule and version are in the details"
        }
        "no_eligible_container" => "shares no eligible container tag with any offered container",
        "time_limit" => "was not reached before the configured time limit expired",
        "effort_limit" => "was not reached before the configured effort budget was exhausted",
        "group_cannot_fit_together" => "belongs to a group that could not all be placed together",
        "insufficient_support" => {
            "would fit geometrically, but only by resting on support the minimum support ratio forbids"
        }
        "no_feasible_placement" => {
            "found no feasible placement in the containers offered, for a reason the search could not further isolate"
        }
        "search_exhausted" => {
            "was not placed before the configured search strategies were exhausted"
        }
        "exact_search_incomplete" => {
            "was not placed because the exact search ended before proving a final answer"
        }
        "container_inventory_exhausted" => {
            "requires another compatible container, but the declared inventory is exhausted"
        }
        _ => {
            return Err(UnknownReasonError {
                reason: reason.into(),
            });
        }
    };
    Ok(message)
}

pub fn explanation_for_unpacked_item(
    item: &UnpackedItem,
) -> Result<Explanation, UnknownReasonError> {
    let arguments = BTreeMap::from([
        ("item_id".into(), item.instance.id()),
        ("evidence_level".into(), item.proof.level.clone()),
        ("details".into(), item.details.join("; ")),
    ]);
    Ok(Explanation {
        message_key: format!("packvium.unpacked.{}", item.reason),
        arguments,
        default_message: explain_reason(&item.reason)?,
    })
}

pub fn explain_unpacked_item(item: &UnpackedItem) -> Result<String, UnknownReasonError> {
    let explanation = explanation_for_unpacked_item(item)?;
    let prefix = match item.proof.level.as_str() {
        "proven" => "Proven: ",
        "unknown_due_to_limit" => "Unknown (limit reached): ",
        "observed" => "Observed: ",
        "inferred" => "Inferred: ",
        _ => "",
    };
    let details = if item.details.is_empty() {
        String::new()
    } else {
        format!(" ({})", item.details.join("; "))
    };
    Ok(format!(
        "{}: {prefix}{}{details}",
        item.instance.id(),
        explanation.default_message
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Dimensions, Item, ItemInstance, Length, Rotation, Weight};
    use std::collections::{BTreeMap, BTreeSet};

    fn unpacked() -> UnpackedItem {
        let item = Item {
            id: "crate".into(),
            dimensions: Dimensions {
                length: Length(1),
                width: Length(1),
                height: Length(1),
            },
            weight: Weight(1),
            quantity: 1,
            allowed_rotations: vec![Rotation::Lwh],
            stackable: true,
            must_be_on_floor: false,
            max_top_load: None,
            minimum_support_ratio: 0.0,
            group: None,
            tags: BTreeSet::new(),
            incompatible_tags: BTreeSet::new(),
            priority: 0,
            metadata: BTreeMap::new(),
            nesting_height: None,
            max_stacked_items: None,
            ground_contact_rule: None,
            stop_index: None,
            value: None,
            shape_type: crate::geometry::ShapeType::RigidCuboid,
            hull_vertices: None,
            compression_ratio_ppm: None,
            max_compression_pressure_kpa: None,
            eligible_container_tags: BTreeSet::new(),
        };
        UnpackedItem::new(
            ItemInstance { item, sequence: 1 },
            "payload_exceeded".into(),
            vec!["limit=1kg".into()],
        )
    }

    #[test]
    fn descriptor_and_rendering_are_stable_and_localization_ready() {
        let item = unpacked();
        let descriptor = explanation_for_unpacked_item(&item).unwrap();
        assert_eq!(descriptor.message_key, "packvium.unpacked.payload_exceeded");
        assert_eq!(descriptor.arguments["item_id"], "crate#1");
        assert_eq!(descriptor.arguments["evidence_level"], "proven");
        assert_eq!(descriptor.arguments["details"], "limit=1kg");
        assert_eq!(
            explain_unpacked_item(&item).unwrap(),
            "crate#1: Proven: exceeds the maximum payload of every offered container (limit=1kg)"
        );
    }

    #[test]
    fn every_registered_reason_has_a_message_and_every_level_a_prefix() {
        // The vocabulary is shared by four engines and the validator, so a code added to
        // one of them and not to this table is a runtime error at the moment a caller
        // asks for an explanation. Walking the whole list is what makes a new arm
        // impossible to add without a message -- one arm at a time never does.
        for reason in [
            "no_compatible_container_dimensions",
            "rotation_restricted",
            "payload_exceeded",
            "policy_rule",
            "no_eligible_container",
            "time_limit",
            "effort_limit",
            "group_cannot_fit_together",
            "insufficient_support",
            "no_feasible_placement",
            "search_exhausted",
            "exact_search_incomplete",
            "container_inventory_exhausted",
        ] {
            let message =
                explain_reason(reason).unwrap_or_else(|_| panic!("{reason} has no message"));
            assert!(!message.is_empty(), "{reason} has an empty message");
        }
        assert!(
            explain_reason("policy_rule")
                .unwrap()
                .contains("policy rule")
        );

        // Every proof level renders its own prefix; an unrecognised one renders none
        // rather than inventing a label.
        let mut item = unpacked();
        for (level, expected) in [
            ("proven", "Proven: "),
            ("unknown_due_to_limit", "Unknown (limit reached): "),
            ("observed", "Observed: "),
            ("inferred", "Inferred: "),
        ] {
            item.proof.level = level.into();
            assert!(
                explain_unpacked_item(&item).unwrap().contains(expected),
                "{level} did not render {expected}"
            );
        }
    }

    #[test]
    fn unknown_reason_is_structured_error() {
        assert_eq!(
            explain_reason("new_reason").unwrap_err(),
            UnknownReasonError {
                reason: "new_reason".into()
            }
        );
    }
}
