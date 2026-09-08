//! Squarified treemap geometry, independent of the UI.
//!
//! Calculations use `f64` so even a collection of `u64::MAX` weights cannot
//! overflow. The final edges are shared and rounded inward where necessary;
//! rectangles stay disjoint even when a very small item becomes subpixel.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct Tile {
    pub id: usize,
    pub rect: Rect,
}

#[derive(Clone, Copy)]
struct Item {
    id: usize,
    weight: u64,
}

/// Lay out nonzero items with area proportional to their weight.
///
/// Tiles are returned in descending weight order, with ascending IDs breaking
/// ties. Empty input and invalid, empty, or unrepresentable bounds produce no
/// tiles. At extreme coordinate scales or weight ratios, `f32` precision can
/// reduce a tile to zero width or height; its ID is still returned.
pub fn layout(items: &[(usize, u64)], bounds: Rect) -> Vec<Tile> {
    if !bounds.x.is_finite()
        || !bounds.y.is_finite()
        || !bounds.width.is_finite()
        || !bounds.height.is_finite()
        || bounds.width <= 0.0
        || bounds.height <= 0.0
        || !(bounds.x + bounds.width).is_finite()
        || !(bounds.y + bounds.height).is_finite()
    {
        return Vec::new();
    }

    let mut left = f64::from(bounds.x);
    let mut top = f64::from(bounds.y);
    let right = left + f64::from(bounds.width);
    let bottom = top + f64::from(bounds.height);
    if right <= left || bottom <= top {
        return Vec::new();
    }

    let mut sorted: Vec<Item> = items
        .iter()
        .filter(|(_, weight)| *weight != 0)
        .map(|&(id, weight)| Item { id, weight })
        .collect();
    sorted.sort_unstable_by(|a, b| b.weight.cmp(&a.weight).then(a.id.cmp(&b.id)));
    if sorted.is_empty() {
        return Vec::new();
    }

    // Suffix totals avoid subtracting a large row from the total and losing
    // the small items that remain. Adding from smallest to largest also keeps
    // the total as accurate as possible without an integer overflow.
    let mut remaining = vec![0.0; sorted.len() + 1];
    for index in (0..sorted.len()).rev() {
        remaining[index] = remaining[index + 1] + sorted[index].weight as f64;
    }
    let area_per_weight = (right - left) * (bottom - top) / remaining[0];
    let clip = Clip {
        left: bounds.x,
        top: bounds.y,
        right: floor_f32(right),
        bottom: floor_f32(bottom),
    };
    let mut tiles = Vec::with_capacity(sorted.len());
    let mut start = 0;

    while start < sorted.len() {
        let width = (right - left).max(0.0);
        let height = (bottom - top).max(0.0);
        let side = width.min(height);
        if side == 0.0 {
            // A previous strip can consume all representable space when its
            // weight is overwhelmingly larger than the remaining weights.
            for item in &sorted[start..] {
                tiles.push(Tile {
                    id: item.id,
                    rect: clip.rect(left, top, left, top),
                });
            }
            break;
        }

        let mut end = start + 1;
        let largest = sorted[start].weight as f64;
        let mut sum = largest;
        let mut score = worst_ratio(sum, largest, largest, side, area_per_weight);
        while end < sorted.len() {
            let smallest = sorted[end].weight as f64;
            let candidate_sum = sum + smallest;
            let candidate = worst_ratio(candidate_sum, largest, smallest, side, area_per_weight);
            if candidate > score {
                break;
            }
            sum = candidate_sum;
            score = candidate;
            end += 1;
        }

        let fraction = (sum / remaining[start]).clamp(0.0, 1.0);
        let mut prefix = 0.0;
        if width >= height {
            // A vertical strip spans the shorter (vertical) edge.
            let strip_right = if end == sorted.len() {
                right
            } else {
                (left + width * fraction).clamp(left, right)
            };
            let mut row_top = top;
            for (index, item) in sorted.iter().enumerate().take(end).skip(start) {
                prefix += item.weight as f64;
                let row_bottom = if index + 1 == end {
                    bottom
                } else {
                    (top + height * (prefix / sum)).clamp(row_top, bottom)
                };
                tiles.push(Tile {
                    id: item.id,
                    rect: clip.rect(left, row_top, strip_right, row_bottom),
                });
                row_top = row_bottom;
            }
            left = strip_right;
        } else {
            // A horizontal strip spans the shorter (horizontal) edge.
            let strip_bottom = if end == sorted.len() {
                bottom
            } else {
                (top + height * fraction).clamp(top, bottom)
            };
            let mut column_left = left;
            for (index, item) in sorted.iter().enumerate().take(end).skip(start) {
                prefix += item.weight as f64;
                let column_right = if index + 1 == end {
                    right
                } else {
                    (left + width * (prefix / sum)).clamp(column_left, right)
                };
                tiles.push(Tile {
                    id: item.id,
                    rect: clip.rect(column_left, top, column_right, strip_bottom),
                });
                column_left = column_right;
            }
            top = strip_bottom;
        }
        start = end;
    }

    tiles
}

// This is the squarify worst-aspect-ratio criterion, expressed using weights
// instead of allocating another array of normalized areas.
fn worst_ratio(sum: f64, largest: f64, smallest: f64, side: f64, scale: f64) -> f64 {
    let side_in_weights = side * side / scale;
    let sum_squared = sum * sum;
    (side_in_weights * largest / sum_squared).max(sum_squared / (side_in_weights * smallest))
}

struct Clip {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
}

impl Clip {
    fn rect(&self, left: f64, top: f64, right: f64, bottom: f64) -> Rect {
        let left = (left as f32).clamp(self.left, self.right);
        let top = (top as f32).clamp(self.top, self.bottom);
        let right = (right as f32).clamp(left, self.right);
        let bottom = (bottom as f32).clamp(top, self.bottom);
        Rect {
            x: left,
            y: top,
            width: floor_f32(f64::from(right) - f64::from(left)),
            height: floor_f32(f64::from(bottom) - f64::from(top)),
        }
    }
}

// Rounding a length upward could make neighboring rectangles overlap. One
// adjacent float is enough to guarantee an inward result, including subnormals.
fn floor_f32(value: f64) -> f32 {
    let rounded = value as f32;
    if f64::from(rounded) > value {
        rounded.next_down()
    } else {
        rounded
    }
}

#[cfg(test)]
mod tests {
    use super::{Rect, Tile, layout};

    fn bounds() -> Rect {
        Rect {
            x: 13.5,
            y: 27.25,
            width: 800.0,
            height: 500.0,
        }
    }

    fn area(rect: Rect) -> f64 {
        f64::from(rect.width) * f64::from(rect.height)
    }

    fn assert_geometry(tiles: &[Tile], bounds: Rect) {
        let far_right = f64::from(bounds.x) + f64::from(bounds.width);
        let far_bottom = f64::from(bounds.y) + f64::from(bounds.height);
        for tile in tiles {
            let rect = tile.rect;
            assert!(rect.x.is_finite() && rect.y.is_finite(), "{tile:?}");
            assert!(
                rect.width.is_finite() && rect.height.is_finite(),
                "{tile:?}"
            );
            assert!(rect.width >= 0.0 && rect.height >= 0.0, "{tile:?}");
            assert!(rect.x >= bounds.x && rect.y >= bounds.y, "{tile:?}");
            assert!(
                f64::from(rect.x) + f64::from(rect.width) <= far_right,
                "{tile:?}"
            );
            assert!(
                f64::from(rect.y) + f64::from(rect.height) <= far_bottom,
                "{tile:?}"
            );
        }
        for (index, a) in tiles.iter().enumerate() {
            if a.rect.width == 0.0 || a.rect.height == 0.0 {
                continue;
            }
            for b in &tiles[index + 1..] {
                if b.rect.width == 0.0 || b.rect.height == 0.0 {
                    continue;
                }
                let separated = f64::from(a.rect.x) + f64::from(a.rect.width)
                    <= f64::from(b.rect.x)
                    || f64::from(b.rect.x) + f64::from(b.rect.width) <= f64::from(a.rect.x)
                    || f64::from(a.rect.y) + f64::from(a.rect.height) <= f64::from(b.rect.y)
                    || f64::from(b.rect.y) + f64::from(b.rect.height) <= f64::from(a.rect.y);
                assert!(separated, "overlapping rectangles: {a:?}, {b:?}");
            }
        }
    }

    fn assert_proportions(items: &[(usize, u64)], tiles: &[Tile], bounds: Rect, tolerance: f64) {
        let total: u128 = items.iter().map(|(_, weight)| u128::from(*weight)).sum();
        let total_area = area(bounds);
        let covered: f64 = tiles.iter().map(|tile| area(tile.rect)).sum();
        assert!((covered / total_area - 1.0).abs() <= tolerance);
        for tile in tiles {
            let weight = items.iter().find(|(id, _)| *id == tile.id).unwrap().1;
            let expected = total_area * weight as f64 / total as f64;
            assert!(
                (area(tile.rect) / expected - 1.0).abs() <= tolerance,
                "tile {} has area {}, expected {expected}",
                tile.id,
                area(tile.rect)
            );
        }
    }

    #[test]
    fn preserves_ids_sorts_deterministically_and_ignores_zero_weights() {
        let items = [(42, 3), (9, 5), (7, 0), (2, 5), (100, 1)];
        let tiles = layout(&items, bounds());
        assert_eq!(
            tiles.iter().map(|tile| tile.id).collect::<Vec<_>>(),
            [2, 9, 42, 100]
        );
        assert_geometry(&tiles, bounds());
        assert_proportions(&items, &tiles, bounds(), 0.00001);
    }

    #[test]
    fn four_equal_items_are_squares() {
        let bounds = Rect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        };
        let tiles = layout(&[(0, 1), (1, 1), (2, 1), (3, 1)], bounds);
        assert_eq!(tiles.len(), 4);
        for tile in &tiles {
            assert_eq!(tile.rect.width, 50.0);
            assert_eq!(tile.rect.height, 50.0);
        }
        assert_geometry(&tiles, bounds);
    }

    #[test]
    fn one_item_fills_the_bounds() {
        let tiles = layout(&[(71, u64::MAX)], bounds());
        assert_eq!(tiles.len(), 1);
        assert_eq!(tiles[0].id, 71);
        assert_eq!(tiles[0].rect, bounds());
    }

    #[test]
    fn maximum_integer_weights_do_not_overflow() {
        let items = [
            (0, u64::MAX),
            (1, u64::MAX),
            (2, u64::MAX / 2),
            (3, u64::MAX / 4),
        ];
        let tiles = layout(&items, bounds());
        assert_geometry(&tiles, bounds());
        assert_proportions(&items, &tiles, bounds(), 0.00001);
    }

    #[test]
    fn invalid_or_empty_inputs_produce_no_tiles() {
        assert!(layout(&[], bounds()).is_empty());
        assert!(layout(&[(0, 0)], bounds()).is_empty());
        for bad in [
            Rect {
                width: 0.0,
                ..bounds()
            },
            Rect {
                height: -1.0,
                ..bounds()
            },
            Rect {
                x: f32::NAN,
                ..bounds()
            },
            Rect {
                y: f32::INFINITY,
                ..bounds()
            },
            Rect {
                width: f32::INFINITY,
                ..bounds()
            },
            Rect {
                height: f32::NAN,
                ..bounds()
            },
            Rect {
                x: f32::MAX,
                width: f32::MAX,
                ..bounds()
            },
        ] {
            assert!(layout(&[(0, 1)], bad).is_empty(), "{bad:?}");
        }
    }

    #[test]
    fn thin_bounds_work_in_both_orientations() {
        let items = [(0, 13), (1, 8), (2, 5), (3, 3), (4, 2), (5, 1)];
        for bounds in [
            Rect {
                x: 0.0,
                y: -100.0,
                width: 0.00001,
                height: 1_000_000.0,
            },
            Rect {
                x: -100.0,
                y: 0.0,
                width: 1_000_000.0,
                height: 0.00001,
            },
        ] {
            let tiles = layout(&items, bounds);
            assert_geometry(&tiles, bounds);
            assert_proportions(&items, &tiles, bounds, 0.00001);
        }
    }

    #[test]
    fn extreme_weight_ratios_and_coordinate_scales_stay_finite_and_disjoint() {
        let items = [(0, u64::MAX), (1, 2), (2, 1)];
        for bounds in [
            bounds(),
            Rect {
                x: -1.0e30,
                y: -1.0e30,
                width: 2.0e30,
                height: 2.0e30,
            },
            Rect {
                x: 0.0,
                y: 0.0,
                width: f32::MAX,
                height: f32::MIN_POSITIVE,
            },
            Rect {
                x: 0.0,
                y: 0.0,
                width: f32::from_bits(1),
                height: f32::from_bits(1),
            },
        ] {
            let tiles = layout(&items, bounds);
            assert_eq!(tiles.len(), items.len());
            assert_geometry(&tiles, bounds);
        }
    }

    #[test]
    fn many_items_cover_bounds_with_proportional_nonoverlapping_areas() {
        let mut state = 0x3857_aec9_7213_0fed_u64;
        let items: Vec<_> = (0..1024)
            .map(|id| {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                (id, state % 1_000_000 + 50_000)
            })
            .collect();
        let bounds = Rect {
            x: -307.125,
            y: -190.25,
            width: 1920.0,
            height: 1080.0,
        };
        let tiles = layout(&items, bounds);
        assert_eq!(tiles.len(), items.len());
        assert_geometry(&tiles, bounds);
        assert_proportions(&items, &tiles, bounds, 0.0001);
    }
}
