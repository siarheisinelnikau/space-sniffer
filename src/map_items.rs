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
    let mut count = 0;
    for &id in children {
        let weight = weights.get(id).copied().unwrap_or(0);
        if weight != 0 {
            count += 1;
            total = total.saturating_add(weight);
        }
    }
    if count == 0 {
        return MapItems::default();
    }
    if limit == 0 || !pixel_area.is_finite() || pixel_area <= 0.0 {
        return collapsed(count, total, limit != 0);
    }

    let pixel_area = f64::from(pixel_area);
    let minimum_area = 4.0_f64.max(pixel_area / (limit as f64 * 4.0));
    // The smallest retained weight (and largest ID for ties) is at the top.
    let mut largest: BinaryHeap<Reverse<(u64, Reverse<usize>)>> =
        BinaryHeap::with_capacity(limit.min(count));
    let mut collapsed_bytes = 0_u64;

    for &id in children {
        let weight = weights.get(id).copied().unwrap_or(0);
        if weight == 0 {
            continue;
        }
        let area = weight as f64 / total as f64 * pixel_area;
        if area < minimum_area {
            collapsed_bytes = collapsed_bytes.saturating_add(weight);
            continue;
        }

        let candidate = (weight, Reverse(id));
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
    items.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    if collapsed_count != 0 {
        items.push((COLLAPSED_ID, collapsed_bytes));
    }
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
    use super::{COLLAPSED_ID, visible_items};
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
            [(4, 60), (0, 50), (2, 40), (COLLAPSED_ID, 60)]
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
        assert_eq!(first.items, [(0, 10), (1, 10), (2, 10), (COLLAPSED_ID, 30)]);
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
}
