use crate::units::Length;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct Point {
    pub x: i64,
    pub y: i64,
    pub z: i64,
}

impl Point {
    pub const ZERO: Self = Self { x: 0, y: 0, z: 0 };

    pub fn to_json(self, unit: &str) -> Value {
        serde_json::json!({
            "x": Length(self.x).to_json(unit),
            "y": Length(self.y).to_json(unit),
            "z": Length(self.z).to_json(unit),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct Dimensions {
    pub length: Length,
    pub width: Length,
    pub height: Length,
}

impl Dimensions {
    pub fn volume(self) -> i128 {
        self.length.0 as i128 * self.width.0 as i128 * self.height.0 as i128
    }

    pub fn base_area(self) -> i128 {
        self.length.0 as i128 * self.width.0 as i128
    }

    pub fn longest_edge(self) -> i64 {
        self.length.0.max(self.width.0).max(self.height.0)
    }

    pub fn fits_inside(self, other: Self) -> bool {
        self.length <= other.length && self.width <= other.width && self.height <= other.height
    }

    pub fn expand(self, clearance: Length) -> Self {
        let twice = clearance.0.saturating_mul(2);
        Self {
            length: Length(self.length.0.saturating_add(twice)),
            width: Length(self.width.0.saturating_add(twice)),
            height: Length(self.height.0.saturating_add(twice)),
        }
    }

    pub fn rotated(self, rotation: Rotation) -> Self {
        let (length, width, height) = (self.length, self.width, self.height);
        match rotation {
            Rotation::Lwh => Self {
                length,
                width,
                height,
            },
            Rotation::Lhw => Self {
                length,
                width: height,
                height: width,
            },
            Rotation::Wlh => Self {
                length: width,
                width: length,
                height,
            },
            Rotation::Whl => Self {
                length: width,
                width: height,
                height: length,
            },
            Rotation::Hlw => Self {
                length: height,
                width: length,
                height: width,
            },
            Rotation::Hwl => Self {
                length: height,
                width,
                height: length,
            },
        }
    }

    pub fn unique_rotations(self, allowed: &[Rotation]) -> Vec<(Rotation, Self)> {
        let mut rotations = Vec::new();
        for &rotation in allowed {
            let dimensions = self.rotated(rotation);
            if !rotations
                .iter()
                .any(|(_, existing)| *existing == dimensions)
            {
                rotations.push((rotation, dimensions));
            }
        }
        rotations
    }

    pub fn to_json(self, unit: &str) -> Value {
        serde_json::json!({
            "length": self.length.to_json(unit),
            "width": self.width.to_json(unit),
            "height": self.height.to_json(unit),
        })
    }
}

/// How much of an item's declared box the item actually occupies.
///
/// `RigidCuboid` is the default and the whole of the contract before this epic: the item is
/// its box. The other two narrow that in one dimension each -- `ConvexHull` in space,
/// `Compressible` in height under load -- and neither may be inferred. An engine that packed a
/// hull as its bounding box would return a plan that validates and does not physically fit.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum ShapeType {
    #[default]
    #[serde(rename = "rigid_cuboid")]
    RigidCuboid,
    #[serde(rename = "convex_hull")]
    ConvexHull,
    #[serde(rename = "compressible")]
    Compressible,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum Rotation {
    #[serde(rename = "LWH")]
    Lwh,
    #[serde(rename = "LHW")]
    Lhw,
    #[serde(rename = "WLH")]
    Wlh,
    #[serde(rename = "WHL")]
    Whl,
    #[serde(rename = "HLW")]
    Hlw,
    #[serde(rename = "HWL")]
    Hwl,
}

impl Rotation {
    pub const ALL: [Self; 6] = [
        Self::Lwh,
        Self::Lhw,
        Self::Wlh,
        Self::Whl,
        Self::Hlw,
        Self::Hwl,
    ];
    pub const UPRIGHT: [Self; 2] = [Self::Lwh, Self::Wlh];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lwh => "LWH",
            Self::Lhw => "LHW",
            Self::Wlh => "WLH",
            Self::Whl => "WHL",
            Self::Hlw => "HLW",
            Self::Hwl => "HWL",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Aabb {
    pub origin: Point,
    pub dimensions: Dimensions,
}

impl Aabb {
    pub fn x2(self) -> i64 {
        self.origin.x + self.dimensions.length.0
    }

    pub fn y2(self) -> i64 {
        self.origin.y + self.dimensions.width.0
    }

    pub fn z2(self) -> i64 {
        self.origin.z + self.dimensions.height.0
    }

    pub fn contains(self, other: Self) -> bool {
        other.origin.x >= self.origin.x
            && other.origin.y >= self.origin.y
            && other.origin.z >= self.origin.z
            && other.x2() <= self.x2()
            && other.y2() <= self.y2()
            && other.z2() <= self.z2()
    }

    pub fn intersects(self, other: Self) -> bool {
        self.origin.x < other.x2()
            && self.x2() > other.origin.x
            && self.origin.y < other.y2()
            && self.y2() > other.origin.y
            && self.origin.z < other.z2()
            && self.z2() > other.origin.z
    }

    pub fn overlap_area_xy(self, other: Self) -> i128 {
        let x = (self.x2().min(other.x2()) - self.origin.x.max(other.origin.x)).max(0) as i128;
        let y = (self.y2().min(other.y2()) - self.origin.y.max(other.origin.y)).max(0) as i128;
        x * y
    }
}

/// The six axis-aligned faces a box can leave a container through, in a fixed order.
///
/// Fixed because callers iterate it to pick the *first* clear direction, and an unordered
/// set would make which one they pick depend on hash order.
pub const ALL_DIRECTIONS: [&str; 6] = ["+x", "-x", "+y", "-y", "+z", "-z"];

/// The region between a box's face and a container wall, as `(x1, y1, z1, x2, y2, z2)` in
/// ticks.
///
/// Named rather than left as six bare integers because the solver keeps one of these per
/// door per placement, and `Vec<Vec<(i64, i64, i64, i64, i64, i64)>>` is exactly the type
/// clippy refuses under `-D warnings` -- a refusal worth agreeing with. PHP calls the same
/// concept `Packvium\Domain\SweptRegion`; this gives the Rust core the same word.
pub type SweptRegion = (i64, i64, i64, i64, i64, i64);

/// The region between `box_`'s own face and the matching container wall along `direction`,
/// or `None` when the direction is outside the six.
///
/// It lives here rather than beside either caller because both the solver and the sequence
/// analysis need it, and the sequence module is post-hoc analysis the solver should not be
/// reaching into. `sequence::swept_volume` delegates here and keeps its own error type;
/// this returns `Option` so `geometry` owes nothing to a module above it. The Python and
/// PHP engines carry the same primitive in the same place, for the same reason.
///
/// `direction` names *which wall's region* a box occupies, not a direction of travel: the
/// region between a box and its `+x` wall is the same whether the box is leaving through it
/// or arriving through it.
pub fn swept_volume(box_: Aabb, container: Dimensions, direction: &str) -> Option<SweptRegion> {
    let (mut x1, mut y1, mut z1) = (box_.origin.x, box_.origin.y, box_.origin.z);
    let (mut x2, mut y2, mut z2) = (box_.x2(), box_.y2(), box_.z2());
    match direction {
        "+x" => {
            x1 = x2;
            x2 = container.length.0;
        }
        "-x" => {
            x2 = x1;
            x1 = 0;
        }
        "+y" => {
            y1 = y2;
            y2 = container.width.0;
        }
        "-y" => {
            y2 = y1;
            y1 = 0;
        }
        "+z" => {
            z1 = z2;
            z2 = container.height.0;
        }
        "-z" => {
            z2 = z1;
            z1 = 0;
        }
        _ => return None,
    }
    Some((x1, y1, z1, x2, y2, z2))
}

/// Does `box_` stand anywhere inside a swept region?
///
/// Half-open on every axis, matching `Aabb::intersects`, so a box flush against another's
/// exit face is not treated as standing in its way.
pub fn sweep_intersects(sweep: SweptRegion, box_: Aabb) -> bool {
    let (sx1, sy1, sz1, sx2, sy2, sz2) = sweep;
    sx1 < box_.x2()
        && box_.origin.x < sx2
        && sy1 < box_.y2()
        && box_.origin.y < sy2
        && sz1 < box_.z2()
        && box_.origin.z < sz2
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::Length;

    fn dims(length: i64, width: i64, height: i64) -> Dimensions {
        Dimensions {
            length: Length(length),
            width: Length(width),
            height: Length(height),
        }
    }

    /// The refusal every engine owes, and the one this module had never been asked for.
    ///
    /// `sequence::swept_volume` turns this `None` into `SequenceError::InvalidDirection`,
    /// and the solver's corridor base drops it -- but both reach it only through here, and
    /// nothing called this function with a direction it does not know. Python raises
    /// `InvalidDirectionError` and PHP throws `InvalidArgumentException` at exactly this
    /// point; the three are now held to the same case.
    #[test]
    fn an_unknown_direction_has_no_swept_region() {
        let box_ = Aabb {
            origin: Point { x: 0, y: 0, z: 0 },
            dimensions: dims(10, 10, 10),
        };
        let container = dims(100, 100, 100);
        assert!(swept_volume(box_, container, "sideways").is_none());
        // Every direction it does know still answers, so the arm above is a refusal rather
        // than a hole in the match.
        for direction in ALL_DIRECTIONS {
            assert!(
                swept_volume(box_, container, direction).is_some(),
                "{direction}"
            );
        }
    }
}
