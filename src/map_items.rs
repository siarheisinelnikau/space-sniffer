//! Bound the number of rectangles passed to the treemap without losing bytes.

use std::{cmp::Reverse, collections::BinaryHeap};

pub const COLLAPSED_ID: usize = usize::MAX;

#[derive(Debug, Default)]
pub struct MapItems {
    pub items: Vec<(usize, u64)>,
    pub collapsed_count: usize,
    pub collapsed_bytes: u64,
}

/// Select the largest drawable children and combine the rest into one tile.
///
/// Zero-weight and invalid child indices are ignored. The rectangle limit
/// includes the collapsed tile. With a zero limit, all bytes and entry counts
/// are available in the collapsed fields, but `items` is empty.
/// Output is sorted by descending weight and then ascending ID, ready for the
/// treemap's linear path without another sort.
///
/// Children smaller than four square pixels, or a quarter of an average tile
/// at the requested limit, are collapsed before touching the bounded heap.
/// This makes large groups of tiny children a pair of linear passes rather
/// than a full sort. Ties favor the lower child ID, independent of input order.
pub fn visible_items(
    children: &[usize],
    weights: &[u64],
    pixel_area: f32,
    limit: usize,
) -> MapItems {
    let mut total = 0_u64;
    for &id in children {
        let weight = weights.get(id).copied().unwrap_or(0);
        total = total.saturating_add(weight);
    }
    if total == 0 {
        return MapItems::default();
    }
    visible_items_with_total(children, weights, total, pixel_area, limit)
}

/// Select visible children in one pass when the parent's visible byte count is
/// already known. `total` should be the saturating sum of the child weights,
/// including the active filter. It is used only for the pixel cutoff: omitted
/// counts and bytes are always calculated from the actual children, including
/// when their total saturates at `u64::MAX`.
///
/// This avoids re-summing a million siblings each time a preview invalidates
/// the map. At most `limit` output rectangles and heap entries are allocated.
pub fn visible_items_with_total(
    children: &[usize],
    weights: &[u64],
    total: u64,
    pixel_area: f32,
    limit: usize,
) -> MapItems {
    if total == 0 {
        return MapItems::default();
    }
    let drawable = limit != 0 && pixel_area.is_finite() && pixel_area > 0.0;
    if !drawable {
        let mut count = 0;
        let mut bytes = 0_u64;
        for &id in children {
            let weight = weights.get(id).copied().unwrap_or(0);
            if weight != 0 {
                count += 1;
                bytes = bytes.saturating_add(weight);
            }
        }
        return if count == 0 {
            MapItems::default()
        } else {
            collapsed(count, bytes, limit != 0)
        };
    }

    // Move the floating-point work out of the sibling loop. The adaptive
    // cutoff allows roughly four times the rectangle budget to reach the heap;
    // all smaller entries only need an integer load/add and one comparison.
    let minimum_weight =
        (4.0 / f64::from(pixel_area)).max(1.0 / (limit as f64 * 4.0)) * total as f64;
    // The smallest retained weight (and largest ID for ties) is at the top.
    let mut largest: BinaryHeap<Reverse<(u64, Reverse<usize>)>> = BinaryHeap::new();
    let mut count = 0;
    let mut collapsed_bytes = 0_u64;

    for &id in children {
        let weight = weights.get(id).copied().unwrap_or(0);
        if weight == 0 {
            continue;
        }
        count += 1;
        if (weight as f64) < minimum_weight {
            collapsed_bytes = collapsed_bytes.saturating_add(weight);
            continue;
        }

        let candidate = (weight, Reverse(id));
        if largest.capacity() == 0 {
            // Allocate only if something survives LOD. Small drawable groups
            // need one allocation instead of repeated heap growth; huge all-
            // tiny groups need none. Cap speculative allocation for callers
            // requesting an unusually large rectangle budget.
            largest.reserve_exact(limit.min(children.len()).min(1024));
        }
        if largest.len() < limit {
            largest.push(Reverse(candidate));
        } else if candidate > largest.peek().unwrap().0 {
            let removed = largest.pop().unwrap().0;
            collapsed_bytes = collapsed_bytes.saturating_add(removed.0);
            largest.push(Reverse(candidate));
        } else {
            collapsed_bytes = collapsed_bytes.saturating_add(weight);
        }
    }

    // Make room for the aggregate only if something was actually omitted.
    if count > largest.len() && largest.len() == limit {
        let removed = largest.pop().unwrap().0;
        collapsed_bytes = collapsed_bytes.saturating_add(removed.0);
    }
    let collapsed_count = count - largest.len();
    let mut items: Vec<_> = largest
        .into_iter()
        .map(|Reverse((weight, Reverse(id)))| (id, weight))
        .collect();
    if collapsed_count != 0 {
        items.push((COLLAPSED_ID, collapsed_bytes));
    }
    items.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    MapItems {
        items,
        collapsed_count,
        collapsed_bytes,
    }
}

fn collapsed(count: usize, bytes: u64, drawable: bool) -> MapItems {
    MapItems {
        items: if drawable {
            vec![(COLLAPSED_ID, bytes)]
        } else {
            Vec::new()
        },
        collapsed_count: count,
        collapsed_bytes: bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::{COLLAPSED_ID, visible_items, visible_items_with_total};
    use std::time::Instant;

    fn sum(items: &[(usize, u64)]) -> u64 {
        items
            .iter()
            .fold(0_u64, |total, (_, weight)| total.saturating_add(*weight))
    }

    #[test]
    fn unsorted_children_keep_the_largest_and_preserve_all_bytes() {
        let weights = [50, 10, 40, 20, 60, 30, 0];
        let result = visible_items(&[1, 5, 3, 6, 0, 2, 4], &weights, 1_000_000.0, 4);
        assert_eq!(
            result.items,
            [(4, 60), (COLLAPSED_ID, 60), (0, 50), (2, 40)]
        );
        assert_eq!(result.collapsed_count, 3);
        assert_eq!(result.collapsed_bytes, 60);
        assert_eq!(sum(&result.items), 210);
    }

    #[test]
    fn all_items_fit_without_an_unnecessary_collapsed_tile() {
        let result = visible_items(&[2, 1, 0], &[7, 19, 11], 10000.0, 3);
        assert_eq!(result.items, [(1, 19), (2, 11), (0, 7)]);
        assert_eq!(result.collapsed_count, 0);
        assert_eq!(result.collapsed_bytes, 0);
    }

    #[test]
    fn equal_weights_use_ids_to_break_ties_independently_of_source_order() {
        let first = visible_items(&[5, 1, 4, 0, 3, 2], &[10; 6], 10000.0, 4);
        let second = visible_items(&[2, 3, 0, 4, 1, 5], &[10; 6], 10000.0, 4);
        assert_eq!(first.items, [(COLLAPSED_ID, 30), (0, 10), (1, 10), (2, 10)]);
        assert_eq!(first.items, second.items);
    }

    #[test]
    fn zero_limit_reports_omitted_bytes_without_rectangles() {
        let result = visible_items(&[0, 1, 2], &[7, 0, 11], 10000.0, 0);
        assert!(result.items.is_empty());
        assert_eq!(result.collapsed_count, 2);
        assert_eq!(result.collapsed_bytes, 18);
    }

    #[test]
    fn one_slot_is_a_complete_aggregate_or_the_only_real_item() {
        let result = visible_items(&[0, 1], &[7, 11], 10000.0, 1);
        assert_eq!(result.items, [(COLLAPSED_ID, 18)]);
        assert_eq!(result.collapsed_count, 2);
        let single = visible_items(&[0], &[7], 10000.0, 1);
        assert_eq!(single.items, [(0, 7)]);
        assert_eq!(single.collapsed_count, 0);
    }

    #[test]
    fn subpixel_children_are_aggregated_even_with_spare_slots() {
        let result = visible_items(&[0, 1, 2, 3], &[1000, 1, 1, 1], 1000.0, 32);
        assert_eq!(result.items, [(0, 1000), (COLLAPSED_ID, 3)]);
        assert_eq!(result.collapsed_count, 3);
        assert_eq!(sum(&result.items), 1003);
    }

    #[test]
    fn zero_weights_bad_indices_and_empty_inputs_are_ignored() {
        assert!(visible_items(&[], &[1], 1000.0, 10).items.is_empty());
        assert!(visible_items(&[0, 99], &[0], 1000.0, 10).items.is_empty());
        let result = visible_items(&[0, 1, 99], &[0, 17], 1000.0, 10);
        assert_eq!(result.items, [(1, 17)]);
        assert_eq!(result.collapsed_count, 0);
    }

    #[test]
    fn bad_pixel_areas_collapse_safely_without_losing_bytes() {
        for area in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            let result = visible_items(&[0, 1], &[3, 5], area, 10);
            assert_eq!(result.items, [(COLLAPSED_ID, 8)]);
            assert_eq!(result.collapsed_count, 2);
        }
    }

    #[test]
    fn overflowing_byte_totals_saturate_like_the_model() {
        let weights = [u64::MAX, u64::MAX, 10, 0];
        let result = visible_items(&[0, 1, 2, 3], &weights, 10000.0, 2);
        assert!(result.items.len() <= 2);
        assert_eq!(sum(&result.items), u64::MAX);
        assert_eq!(result.collapsed_count, 2);
        assert_eq!(result.collapsed_bytes, u64::MAX);
    }

    #[test]
    fn every_limit_preserves_coverage_for_a_large_unsorted_group() {
        let weights: Vec<_> = (0..4096)
            .map(|id| ((id * 3571) % 9999 + 1) as u64)
            .collect();
        let children: Vec<_> = (0..weights.len()).rev().collect();
        let total: u64 = weights.iter().sum();
        for limit in [1, 2, 3, 16, 128, 1024, 4096, 5000] {
            let result = visible_items(&children, &weights, 2_000_000.0, limit);
            assert!(result.items.len() <= limit);
            assert_eq!(sum(&result.items), total);
            assert_eq!(
                result
                    .items
                    .iter()
                    .filter(|(id, _)| *id != COLLAPSED_ID)
                    .count()
                    + result.collapsed_count,
                weights.len()
            );
        }
    }

    #[test]
    fn million_equal_tiny_siblings_use_one_complete_aggregate() {
        let children: Vec<_> = (0..1_000_000).collect();
        let weights = vec![1; children.len()];
        let started = Instant::now();
        let result = visible_items(&children, &weights, 2_000_000.0, 1024);
        let elapsed = started.elapsed();
        assert_eq!(result.items, [(COLLAPSED_ID, 1_000_000)]);
        assert_eq!(result.collapsed_count, 1_000_000);
        println!(
            "million equal siblings: {:.3} ms",
            elapsed.as_secs_f64() * 1000.0
        );
        // No machine-speed assertion: rectangle count and complete accounting
        // are the invariant, and the printed timing can be inspected directly.
    }

    #[test]
    fn known_parent_total_matches_the_general_path_for_filtered_unsorted_weights() {
        let weights: Vec<_> = (0..10_000)
            .map(|id| {
                if id % 7 == 0 {
                    0
                } else {
                    ((id * 997) % 9999 + 1) as u64
                }
            })
            .collect();
        let children: Vec<_> = (0..weights.len()).rev().collect();
        let total = weights
            .iter()
            .fold(0_u64, |sum, weight| sum.saturating_add(*weight));
        for area in [0.0, 1.0, 1000.0, 2_000_000.0, f32::NAN] {
            for limit in [0, 1, 2, 16, 1024, 20_000] {
                let general = visible_items(&children, &weights, area, limit);
                let known = visible_items_with_total(&children, &weights, total, area, limit);
                assert_eq!(known.items, general.items);
                assert_eq!(known.collapsed_count, general.collapsed_count);
                assert_eq!(known.collapsed_bytes, general.collapsed_bytes);
            }
        }
    }

    #[test]
    fn known_saturated_total_preserves_every_omitted_weight() {
        let weights = [u64::MAX, u64::MAX, u64::MAX / 2, 11, 0];
        for limit in [0, 1, 2, 3, 10] {
            let result =
                visible_items_with_total(&[4, 3, 2, 1, 0], &weights, u64::MAX, 2_000_000.0, limit);
            if limit != 0 {
                assert_eq!(sum(&result.items), u64::MAX);
                assert!(result.items.len() <= limit);
            }
            let selected: Vec<_> = result
                .items
                .iter()
                .filter(|(id, _)| *id != COLLAPSED_ID)
                .map(|(id, _)| *id)
                .collect();
            let omitted = weights
                .iter()
                .enumerate()
                .filter(|(id, weight)| **weight != 0 && !selected.contains(id));
            let expected = omitted.fold(0_u64, |sum, (_, weight)| sum.saturating_add(*weight));
            assert_eq!(result.collapsed_bytes, expected);
            assert_eq!(result.collapsed_count + selected.len(), 4);
        }
    }

    #[test]
    fn output_is_already_in_the_geometry_sort_order() {
        let result = visible_items_with_total(&[4, 3, 2, 1, 0], &[9, 3, 7, 1, 20], 40, 10000.0, 4);
        assert!(
            result.items.windows(2).all(
                |pair| pair[0].1 > pair[1].1 || pair[0].1 == pair[1].1 && pair[0].0 < pair[1].0
            )
        );
        assert_eq!(sum(&result.items), 40);
    }
}
