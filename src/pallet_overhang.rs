//! Pallet overhang, ported from Python's `pallet_overhang.py`.
//!
//! A container with `pallet_overhang_limit` `(l, w)` is a deck `L x W`, its declared
//! `inner_dimensions`, that a load may overhang by at most `l` past each end and `w` past each
//! side. Every solver packs the solving frame `(L + 2l) x (W + 2w) x H`, whose origin is the
//! corner of the overhang zone, so the deck lies on `[l, l + L] x [w, w + W]` and every
//! coordinate stays non-negative. The frame is made once, at the pack entry point, so no
//! solver needs to know the limit exists.
//!
//! Only the deck carries weight: a floor placement rests on its overlap with the deck, judged
//! by `minimum_support_ratio` and the centre-of-gravity rule exactly as a supporting box is.
//! The packed pallet is billed and handed up at the size it reached. Exact integers throughout.

use crate::geometry::{Aabb, Dimensions, Point};
use crate::model::{Container, Placement, required_support_area, scaled_support_ratio};
use crate::support_polygon::centre_of_gravity_supported;
use crate::units::Length;

/// How far a load may pass the deck edge, per end along `length` and per side along `width`.
/// Zero forbids overhang on that axis; it is not the same as the field being absent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverhangLimit {
    pub length: Length,
    pub width: Length,
}

/// The weight-bearing footprint inside a solving frame, in frame ticks. Set only by the
/// solving frame; a library caller states an [`OverhangLimit`] instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Deck {
    pub x: i64,
    pub y: i64,
    pub length: i64,
    pub width: i64,
}

impl Deck {
    pub fn x2(self) -> i64 {
        self.x + self.length
    }

    pub fn y2(self) -> i64 {
        self.y + self.width
    }

    /// The deck as a one-tick slab under the floor: only its footprint is ever read.
    fn as_box(self) -> Aabb {
        Aabb {
            origin: Point {
                x: self.x,
                y: self.y,
                z: -1,
            },
            dimensions: Dimensions {
                length: Length(self.length),
                width: Length(self.width),
                height: Length(1),
            },
        }
    }

    /// The area of a floor placement's footprint that rests on the deck. `O(1)`.
    pub(crate) fn contact_area(self, box_: Aabb) -> i128 {
        let dx = box_.x2().min(self.x2()) - box_.origin.x.max(self.x);
        let dy = box_.y2().min(self.y2()) - box_.origin.y.max(self.y);
        if dx > 0 && dy > 0 {
            i128::from(dx) * i128::from(dy)
        } else {
            0
        }
    }

    /// Why the deck cannot carry a floor placement at the `required` ratio, if it cannot:
    /// the same ratio and centre-of-gravity rule a supporting box is judged by. `O(1)`.
    pub(crate) fn refusal(self, box_: Aabb, required: f64) -> Option<&'static str> {
        if required <= 0.0 {
            return None;
        }
        let area = self.contact_area(box_);
        let base = box_.dimensions.base_area();
        if area < required_support_area(base, scaled_support_ratio(required)) {
            return Some("insufficient_support");
        }
        (area * 2 <= base && !centre_of_gravity_supported(box_, [self.as_box()]))
            .then_some("centre_of_gravity_unsupported")
    }

    /// How far the boxes pass the deck in total along each axis: both ends, then both
    /// sides. `O(p)`; `(0, 0)` for none.
    fn overhang_used(self, boxes: impl IntoIterator<Item = Aabb>) -> (i64, i64) {
        let (mut low_x, mut low_y, mut high_x, mut high_y) = (self.x, self.y, self.x2(), self.y2());
        for box_ in boxes {
            low_x = low_x.min(box_.origin.x);
            low_y = low_y.min(box_.origin.y);
            high_x = high_x.max(box_.x2());
            high_y = high_y.max(box_.y2());
        }
        (
            (self.x - low_x) + (high_x - self.x2()),
            (self.y - low_y) + (high_y - self.y2()),
        )
    }

    /// The deck grown by the overhang used, at the load's height. `O(p)`.
    pub(crate) fn achieved_dimensions(self, placements: &[Placement]) -> Dimensions {
        let (over_x, over_y) = self.overhang_used(placements.iter().map(Placement::envelope_box));
        let height = placements
            .iter()
            .map(|placement| placement.envelope_box().z2())
            .max()
            .unwrap_or(0);
        Dimensions {
            length: Length(self.length + over_x),
            width: Length(self.width + over_y),
            height: Length(height),
        }
    }

    /// Per horizontal axis the larger of the declared footprint, outer if stated, and the
    /// deck grown by the overhang used; the declared height. An outer margin is not
    /// positioned against the deck, so it is never added to the overhang. `O(p)`.
    pub(crate) fn billed_dimensions(
        self,
        container: &Container,
        placements: &[Placement],
    ) -> Dimensions {
        let base = container
            .outer_dimensions
            .unwrap_or_else(|| container.declared_inner());
        let (over_x, over_y) = self.overhang_used(placements.iter().map(Placement::envelope_box));
        Dimensions {
            length: Length(base.length.0.max(self.length + over_x)),
            width: Length(base.width.0.max(self.width + over_y)),
            height: base.height,
        }
    }

    /// The shift that seats a frame trial's load with the least overhang. Per axis: flush
    /// with the deck's near edge when the load is no longer than the deck, else centred on
    /// the deck with an odd tick past the far edge. `O(p)`; `(0, 0)` for none.
    pub(crate) fn centring_offset(self, placements: &[Placement]) -> (i64, i64) {
        let boxes = placements.iter().map(Placement::envelope_box);
        let Some((low_x, low_y, high_x, high_y)) = boxes.fold(None, |extent, box_| {
            let (low_x, low_y, high_x, high_y) =
                extent.unwrap_or((box_.origin.x, box_.origin.y, box_.x2(), box_.y2()));
            Some((
                low_x.min(box_.origin.x),
                low_y.min(box_.origin.y),
                high_x.max(box_.x2()),
                high_y.max(box_.y2()),
            ))
        }) else {
            return (0, 0);
        };
        (
            seat(self.x, self.length, low_x, high_x),
            seat(self.y, self.width, low_y, high_y),
        )
    }
}

fn seat(start: i64, length: i64, low: i64, high: i64) -> i64 {
    let span = high - low;
    // Both operands are non-negative here, so `/` is the floor division Python uses.
    let target = if span <= length {
        start
    } else {
        start - (span - length) / 2
    };
    target - low
}

/// The support ratio a placement on the floor reports: whole without a deck.
pub(crate) fn floor_support(deck: Option<Deck>, box_: Aabb) -> (i128, f64) {
    let base = box_.dimensions.base_area();
    match deck {
        None => (base, 1.0),
        Some(deck) => {
            let area = deck.contact_area(box_);
            (area, area as f64 / base as f64)
        }
    }
}

/// The container every solver packs: itself without a limit, else the widened frame
/// carrying its deck. The frame states no limit, so a second call returns it unchanged.
pub(crate) fn solving_frame(container: &Container) -> Container {
    let Some(limit) = container.pallet_overhang_limit else {
        return container.clone();
    };
    let inner = container.inner_dimensions;
    let (overhang_x, overhang_y) = (limit.length.0, limit.width.0);
    Container {
        inner_dimensions: Dimensions {
            length: Length(inner.length.0 + 2 * overhang_x),
            width: Length(inner.width.0 + 2 * overhang_y),
            height: inner.height,
        },
        pallet_overhang_limit: None,
        deck: Some(Deck {
            x: overhang_x,
            y: overhang_y,
            length: inner.length.0,
            width: inner.width.0,
        }),
        ..container.clone()
    }
}

/// A solving frame's container as declared, without the overhang zone: the rung a pallet
/// trial packs first.
pub(crate) fn deck_alone(frame: &Container) -> Container {
    Container {
        inner_dimensions: frame.declared_inner(),
        deck: None,
        ..frame.clone()
    }
}

/// A placement moved by `(dx, dy)` on `frame`; a floor placement's support, its overlap
/// with the deck, is measured again. `O(1)`.
pub(crate) fn moved(frame: &Container, placement: &Placement, dx: i64, dy: i64) -> Placement {
    let shift = |point: Point| Point {
        x: point.x + dx,
        y: point.y + dy,
        z: point.z,
    };
    let mut moved = Placement {
        position: shift(placement.position),
        envelope_origin: shift(placement.envelope_origin),
        ..placement.clone()
    };
    if moved.envelope_origin.z == 0 {
        moved.support_ratio = floor_support(frame.deck, moved.envelope_box()).1;
    }
    moved
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aabb(x: i64, y: i64, length: i64, width: i64) -> Aabb {
        Aabb {
            origin: Point { x, y, z: 0 },
            dimensions: Dimensions {
                length: Length(length),
                width: Length(width),
                height: Length(5),
            },
        }
    }

    const DECK: Deck = Deck {
        x: 5,
        y: 5,
        length: 100,
        width: 80,
    };

    #[test]
    fn contact_is_the_overlap_with_the_deck_and_zero_off_it() {
        assert_eq!(DECK.contact_area(aabb(0, 0, 10, 10)), 25);
        assert_eq!(DECK.contact_area(aabb(105, 0, 5, 10)), 0);
        assert_eq!(floor_support(None, aabb(0, 0, 10, 10)), (100, 1.0));
        assert_eq!(floor_support(Some(DECK), aabb(0, 0, 10, 10)), (25, 0.25));
    }

    #[test]
    fn the_deck_refuses_short_support_then_an_unsupported_centroid() {
        assert_eq!(DECK.refusal(aabb(0, 0, 10, 10), 0.0), None);
        assert_eq!(
            DECK.refusal(aabb(0, 0, 10, 10), 0.5),
            Some("insufficient_support")
        );
        // Half the base on the deck, centroid on its edge: carried.
        assert_eq!(DECK.refusal(aabb(0, 5, 10, 10), 0.5), None);
        // A sliver of support passes a tiny ratio but leaves the centroid off the deck.
        assert_eq!(
            DECK.refusal(aabb(0, 5, 8, 10), 0.01),
            Some("centre_of_gravity_unsupported")
        );
    }

    #[test]
    fn the_seat_is_flush_when_the_load_fits_and_centred_when_it_does_not() {
        assert_eq!(seat(5, 100, 0, 90), 5);
        assert_eq!(seat(5, 100, 0, 101), 5);
        assert_eq!(seat(5, 100, 0, 103), 4);
        assert_eq!(seat(5, 100, 3, 103), 2);
    }
}
