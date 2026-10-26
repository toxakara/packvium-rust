//! The request, read independently of the engine's own model (the reference's `ItemSpec`,
//! `ContainerSpec` and `MachineSpec`).

use std::collections::BTreeSet;

use super::MalformedResult;
use super::container::ratio_ppm;
use super::geometry::{Cuboid, ROTATIONS, Vertex};
use super::json::{EMPTY_LIST, Json, Ordered, Res};
use super::num::{Ratio, mul};
use super::units::{LENGTH, WEIGHT, dimensions, ratio_of, to_ticks};

/// The six axis-aligned exit directions a placement could be slid out through.
pub(crate) const EXIT_DIRECTIONS: [&str; 6] = ["+x", "-x", "+y", "-y", "+z", "-z"];

const UPRIGHT_ROTATIONS: [&str; 2] = ["LWH", "WLH"];

#[derive(Clone, Debug)]
pub(crate) struct ItemSpec {
    pub(crate) id: String,
    pub(crate) dimensions: [i128; 3],
    pub(crate) weight: i128,
    pub(crate) quantity: Json,
    pub(crate) allowed_rotations: Vec<Json>,
    pub(crate) stackable: bool,
    pub(crate) must_be_on_floor: bool,
    pub(crate) max_top_load: Option<i128>,
    pub(crate) minimum_support_ratio: Ratio,
    pub(crate) tags: BTreeSet<String>,
    pub(crate) eligible_container_tags: BTreeSet<String>,
    pub(crate) max_stacked_items: Option<i128>,
    pub(crate) ground_contact_rule: Json,
    pub(crate) nesting_height: Option<i128>,
    pub(crate) incompatible_tags: BTreeSet<String>,
    pub(crate) stop_index: Option<i128>,
    pub(crate) value: Json,
    pub(crate) shape_type: Json,
    pub(crate) hull_vertices: Option<Vec<Vertex>>,
    pub(crate) compression_ratio_ppm: Option<i128>,
    pub(crate) max_compression_pressure_kpa: Option<i128>,
    pub(crate) stacking_group: Json,
    pub(crate) accepts_stacking_groups: BTreeSet<String>,
}

impl ItemSpec {
    pub(crate) fn allows(&self, orientation: &Json) -> bool {
        self.allowed_rotations
            .iter()
            .any(|allowed| allowed.py_eq(orientation))
    }

    pub(crate) fn is_convex_hull(&self) -> bool {
        self.shape_type.is_str("convex_hull")
    }

    pub(crate) fn rotated(&self, orientation: &str) -> Res<[i128; 3]> {
        let axes = rotation_axes(orientation)?;
        Ok([
            self.dimensions[axes[0]],
            self.dimensions[axes[1]],
            self.dimensions[axes[2]],
        ])
    }
}

#[derive(Clone, Debug)]
pub(crate) struct MachineSpec {
    pub(crate) minimum: [i128; 3],
    pub(crate) increment: [i128; 3],
    pub(crate) price_per_square_metre_minor: i128,
    pub(crate) tare_per_square_metre: i128,
    pub(crate) allowance: [i128; 3],
}

#[derive(Clone, Debug)]
pub(crate) struct Axle {
    pub(crate) position: i128,
    pub(crate) max_load: Option<i128>,
}

#[derive(Clone, Debug)]
pub(crate) struct ContainerSpec {
    pub(crate) id: Json,
    pub(crate) inner: [i128; 3],
    pub(crate) outer: Option<[i128; 3]>,
    pub(crate) cost_minor: i128,
    pub(crate) max_payload: Option<i128>,
    pub(crate) max_items: Option<i128>,
    pub(crate) quantity: Option<i128>,
    pub(crate) obstacles: Vec<Cuboid>,
    pub(crate) tags: BTreeSet<String>,
    /// Sorted by tag, the order the reference walks it in.
    pub(crate) tag_limits: Vec<(String, i128)>,
    pub(crate) void_fill_reserve_ratio: Ratio,
    pub(crate) tare_weight: i128,
    pub(crate) axles: Option<Vec<Axle>>,
    pub(crate) max_stack_density: Option<i128>,
    pub(crate) rate_table: Json,
    pub(crate) access_directions: Vec<&'static str>,
    pub(crate) flat_bag: bool,
    pub(crate) reports_achieved_extent: bool,
    pub(crate) made_to_measure: Option<MachineSpec>,
}

impl ContainerSpec {
    pub(crate) fn volume(&self) -> Res<i128> {
        mul(mul(self.inner[0], self.inner[1])?, self.inner[2])
    }
}

/// `ROTATIONS[orientation]`.
pub(crate) fn rotation_axes(orientation: &str) -> Res<[usize; 3]> {
    ROTATIONS
        .iter()
        .find(|(name, _)| *name == orientation)
        .map(|(_, axes)| *axes)
        .ok_or_else(|| MalformedResult::shape(format!("unknown orientation {orientation:?}")))
}

/// `orientation in ROTATIONS`, for any JSON value; an unhashable one is Python's `TypeError`.
pub(crate) fn is_rotation(orientation: &Json) -> Res<bool> {
    orientation.hash_key()?;
    Ok(matches!(orientation, Json::Str(name) if ROTATIONS.iter().any(|(code, _)| code == name)))
}

/// `frozenset(value)` of a request's tag list. The contract's tags are strings.
fn tag_set(value: &Json, what: &str) -> Res<BTreeSet<String>> {
    value
        .list(what)?
        .iter()
        .map(|tag| tag.text(what).map(str::to_owned))
        .collect()
}

fn optional<T>(raw: &Json, key: &str, read: impl FnOnce(&Json) -> Res<T>) -> Res<Option<T>> {
    match raw.get(key)? {
        None | Some(Json::Null) => Ok(None),
        Some(value) => read(value).map(Some),
    }
}

pub(crate) fn parse_items(request: &Json, unit: &str) -> Res<Ordered<ItemSpec>> {
    let mut items = Ordered::default();
    for (index, raw) in request
        .at("items")?
        .list("request.items")?
        .iter()
        .enumerate()
    {
        let owner = format!("items[{index}]");
        let keep_upright = raw.flag("keep_upright", false, &owner)?;
        let stackable = raw.flag("stackable", true, &owner)?;
        let must_be_on_floor = raw.flag("must_be_on_floor", false, &owner)?;
        let rotations = match raw.get_null("allowed_rotations")? {
            Json::Null if keep_upright => UPRIGHT_ROTATIONS.map(Json::str_of).to_vec(),
            Json::Null => ROTATIONS
                .iter()
                .map(|(code, _)| Json::str_of(code))
                .collect(),
            listed => listed.list("allowed_rotations")?.to_vec(),
        };
        let id = raw.at("id")?;
        let spec = ItemSpec {
            id: id.text("an item id")?.to_owned(),
            dimensions: dimensions(raw.at("dimensions")?, unit)?,
            weight: to_ticks(raw.get_or("weight", &Json::Int(0))?, "g", WEIGHT)?,
            quantity: raw.get_or("quantity", &Json::Int(1))?.clone(),
            allowed_rotations: rotations,
            stackable,
            must_be_on_floor,
            max_top_load: optional(raw, "max_top_load", |value| to_ticks(value, "g", WEIGHT))?,
            minimum_support_ratio: ratio_of(raw.get_or("minimum_support_ratio", &Json::Int(0))?)?,
            tags: tag_set(raw.get_or("tags", &EMPTY_LIST)?, "tags")?,
            eligible_container_tags: tag_set(
                raw.get_or("eligible_container_tags", &EMPTY_LIST)?,
                "eligible_container_tags",
            )?,
            max_stacked_items: raw
                .get_null("max_stacked_items")?
                .opt_int("max_stacked_items")?,
            ground_contact_rule: raw.get_null("ground_contact_rule")?.clone(),
            nesting_height: optional(raw, "nesting_height", |value| to_ticks(value, unit, LENGTH))?,
            incompatible_tags: tag_set(
                raw.get_or("incompatible_tags", &EMPTY_LIST)?,
                "incompatible_tags",
            )?,
            stop_index: raw.get_null("stop_index")?.opt_int("stop_index")?,
            value: raw.get_null("value")?.clone(),
            shape_type: raw
                .get_or("shape_type", &Json::str_of("rigid_cuboid"))?
                .clone(),
            hull_vertices: optional(raw, "hull_vertices", |value| {
                value
                    .list("hull_vertices")?
                    .iter()
                    .map(|vertex| {
                        Ok([
                            to_ticks(vertex.at("x")?, unit, LENGTH)?,
                            to_ticks(vertex.at("y")?, unit, LENGTH)?,
                            to_ticks(vertex.at("z")?, unit, LENGTH)?,
                        ])
                    })
                    .collect()
            })?,
            // The solvers' ratio rule, applied once: floor(ratio * 1000000 + 0.5) on the double.
            compression_ratio_ppm: optional(raw, "compression_ratio", |value| {
                Ok(ratio_ppm(ratio_of(value)?))
            })?,
            max_compression_pressure_kpa: raw
                .get_null("max_compression_pressure_kpa")?
                .opt_int("max_compression_pressure_kpa")?,
            stacking_group: raw.get_null("stacking_group")?.clone(),
            accepts_stacking_groups: match raw.get_null("accepts_stacking_groups")? {
                value if !value.truthy() => BTreeSet::new(),
                value => tag_set(value, "accepts_stacking_groups")?,
            },
        };
        items.insert(id, spec)?;
    }
    Ok(items)
}

pub(crate) fn parse_containers(request: &Json, unit: &str) -> Res<Ordered<ContainerSpec>> {
    let mut containers = Ordered::default();
    for raw in request.at("containers")?.list("request.containers")? {
        let spec = ContainerSpec {
            id: raw.at("id")?.clone(),
            inner: dimensions(raw.at("inner_dimensions")?, unit)?,
            access_directions: access_directions(raw)?,
            outer: optional(raw, "outer_dimensions", |value| dimensions(value, unit))?,
            cost_minor: raw.get_or("cost_minor", &Json::Int(0))?.int("cost_minor")?,
            max_payload: optional(raw, "max_payload", |value| to_ticks(value, "g", WEIGHT))?,
            max_items: raw.get_null("max_items")?.opt_int("max_items")?,
            quantity: raw.get_null("quantity")?.opt_int("quantity")?,
            obstacles: obstacles(raw, unit)?,
            tags: tag_set(raw.get_or("tags", &EMPTY_LIST)?, "tags")?,
            tag_limits: tag_limits(raw)?,
            void_fill_reserve_ratio: ratio_of(
                raw.get_or("void_fill_reserve_ratio", &Json::Int(0))?,
            )?,
            tare_weight: to_ticks(raw.get_or("tare_weight", &Json::Int(0))?, "g", WEIGHT)?,
            axles: optional(raw, "axles", |value| {
                value
                    .list("axles")?
                    .iter()
                    .map(|axle| {
                        Ok(Axle {
                            position: to_ticks(axle.at("position")?, unit, LENGTH)?,
                            max_load: optional(axle, "max_load", |load| {
                                to_ticks(load, "g", WEIGHT)
                            })?,
                        })
                    })
                    .collect()
            })?,
            max_stack_density: optional(raw, "max_stack_density", |value| {
                to_ticks(value, "g", WEIGHT)
            })?,
            rate_table: raw.get_null("rate_table")?.clone(),
            flat_bag: !raw.get_null("deformable_boundary")?.is_null(),
            reports_achieved_extent: !raw.get_null("deformable_boundary")?.is_null()
                || !raw.get_null("made_to_measure")?.is_null(),
            made_to_measure: optional(raw, "made_to_measure", |machine| {
                parse_machine(machine, unit)
            })?,
        };
        containers.insert(raw.at("id")?, spec)?;
    }
    Ok(containers)
}

fn access_directions(raw: &Json) -> Res<Vec<&'static str>> {
    let declared = raw
        .get_or("access_directions", &EMPTY_LIST)?
        .list("access_directions")?;
    for direction in declared {
        direction.hash_key()?;
    }
    Ok(EXIT_DIRECTIONS
        .into_iter()
        .filter(|direction| declared.iter().any(|declared| declared.is_str(direction)))
        .collect())
}

fn obstacles(raw: &Json, unit: &str) -> Res<Vec<Cuboid>> {
    let mut boxes = Vec::new();
    for obstacle in raw.get_or("obstacles", &EMPTY_LIST)?.list("obstacles")? {
        let additional = obstacle
            .get_or("additional_boxes", &EMPTY_LIST)?
            .list("additional_boxes")?;
        for component in std::iter::once(obstacle).chain(additional) {
            let empty = Json::Dict(Vec::new());
            let origin = component.get_or("origin", &empty)?;
            let [length, width, height] = dimensions(component.at("dimensions")?, unit)?;
            let zero = Json::Int(0);
            boxes.push(Cuboid::new(
                to_ticks(origin.get_or("x", &zero)?, unit, LENGTH)?,
                to_ticks(origin.get_or("y", &zero)?, unit, LENGTH)?,
                to_ticks(origin.get_or("z", &zero)?, unit, LENGTH)?,
                length,
                width,
                height,
            )?);
        }
    }
    Ok(boxes)
}

/// `{str(tag): int(limit)}`, then sorted by tag as the reference iterates it.
fn tag_limits(raw: &Json) -> Res<Vec<(String, i128)>> {
    let empty = Json::Dict(Vec::new());
    let declared = raw.get_or("tag_limits", &empty)?;
    let pairs = declared
        .as_dict()
        .ok_or_else(|| MalformedResult::shape("tag_limits must be an object".to_owned()))?;
    let mut limits: Vec<(String, i128)> = Vec::new();
    for (tag, limit) in pairs {
        limits.push((tag.clone(), limit.py_int("tag_limits")?));
    }
    limits.sort();
    Ok(limits)
}

fn parse_machine(raw: &Json, unit: &str) -> Res<MachineSpec> {
    let sides = |name: &str| -> Res<[i128; 3]> {
        match raw.get_null(name)? {
            Json::Null => Ok([0, 0, 0]),
            value => dimensions(value, unit),
        }
    };
    let price = raw.get_null("cost_per_square_metre_minor")?;
    Ok(MachineSpec {
        minimum: sides("minimum")?,
        increment: sides("increment")?,
        price_per_square_metre_minor: if price.truthy() {
            price.int("cost_per_square_metre_minor")?
        } else {
            0
        },
        tare_per_square_metre: optional(raw, "tare_per_square_metre", |value| {
            to_ticks(value, "g", WEIGHT)
        })?
        .unwrap_or(0),
        allowance: sides("outer_allowance")?,
    })
}
