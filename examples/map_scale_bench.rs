//! CPU-only, synthetic rendering preparation benchmarks; no disk scans.
//!
//! `cargo run --release --example map_scale_bench`
//! Add `--raw` to also measure deliberately unbounded raw geometry calls.
//! The frozen v0.3.1 selector provides a same-process baseline. Shape cases
//! include many retained rectangles, nested directories, skew, and filtering.

use space_sniffer::{
    map_items::{COLLAPSED_ID, MapItems, visible_items, visible_items_with_total},
    treemap::{self, Rect},
};
use std::{cmp::Reverse, collections::BinaryHeap, hint::black_box, time::Instant};

const AREA: f32 = 1920.0 * 1080.0;
const LIMIT: usize = 1024;

fn main() {
    for count in [100_000, 1_000_000] {
        let children: Vec<_> = (0..count).rev().collect();
        for scenario in ["tiny", "mixed", "filtered"] {
            let weights: Vec<_> = (0..count)
                .map(|id| match scenario {
                    "tiny" => 1,
                    "mixed" if id % (count / 64) == 0 => 1_000_000_000 + id as u64,
                    "mixed" => 1,
                    _ if id % 1000 == 0 => 1_000_000,
                    _ => 0,
                })
                .collect();
            let total = weights
                .iter()
                .fold(0_u64, |sum, weight| sum.saturating_add(*weight));
            let baseline = baseline_visible_items(&children, &weights, AREA, LIMIT);
            let current = visible_items_with_total(&children, &weights, total, AREA, LIMIT);
            assert_equivalent(&baseline, &current);
            let before = time(31, || {
                baseline_visible_items(&children, &weights, AREA, LIMIT)
            });
            let general = time(31, || visible_items(&children, &weights, AREA, LIMIT));
            let known = time(31, || {
                visible_items_with_total(&children, &weights, total, AREA, LIMIT)
            });
            let geometry = time(101, || treemap::layout(&current.items, screen()));
            println!(
                "siblings={count} scenario={scenario} rectangles={} collapsed={} baseline_selector_us={:.3} general_selector_us={:.3} known_total_selector_us={:.3} known_total_p95_us={:.3} geometry_us={:.3}",
                current.items.len(),
                current.collapsed_count,
                before.0,
                general.0,
                known.0,
                known.1,
                geometry.0
            );
        }
    }

    for (name, tree) in [
        ("broad_equal", Fixture::broad(64, 128, false)),
        ("broad_skewed", Fixture::broad(128, 512, true)),
        ("nested_filtered", Fixture::nested()),
    ] {
        let baseline = build_geometry(&tree, false);
        let current = build_geometry(&tree, true);
        assert_eq!(baseline.rectangles, current.rectangles);
        assert!(current.rectangles <= 8000);
        let before = time(21, || build_geometry(&tree, false));
        let after = time(21, || build_geometry(&tree, true));
        println!(
            "shape={name} nodes={} rectangles={} laid_out_groups={} examined_siblings={} baseline_preparation_us={:.3} known_total_preparation_us={:.3} known_total_p95_us={:.3}",
            tree.weights.len(),
            current.rectangles,
            current.groups,
            current.siblings,
            before.0,
            after.0,
            after.1
        );
    }

    if std::env::args().any(|argument| argument == "--raw") {
        for count in [100_000, 1_000_000] {
            let mut state = 0x473a_7861_834e_6153_u64;
            let mut items: Vec<_> = (0..count)
                .map(|id| {
                    state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                    (id, state % 1_000_000 + 1)
                })
                .collect();
            let unsorted = time(5, || treemap::layout(&items, screen()));
            items.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            let sorted = time(5, || treemap::layout(&items, screen()));
            println!(
                "raw_layout rectangles={count} unsorted_us={:.3} sorted_us={:.3} ui_uses_lod=true",
                unsorted.0, sorted.0
            );
        }
    }
}

fn screen() -> Rect {
    Rect {
        x: 0.0,
        y: 0.0,
        width: 1920.0,
        height: 1080.0,
    }
}

fn time<R>(samples: usize, mut run: impl FnMut() -> R) -> (f64, f64) {
    for _ in 0..3 {
        black_box(run());
    }
    let mut timings = Vec::with_capacity(samples);
    for _ in 0..samples {
        let started = Instant::now();
        black_box(run());
        timings.push(started.elapsed());
    }
    timings.sort_unstable();
    (
        timings[samples / 2].as_secs_f64() * 1_000_000.0,
        timings[(samples * 95).div_ceil(100) - 1].as_secs_f64() * 1_000_000.0,
    )
}

fn assert_equivalent(before: &MapItems, after: &MapItems) {
    let mut before_items = before.items.clone();
    before_items.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    assert_eq!(before_items, after.items);
    assert_eq!(before.collapsed_count, after.collapsed_count);
    assert_eq!(before.collapsed_bytes, after.collapsed_bytes);
}

struct Fixture {
    children: Vec<Vec<usize>>,
    weights: Vec<u64>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            children: vec![Vec::new()],
            weights: vec![0],
        }
    }

    fn node(&mut self, parent: usize, weight: u64) -> usize {
        let id = self.weights.len();
        self.children.push(Vec::new());
        self.weights.push(weight);
        self.children[parent].push(id);
        id
    }

    fn aggregate(&mut self) {
        for id in (0..self.weights.len()).rev() {
            if !self.children[id].is_empty() {
                self.weights[id] = self.children[id]
                    .iter()
                    .fold(0_u64, |sum, child| sum.saturating_add(self.weights[*child]));
            }
        }
    }

    fn broad(directories: usize, files: usize, skewed: bool) -> Self {
        let mut fixture = Self::new();
        for directory in 0..directories {
            let parent = fixture.node(0, 0);
            for file in 0..files {
                let weight = if skewed {
                    1_000_000 / (file + 1) as u64 * (directory % 8 + 1) as u64
                } else {
                    1_048_576
                };
                fixture.node(parent, weight);
            }
        }
        fixture.aggregate();
        fixture
    }

    fn nested() -> Self {
        let mut fixture = Self::new();
        for outer in 0..16 {
            let directory = fixture.node(0, 0);
            for inner in 0..16 {
                let parent = fixture.node(directory, 0);
                for file in 0..256 {
                    let weight = if (file + outer + inner) % 5 == 0 {
                        0
                    } else {
                        4096 + file as u64 * 31
                    };
                    fixture.node(parent, weight);
                }
            }
        }
        fixture.aggregate();
        fixture
    }
}

struct GeometryStats {
    rectangles: usize,
    groups: usize,
    siblings: usize,
}

fn build_geometry(tree: &Fixture, known_total: bool) -> GeometryStats {
    let mut pending = vec![(Some(0), screen(), 6)];
    let mut stats = GeometryStats {
        rectangles: 0,
        groups: 0,
        siblings: 0,
    };
    while let Some((id, rect, depth)) = pending.pop() {
        stats.rectangles += 1;
        let Some(id) = id else {
            continue;
        };
        if depth == 0 || rect.width <= 60.0 || rect.height <= 44.0 || tree.children[id].is_empty() {
            continue;
        }
        // Reserve every pending sibling's rectangle before entering children.
        let limit = LIMIT.min(8000 - stats.rectangles - pending.len());
        if limit == 0 {
            continue;
        }
        let inner = Rect {
            x: rect.x + 3.0,
            y: rect.y + 20.0,
            width: rect.width - 7.0,
            height: rect.height - 24.0,
        };
        let items = if known_total {
            visible_items_with_total(
                &tree.children[id],
                &tree.weights,
                tree.weights[id],
                inner.width * inner.height,
                limit,
            )
        } else {
            baseline_visible_items(
                &tree.children[id],
                &tree.weights,
                inner.width * inner.height,
                limit,
            )
        };
        stats.groups += 1;
        stats.siblings += tree.children[id].len();
        let tiles = treemap::layout(&items.items, inner);
        for tile in tiles.into_iter().rev() {
            pending.push((
                (tile.id != COLLAPSED_ID).then_some(tile.id),
                tile.rect,
                depth - 1,
            ));
        }
    }
    stats
}

// Frozen v0.3.1 implementation: two child passes, per-child division, eager
// heap allocation, and the aggregate appended after the selected-item sort.
fn baseline_visible_items(
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
        return MapItems {
            items: if limit == 0 {
                Vec::new()
            } else {
                vec![(COLLAPSED_ID, total)]
            },
            collapsed_count: count,
            collapsed_bytes: total,
        };
    }
    let pixel_area = f64::from(pixel_area);
    let minimum_area = 4.0_f64.max(pixel_area / (limit as f64 * 4.0));
    let mut largest: BinaryHeap<Reverse<(u64, Reverse<usize>)>> =
        BinaryHeap::with_capacity(limit.min(count));
    let mut collapsed_bytes = 0_u64;
    for &id in children {
        let weight = weights.get(id).copied().unwrap_or(0);
        if weight == 0 {
            continue;
        }
        if (weight as f64 / total as f64 * pixel_area) < minimum_area {
            collapsed_bytes = collapsed_bytes.saturating_add(weight);
            continue;
        }
        let candidate = (weight, Reverse(id));
        if largest.len() < limit {
            largest.push(Reverse(candidate));
        } else if candidate > largest.peek().unwrap().0 {
            collapsed_bytes = collapsed_bytes.saturating_add(largest.pop().unwrap().0.0);
            largest.push(Reverse(candidate));
        } else {
            collapsed_bytes = collapsed_bytes.saturating_add(weight);
        }
    }
    if count > largest.len() && largest.len() == limit {
        collapsed_bytes = collapsed_bytes.saturating_add(largest.pop().unwrap().0.0);
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
