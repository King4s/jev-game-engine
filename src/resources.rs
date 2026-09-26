//! Survival resource classification and nearest-per-kind selection.
//!
//! Pure functions, independent of Azalea, so the live adapter and tests share them. Ids
//! are registry ids such as `minecraft:oak_log`; a bare path such as `oak_log` is
//! accepted too.
use std::collections::BTreeMap;

use crate::model::{Position, Resource};

/// At most this many resources are reported per kind.
pub const RESOURCES_PER_KIND: usize = 3;
/// Horizontal reach of the block scan, in blocks, on each side of the bot.
pub const BLOCK_SCAN_HORIZONTAL: i32 = 16;
/// Vertical reach of the block scan, in blocks, above and below the bot.
pub const BLOCK_SCAN_VERTICAL: i32 = 8;
/// Animals further than this from the bot, in blocks, are not reported.
pub const ANIMAL_RADIUS_M: f64 = 16.0;

/// Deterministic, fixed-budget cuboid: 33 * 33 * 17 = 18,513 block lookups.
pub fn block_scan_offsets() -> impl Iterator<Item = (i32, i32, i32)> {
    (-BLOCK_SCAN_HORIZONTAL..=BLOCK_SCAN_HORIZONTAL).flat_map(|x| {
        (-BLOCK_SCAN_HORIZONTAL..=BLOCK_SCAN_HORIZONTAL)
            .flat_map(move |z| (-BLOCK_SCAN_VERTICAL..=BLOCK_SCAN_VERTICAL).map(move |y| (x, y, z)))
    })
}

pub fn animal_in_range(origin: &Position, animal: &Position) -> bool {
    distance(origin, animal) <= ANIMAL_RADIUS_M
}

fn path(id: &str) -> &str {
    id.strip_prefix("minecraft:").unwrap_or(id)
}

/// Survival kind of a block id, or `None` when the block is not a tracked resource.
///
/// `log` is the broad suffix category for every *_log and *_stem, including plant
/// stems (not a guarantee of usable wood); the ores include their
/// deepslate variants.
pub fn classify_block(id: &str) -> Option<&'static str> {
    let path = path(id);
    Some(match path {
        "stone" | "cobblestone" | "deepslate" | "cobbled_deepslate" => "stone",
        "coal_ore" | "deepslate_coal_ore" => "coal_ore",
        "iron_ore" | "deepslate_iron_ore" => "iron_ore",
        "copper_ore" | "deepslate_copper_ore" => "copper_ore",
        "crafting_table" => "crafting_table",
        "furnace" => "furnace",
        "chest" => "chest",
        _ if path.ends_with("_log") || path.ends_with("_stem") => "log",
        _ if path.ends_with("_bed") => "bed",
        // `_trapdoor` does not end in `_door`, so trapdoors are excluded.
        _ if path.ends_with("_door") => "door",
        _ => return None,
    })
}

/// Survival kind of an entity id: the animal name for `cow`, `pig`, `sheep` and
/// `chicken`, otherwise `None`.
pub fn classify_entity(id: &str) -> Option<&'static str> {
    match path(id) {
        "cow" => Some("cow"),
        "pig" => Some("pig"),
        "sheep" => Some("sheep"),
        "chicken" => Some("chicken"),
        _ => None,
    }
}

fn distance(a: &Position, b: &Position) -> f64 {
    (a.x - b.x).hypot(a.y - b.y).hypot(a.z - b.z)
}

type Find<'a> = (f64, Position, &'a str);

fn order(a: (f64, &Position, &str), b: (f64, &Position, &str)) -> std::cmp::Ordering {
    a.0.total_cmp(&b.0)
        .then(a.1.x.total_cmp(&b.1.x))
        .then(a.1.y.total_cmp(&b.1.y))
        .then(a.1.z.total_cmp(&b.1.z))
        .then(a.2.cmp(b.2))
}

fn nearer(a: &Find, b: &Find) -> std::cmp::Ordering {
    order((a.0, &a.1, a.2), (b.0, &b.1, b.2))
}

/// Keeps the [`RESOURCES_PER_KIND`] nearest finds of each kind.
///
/// `found` yields `(kind, name, position)`; distance is measured from `origin` to
/// `position` exactly as given. The result is sorted by distance, ties broken by
/// position (x, y, z), then name, then kind, so equal inputs always give equal output.
/// Non-finite distances are dropped. Only the kept finds allocate.
pub fn nearest_resources<'a>(
    origin: &Position,
    found: impl IntoIterator<Item = (&'a str, &'a str, Position)>,
) -> Vec<Resource> {
    let mut kept: BTreeMap<&'a str, Vec<Find<'a>>> = BTreeMap::new();
    for (kind, name, position) in found {
        let entry = (distance(origin, &position), position, name);
        if !entry.0.is_finite() {
            continue;
        }
        let nearest = kept.entry(kind).or_default();
        if nearest.len() == RESOURCES_PER_KIND {
            if nearer(&entry, &nearest[RESOURCES_PER_KIND - 1]).is_ge() {
                continue;
            }
            nearest.pop();
        }
        let index = nearest.partition_point(|kept| nearer(kept, &entry).is_le());
        nearest.insert(index, entry);
    }
    let mut resources: Vec<Resource> = kept
        .into_iter()
        .flat_map(|(kind, nearest)| {
            nearest
                .into_iter()
                .map(move |(distance_m, position, name)| Resource {
                    kind: kind.to_owned(),
                    name: name.to_owned(),
                    position,
                    distance_m,
                })
        })
        .collect();
    resources.sort_by(|a, b| {
        order(
            (a.distance_m, &a.position, &a.name),
            (b.distance_m, &b.position, &b.name),
        )
        .then_with(|| a.kind.cmp(&b.kind))
    });
    resources
}

/// The nearest resource of each kind, in kind order; for reports.
pub fn nearest_per_kind(resources: &[Resource]) -> Vec<&Resource> {
    let mut nearest: BTreeMap<&str, &Resource> = BTreeMap::new();
    for resource in resources {
        nearest
            .entry(resource.kind.as_str())
            .and_modify(|current| {
                if resource.distance_m < current.distance_m {
                    *current = resource;
                }
            })
            .or_insert(resource);
    }
    nearest.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_scan_includes_boundaries_and_has_a_fixed_cell_budget() {
        let offsets: Vec<_> = block_scan_offsets().collect();
        assert_eq!(offsets.len(), 18_513);
        for corner in [(-16, -8, -16), (16, 8, 16), (16, -8, -16)] {
            assert!(offsets.contains(&corner));
        }
        for outside in [(17, 0, 0), (-17, 0, 0), (0, 9, 0), (0, -9, 0), (0, 0, 17)] {
            assert!(!offsets.contains(&outside));
        }
        assert_eq!(
            offsets
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            18_513
        );
    }

    #[test]
    fn animal_radius_is_inclusive_and_euclidean_in_three_dimensions() {
        let origin = Position {
            x: 10.0,
            y: 64.0,
            z: -10.0,
        };
        for (dx, dy, dz, expected) in [
            (16.0, 0.0, 0.0, true),
            (0.0, -16.0, 0.0, true),
            (0.0, 0.0, 16.0001, false),
            (12.0, 12.0, 0.0, false),
            (8.0, 8.0, 8.0, true),
        ] {
            assert_eq!(
                animal_in_range(
                    &origin,
                    &Position {
                        x: origin.x + dx,
                        y: origin.y + dy,
                        z: origin.z + dz
                    }
                ),
                expected
            );
        }
        for name in ["cow", "pig", "sheep", "chicken"] {
            assert_eq!(classify_entity(&format!("minecraft:{name}")), Some(name));
        }
        assert_eq!(classify_entity("zombie"), None);
    }

    #[test]
    fn classifies_all_accepted_block_families() {
        for id in [
            "oak_log",
            "stripped_birch_log",
            "crimson_stem",
            "warped_stem",
        ] {
            assert_eq!(classify_block(id), Some("log"), "{id}");
        }
        // The accepted suffix is intentionally broad and includes non-wood stems.
        assert_eq!(classify_block("pumpkin_stem"), Some("log"));
        for id in ["stone", "cobblestone", "deepslate", "cobbled_deepslate"] {
            assert_eq!(classify_block(id), Some("stone"), "{id}");
        }
        for (ids, kind) in [
            (&["coal_ore", "deepslate_coal_ore"][..], "coal_ore"),
            (&["iron_ore", "deepslate_iron_ore"][..], "iron_ore"),
            (&["copper_ore", "deepslate_copper_ore"][..], "copper_ore"),
        ] {
            for id in ids {
                assert_eq!(classify_block(id), Some(kind), "{id}");
            }
        }
        for id in ["red_bed", "oak_door", "iron_door"] {
            assert_eq!(
                classify_block(id),
                Some(if id.ends_with("_bed") { "bed" } else { "door" })
            );
        }
        for (id, kind) in [
            ("crafting_table", "crafting_table"),
            ("furnace", "furnace"),
            ("chest", "chest"),
        ] {
            assert_eq!(classify_block(id), Some(kind));
        }
        assert_eq!(classify_block("minecraft:oak_log"), Some("log"));
        assert_eq!(classify_block("oak_trapdoor"), None);
    }

    #[test]
    fn keeps_nearest_three_per_kind_in_deterministic_distance_order() {
        let origin = Position::default();
        let found = [
            (
                "log",
                "four",
                Position {
                    x: 4.0,
                    y: 0.0,
                    z: 0.0,
                },
            ),
            (
                "log",
                "two",
                Position {
                    x: 2.0,
                    y: 0.0,
                    z: 0.0,
                },
            ),
            (
                "log",
                "three",
                Position {
                    x: 3.0,
                    y: 0.0,
                    z: 0.0,
                },
            ),
            (
                "log",
                "one",
                Position {
                    x: 1.0,
                    y: 0.0,
                    z: 0.0,
                },
            ),
            (
                "sheep",
                "sheep",
                Position {
                    x: 0.5,
                    y: 0.0,
                    z: 0.0,
                },
            ),
        ];
        let resources = nearest_resources(&origin, found);
        let logs: Vec<_> = resources.iter().filter(|r| r.kind == "log").collect();
        assert_eq!(
            logs.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            ["one", "two", "three"]
        );
        assert_eq!(
            logs.iter().map(|r| r.distance_m).collect::<Vec<_>>(),
            [1.0, 2.0, 3.0]
        );
        assert_eq!(resources[0].kind, "sheep");
    }

    #[test]
    fn breaks_equal_distance_ties_by_position_and_discards_nonfinite_distances() {
        let origin = Position::default();
        let finds = [
            (
                "log",
                "right",
                Position {
                    x: 1.0,
                    y: 0.0,
                    z: 0.0,
                },
            ),
            (
                "log",
                "left",
                Position {
                    x: -1.0,
                    y: 0.0,
                    z: 0.0,
                },
            ),
            (
                "log",
                "nan",
                Position {
                    x: f64::NAN,
                    y: 0.0,
                    z: 0.0,
                },
            ),
            (
                "log",
                "infinite",
                Position {
                    x: f64::INFINITY,
                    y: 0.0,
                    z: 0.0,
                },
            ),
        ];
        let first = nearest_resources(&origin, finds.clone());
        let second = nearest_resources(&origin, finds);
        assert_eq!(first, second);
        assert_eq!(
            first.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            ["left", "right"]
        );
    }
}
