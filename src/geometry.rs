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
