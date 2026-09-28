//! A browsable index of everything the engine has observed.
//!
//! The engine used to answer "what can be done here" by walking one observation from
//! scratch on every request. That is both slow (the whole observation is re-read) and
//! lossy (only what that walk happened to look at survives). This module keeps the
//! observed world instead: cells, entities, inventory and the last observed body state,
//! updated incrementally and queried by capability.
//!
//! # Layout
//!
//! Observed cells are bucketed into 16x16x16 chunks. The bucket key is [`ChunkKey`]
//! (`floor(coordinate / 16)` per axis), and each bucket holds a `BTreeMap<u16, Cell>`: a
//! compact list keyed by a local index `(local_x << 8) | (local_z << 4) | local_y`. The
//! vertical neighbour of a cell is therefore the adjacent index, which makes the one
//! derived capability cheap. A cell is 12 bytes: an interned name id (`u32`), a capability
//! bit set (`u8`), three sub-block offsets in 1/16 block units (`[u8; 3]`) and the
//! generation it was last observed in (`u32`). Names live once in an interning table, so a
//! cell never carries a `String`.
//!
//! Entities are bucketed the same way, keyed by interned name plus position bits, so an
//! entity that moved is one removal and one insertion rather than a rebuild.
//!
//! Bucketing is what makes the index browsable: a query visits only the buckets that can
//! intersect its sphere, and a page walks buckets in key order, which is a stable total
//! order across runs. `BTreeMap` (not `HashMap`) is the deliberate choice for exactly that
//! stability; the interning map and the entity buckets use a small FNV hasher instead,
//! because they are only ever probed by key.
//!
//! # What is cheap
//!
//! * [`WorldIndex::apply`] is O(cells in the observation) for the comparison pass — a
//!   cumulative observation has to be looked at to know what changed — and O(changed cells)
//!   for every write, allocation and neighbour ripple. Nothing is ever rebuilt while the
//!   world epoch and dimension stay the same; only the cells whose name or position changed
//!   are written, and only the neighbour whose derived bit depends on them is recomputed.
//! * Re-applying an identical observation performs no insert, no removal, no rename and no
//!   allocation: it marks the generation on each observed cell and returns. Measured in
//!   `tests/world_index.rs`, it costs a fraction of the first apply.
//! * Dropping cells the latest observation no longer reports is the only O(stored cells)
//!   step, and it runs only when a new cell appeared or the observed count disagrees with
//!   the stored count; a repeated or purely renamed observation never sweeps.
//! * Every query is bounded by an explicit cap and stops as soon as the cap is filled, so a
//!   common query touches a few hundred cells, not the whole world.
//! * [`WorldIndex::browse`] resumes from a cursor, so paging through everything costs one
//!   page of work per page, never a full materialisation.
//!
//! # Capabilities and caps
//!
//! Every observed cell carries a bit set: [`category::HARVESTABLE`] (breaking it yields the
//! expected drop), [`category::SOLID_SUPPORT`] (a full cube a block can rest on),
//! [`category::AIR`] (empty) and [`category::BUILDABLE_SURFACE`] (a placeable cell whose
//! observed support is solid — derived from the cell below and refreshed when that support
//! changes). A cell with no bit is observed but carries no capability.
//!
//! Block classification is name-based ([`category_of`], [`drop_item`]). It is a stand-in
//! for the adapter's block-state data (`crate::perception::BlockFacts`): an unrecognised
//! block name is observed and queryable but is deliberately not claimed to be harvestable
//! or solid. Drops are the plain-tool common case; enchantment variants are not modelled.
//!
//! Caps: queries clamp their radius to [`MAX_QUERY_RADIUS_M`] and their cap to
//! [`MAX_QUERY_CAP`]; [`WorldIndex::browse`] clamps a page to [`MAX_PAGE_SIZE`];
//! `edible_now`, `craftable_now` and `placeable_now` are bounded by [`MAX_DERIVED_OPTIONS`]
//! and `attackable_now` by [`MAX_ATTACK_TARGETS`]. Positions are kept to 1/16 block
//! ([`POSITION_STEPS_PER_BLOCK`]), far finer than the engine's arrival tolerance, so no
//! decision turns on the quantisation.
//!
//! The index holds what the *latest applied* observation reported: an entity the newest
//! observation did not mention is dropped, so answers never mix two snapshots. Health is
//! only ever replaced by a caller that supplies it ([`WorldIndex::apply_entities`]).

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::hash::{BuildHasherDefault, Hasher};
use std::ops::Bound;

use crate::model::{Landmark, Observation, Position};

/// Side of one index bucket, in blocks.
pub const CHUNK_SIZE: i32 = 16;
/// Cells one bucket can hold (`16 * 16 * 16`).
pub const CELLS_PER_CHUNK: u16 = 4096;
/// Sub-block steps per block used to store a cell position.
pub const POSITION_STEPS_PER_BLOCK: f64 = 16.0;
/// A query radius larger than this is clamped to it.
pub const MAX_QUERY_RADIUS_M: f64 = 128.0;
/// A query cap larger than this is clamped to it.
pub const MAX_QUERY_CAP: usize = 4096;
/// Smallest page [`WorldIndex::browse`] will serve.
pub const MIN_PAGE_SIZE: usize = 1;
/// Largest page [`WorldIndex::browse`] will serve.
pub const MAX_PAGE_SIZE: usize = 256;
/// Page size used when a caller does not choose one.
pub const DEFAULT_PAGE_SIZE: usize = 32;
/// Bound on each inventory-derived list (`edible_now`, `craftable_now`, `placeable_now`).
pub const MAX_DERIVED_OPTIONS: usize = 64;
/// Bound on `attackable_now`, which has no cap argument of its own.
pub const MAX_ATTACK_TARGETS: usize = 64;
/// Sections of the browsable space, in the fixed order [`WorldIndex::browse`] walks them.
pub const SECTION_COUNT: usize = 6;

/// Capability bits of one observed cell.
pub mod category {
    /// Breaking the block yields [`super::drop_item`]'s expected drop.
    pub const HARVESTABLE: u8 = 1 << 0;
    /// A placeable cell whose observed support is solid.
    pub const BUILDABLE_SURFACE: u8 = 1 << 1;
    /// A full cube another block can rest on.
    pub const SOLID_SUPPORT: u8 = 1 << 2;
    /// Empty.
    pub const AIR: u8 = 1 << 3;
    /// The four published bits.
    pub const MASK: u8 = HARVESTABLE | BUILDABLE_SURFACE | SOLID_SUPPORT | AIR;
}

/// Internal name bit: a block that a placement replaces. It is never published on a cell;
/// it only feeds the derived [`category::BUILDABLE_SURFACE`] bit.
const NAME_REPLACEABLE: u8 = 1 << 4;

/// Coordinates outside this bound are not indexed at all.
const MAX_COORDINATE: f64 = 30_000_000.0;
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

// ---------------------------------------------------------------------------------------
// Chunk and cell geometry
// ---------------------------------------------------------------------------------------

/// Bucket key of one 16x16x16 region of the world.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChunkKey {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl ChunkKey {
    /// The bucket that contains `position`, or `None` when it cannot be bucketed.
    pub fn of(position: &Position) -> Option<Self> {
        Some(Self {
            x: chunk_axis(position.x)?,
            y: chunk_axis(position.y)?,
            z: chunk_axis(position.z)?,
        })
    }

    /// The lowest corner of the bucket, as a block coordinate.
    pub fn origin(self) -> Position {
        let size = f64::from(CHUNK_SIZE);
        Position {
            x: f64::from(self.x) * size,
            y: f64::from(self.y) * size,
            z: f64::from(self.z) * size,
        }
    }

    /// Whether `position` buckets into this chunk.
    pub fn contains(self, position: &Position) -> bool {
        Self::of(position) == Some(self)
    }
}

/// Stable address of one observed cell: its bucket and its local index in that bucket.
/// This is what [`WorldIndex::harvestable_since`] and friends resume from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CellKey {
    pub chunk: ChunkKey,
    pub index: u16,
}

impl CellKey {
    /// The cell address `position` falls into, or `None` when it cannot be bucketed.
    pub fn of(position: &Position) -> Option<Self> {
        let chunk = ChunkKey::of(position)?;
        Some(Self {
            chunk,
            index: local_index(chunk, position),
        })
    }

    /// The cell position this address is stored at, at 1/16 block resolution.
    pub fn position(self) -> Position {
        Position {
            x: f64::from(self.chunk.x) * f64::from(CHUNK_SIZE)
                + f64::from(self.index >> 8) / POSITION_STEPS_PER_BLOCK,
            y: f64::from(self.chunk.y) * f64::from(CHUNK_SIZE)
                + f64::from((self.index >> 4) & 15) / POSITION_STEPS_PER_BLOCK,
            z: f64::from(self.chunk.z) * f64::from(CHUNK_SIZE)
                + f64::from(self.index & 15) / POSITION_STEPS_PER_BLOCK,
        }
    }
}

/// One observed cell, as reported back to a caller.
#[derive(Clone, Debug, PartialEq)]
pub struct ObservedCell {
    /// Name exactly as the observation spelled it.
    pub name: String,
    pub position: Position,
    pub category: u8,
    /// Expected drop when the cell is harvestable; `None` for any other cell.
    pub drop_item: Option<String>,
}

fn chunk_axis(value: f64) -> Option<i32> {
    if !value.is_finite() || value.abs() > MAX_COORDINATE {
        return None;
    }
    Some((value / f64::from(CHUNK_SIZE)).floor() as i32)
}

fn local_index(chunk: ChunkKey, position: &Position) -> u16 {
    let size = f64::from(CHUNK_SIZE);
    let lx = (position.x.floor() - f64::from(chunk.x) * size) as i32;
    let ly = (position.y.floor() - f64::from(chunk.y) * size) as i32;
    let lz = (position.z.floor() - f64::from(chunk.z) * size) as i32;
    ((lx as u16 & 15) << 8) | ((lz as u16 & 15) << 4) | (ly as u16 & 15)
}

fn cell_offsets(chunk: ChunkKey, position: &Position) -> [u8; 3] {
    let size = f64::from(CHUNK_SIZE);
    let step = |value: f64, axis: i32| -> u8 {
        let offset = (value - f64::from(axis) * size) * POSITION_STEPS_PER_BLOCK;
        offset.round().clamp(0.0, 255.0) as u8
    };
    [
        step(position.x, chunk.x),
        step(position.y, chunk.y),
        step(position.z, chunk.z),
    ]
}

/// The cell directly below `index`, which may live in the bucket below.
fn below(chunk: ChunkKey, index: u16) -> (ChunkKey, u16) {
    if index & 15 > 0 {
        (chunk, index - 1)
    } else {
        (
            ChunkKey {
                y: chunk.y.wrapping_sub(1),
                ..chunk
            },
            (index & !15) | 15,
        )
    }
}

/// The cell directly above `index`, which may live in the bucket above.
fn above(chunk: ChunkKey, index: u16) -> (ChunkKey, u16) {
    if index & 15 < 15 {
        (chunk, index + 1)
    } else {
        (
            ChunkKey {
                y: chunk.y.wrapping_add(1),
                ..chunk
            },
            index & !15,
        )
    }
}

fn distance_squared(a: &Position, b: &Position) -> f64 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    let dz = a.z - b.z;
    dx * dx + dy * dy + dz * dz
}

fn usable_center(center: &Position) -> bool {
    center.x.is_finite() && center.y.is_finite() && center.z.is_finite()
}

fn buckets_around(center: &Position, radius: f64) -> ((i32, i32, i32), (i32, i32, i32)) {
    let axis = |value: f64| (value / f64::from(CHUNK_SIZE)).floor() as i32;
    (
        (
            axis(center.x - radius),
            axis(center.y - radius),
            axis(center.z - radius),
        ),
        (
            axis(center.x + radius),
            axis(center.y + radius),
            axis(center.z + radius),
        ),
    )
}

// ---------------------------------------------------------------------------------------
// Storage types
// ---------------------------------------------------------------------------------------

/// One stored cell. 12 bytes, no padding: no `String`, no `Vec`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Cell {
    name: u32,
    category: u8,
    offsets: [u8; 3],
    seen: u32,
}

/// Identity of an observed entity: interned name plus the exact observed position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct EntityId {
    name: u32,
    x: u64,
    y: u64,
    z: u64,
}

#[derive(Clone, Debug, PartialEq)]
struct EntityRecord {
    position: Position,
    health: Option<f32>,
    seen: u32,
}

/// FNV-1a, so interning and entity lookups do not pay for a cryptographic hash on every one
/// of hundreds of thousands of observed cells. No dependencies.
struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Self(FNV_OFFSET)
    }
}

impl Hasher for Fnv {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(FNV_PRIME);
        }
    }
}

type FastMap<K, V> = HashMap<K, V, BuildHasherDefault<Fnv>>;

// ---------------------------------------------------------------------------------------
// Name classification
// ---------------------------------------------------------------------------------------

/// Lowercase registry name without its namespace: `minecraft:Oak_Log` -> `oak_log`, and
/// `waypoint:12:64:3` -> `12:64:3`.
pub fn normalized(name: &str) -> String {
    let lower = name.trim().to_ascii_lowercase();
    match lower.split_once(':') {
        Some((_, bare)) => bare.to_owned(),
        None => lower,
    }
}

/// Canonical registry id: `oak_log` -> `minecraft:oak_log`; a name that already carries a
/// namespace is kept as written.
pub fn canonical_item(name: &str) -> String {
    let lower = name.trim().to_ascii_lowercase();
    if lower.contains(':') {
        lower
    } else {
        format!("minecraft:{lower}")
    }
}

/// The capability bits one block name always carries, derived from the name alone.
pub fn category_of(block: &str) -> u8 {
    name_bits(&normalized(block)) & category::MASK
}

/// Expected drop of one observed block name, or `None` when breaking it is not modelled to
/// yield an item. Always a canonical registry id (`minecraft:cobblestone`).
pub fn drop_item(block: &str) -> Option<String> {
    let bare = normalized(block);
    Some(match drop_of(&bare)? {
        Drop::Itself => canonical_item(block),
        Drop::Item(item) => item.to_owned(),
    })
}

/// Hunger points one item restores, or `None` when it is not food.
pub fn food_value(item: &str) -> Option<f32> {
    let bare = normalized(item);
    FOOD.iter()
        .find(|(name, _)| *name == bare.as_str())
        .map(|(_, hunger)| *hunger)
}

/// Whether one inventory item can be placed as a block.
pub fn is_placeable(item: &str) -> bool {
    let bare = normalized(item);
    is_plain_block_name(&bare)
        && (is_solid_support(&bare) || PLACEABLE_EXACT.contains(&bare.as_str()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Drop {
    /// Breaking the block yields the block itself.
    Itself,
    /// Breaking the block yields a different canonical item.
    Item(&'static str),
}

fn is_plain_block_name(bare: &str) -> bool {
    !bare.is_empty() && bare.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
}

fn name_bits(bare: &str) -> u8 {
    if !is_plain_block_name(bare) {
        return 0;
    }
    let mut bits = 0;
    if AIR_EXACT.contains(&bare) {
        bits |= category::AIR;
    } else if REPLACEABLE_EXACT.contains(&bare) {
        bits |= NAME_REPLACEABLE;
    }
    if is_solid_support(bare) {
        bits |= category::SOLID_SUPPORT;
    }
    if drop_of(bare).is_some() {
        bits |= category::HARVESTABLE;
    }
    bits
}

fn has_suffix(bare: &str, suffixes: &[&str]) -> bool {
    suffixes.iter().any(|suffix| bare.ends_with(suffix))
}

fn drop_of(bare: &str) -> Option<Drop> {
    let named = match bare {
        "stone" => Some("minecraft:cobblestone"),
        "grass_block" | "farmland" | "dirt_path" => Some("minecraft:dirt"),
        "deepslate" => Some("minecraft:cobbled_deepslate"),
        "coal_ore" | "deepslate_coal_ore" => Some("minecraft:coal"),
        "iron_ore" | "deepslate_iron_ore" => Some("minecraft:raw_iron"),
        "copper_ore" | "deepslate_copper_ore" => Some("minecraft:raw_copper"),
        "gold_ore" | "deepslate_gold_ore" | "nether_gold_ore" => Some("minecraft:raw_gold"),
        "diamond_ore" | "deepslate_diamond_ore" => Some("minecraft:diamond"),
        "emerald_ore" | "deepslate_emerald_ore" => Some("minecraft:emerald"),
        "lapis_ore" | "deepslate_lapis_ore" => Some("minecraft:lapis_lazuli"),
        "redstone_ore" | "deepslate_redstone_ore" => Some("minecraft:redstone"),
        "nether_quartz_ore" => Some("minecraft:quartz"),
        "snow" | "snow_layer" => Some("minecraft:snowball"),
        "gravel" => Some("minecraft:gravel"),
        "melon" => Some("minecraft:melon_slice"),
        _ => None,
    };
    if let Some(item) = named {
        return Some(Drop::Item(item));
    }
    if HARVEST_SELF.contains(&bare) || has_suffix(bare, HARVEST_SUFFIXES) {
        return Some(Drop::Itself);
    }
    None
}

fn is_solid_support(bare: &str) -> bool {
    if !is_plain_block_name(bare) {
        return false;
    }
    if NOT_FULL_CUBE_EXACT.contains(&bare) || has_suffix(bare, NOT_FULL_CUBE_SUFFIXES) {
        return false;
    }
    if SOLID_EXACT.contains(&bare) {
        return true;
    }
    if NOT_SOLID_EXACT.contains(&bare) {
        return false;
    }
    has_suffix(bare, SOLID_SUFFIXES)
}

const AIR_EXACT: &[&str] = &["air", "cave_air", "void_air"];

/// Blocks a placement passes through rather than rests on.
const REPLACEABLE_EXACT: &[&str] = &[
    "water",
    "short_grass",
    "grass",
    "fern",
    "large_fern",
    "tall_grass",
    "dead_bush",
    "snow",
    "snow_layer",
    "seagrass",
    "kelp",
    "vine",
    "glow_lichen",
    "hanging_roots",
    "lily_pad",
];

/// Fluid names that are never a full cube.
const NOT_SOLID_EXACT: &[&str] = &[
    "water",
    "lava",
    "flowing_water",
    "flowing_lava",
    "bubble_column",
    "frosted_ice",
];

/// Decorative names whose shape is not a full cube, so nothing rests on them.
const NOT_FULL_CUBE_EXACT: &[&str] = &[
    "torch",
    "soul_torch",
    "redstone_torch",
    "lantern",
    "soul_lantern",
    "campfire",
    "soul_campfire",
    "flower_pot",
    "ladder",
    "scaffolding",
    "cobweb",
    "rail",
    "powered_rail",
    "detector_rail",
    "activator_rail",
    "tripwire",
    "tripwire_hook",
    "snow",
    "snow_layer",
    "cactus",
    "sugar_cane",
    "bamboo",
    "kelp",
    "seagrass",
    "vine",
    "dead_bush",
    "short_grass",
    "grass",
    "fern",
    "large_fern",
    "tall_grass",
    "mushroom_stem",
];

/// Full cubes that are always solid support.
const SOLID_EXACT: &[&str] = &[
    "stone",
    "cobblestone",
    "dirt",
    "coarse_dirt",
    "rooted_dirt",
    "grass_block",
    "podzol",
    "mycelium",
    "mud",
    "packed_mud",
    "moss_block",
    "sand",
    "red_sand",
    "gravel",
    "clay",
    "snow_block",
    "obsidian",
    "bedrock",
    "netherrack",
    "soul_sand",
    "soul_soil",
    "end_stone",
    "sandstone",
    "red_sandstone",
    "smooth_sandstone",
    "deepslate",
    "cobbled_deepslate",
    "polished_deepslate",
    "tuff",
    "calcite",
    "basalt",
    "smooth_basalt",
    "blackstone",
    "andesite",
    "diorite",
    "granite",
    "crafting_table",
    "furnace",
    "chest",
    "trapped_chest",
    "ender_chest",
    "glass",
    "ice",
    "packed_ice",
    "blue_ice",
    "sculk",
    "bone_block",
    "hay_block",
    "dried_kelp_block",
    "melon",
    "pumpkin",
    "carved_pumpkin",
    "jack_o_lantern",
    "bookshelf",
    "glowstone",
    "sea_lantern",
    "shroomlight",
    "quartz_block",
    "bricks",
    "nether_bricks",
    "stone_bricks",
];

/// Suffixes that make a name a full cube.
const SOLID_SUFFIXES: &[&str] = &[
    "_log",
    "_wood",
    "_hyphae",
    "_stem",
    "_planks",
    "_bricks",
    "_terracotta",
    "_concrete",
    "_concrete_powder",
    "_wool",
    "_block",
    "_ore",
    "_deepslate",
    "_sandstone",
    "_sand",
    "_dirt",
    "_leaves",
    "_glass",
];

/// Suffixes that make a name something other than a full cube.
const NOT_FULL_CUBE_SUFFIXES: &[&str] = &[
    "_slab",
    "_stairs",
    "_fence",
    "_fence_gate",
    "_wall",
    "_door",
    "_trapdoor",
    "_pane",
    "_carpet",
    "_button",
    "_pressure_plate",
    "_sapling",
    "_bed",
    "_banner",
    "_sign",
    "_hanging_sign",
    "_coral",
    "_coral_fan",
    "_coral_wall_fan",
    "_fan",
    "_seeds",
    "_propagule",
    "_roots",
    "_fungus",
    "_sprouts",
    "_bud",
    "_shoots",
    "_head",
    "_skull",
    "_torch",
    "_rail",
    "_candle",
    "_vein",
    "_bush",
    "_grass",
    "_fern",
    "_wart",
    "_web",
    "_ladder",
    "_chain",
    "_rod",
    "_bars",
    "_campfire",
    "_flower",
    "_petals",
    "_cluster",
    "_lily",
    "_shelf",
];

/// Names that drop themselves under a plain tool.
const HARVEST_SELF: &[&str] = &[
    "cobblestone",
    "cobbled_deepslate",
    "andesite",
    "diorite",
    "granite",
    "tuff",
    "calcite",
    "basalt",
    "blackstone",
    "netherrack",
    "end_stone",
    "obsidian",
    "sand",
    "red_sand",
    "gravel",
    "clay",
    "dirt",
    "coarse_dirt",
    "rooted_dirt",
    "podzol",
    "mycelium",
    "moss_block",
    "mud",
    "packed_mud",
    "snow_block",
    "sandstone",
    "red_sandstone",
    "crafting_table",
    "furnace",
    "chest",
    "bookshelf",
    "hay_block",
    "bamboo",
    "sugar_cane",
    "cactus",
    "kelp",
    "dried_kelp_block",
    "wheat",
    "carrots",
    "potatoes",
    "beetroots",
    "glowstone",
    "sea_lantern",
    "shroomlight",
    "bone_block",
    "sculk",
    "amethyst_block",
    "copper_block",
    "iron_block",
    "gold_block",
    "diamond_block",
    "emerald_block",
    "coal_block",
    "redstone_block",
    "lapis_block",
    "quartz_block",
    "bricks",
    "nether_bricks",
    "stone_bricks",
    "melon",
    "pumpkin",
];

/// Suffixes whose blocks drop themselves under a plain tool (dyed and wood families).
const HARVEST_SUFFIXES: &[&str] = &[
    "_log",
    "_wood",
    "_hyphae",
    "_stem",
    "_planks",
    "_ore",
    "_wool",
    "_terracotta",
    "_concrete",
    "_concrete_powder",
    "_leaves",
    "_sapling",
    "_sandstone",
    "_bricks",
    "_block",
];

/// Items that are placeable even though they are not a full cube.
const PLACEABLE_EXACT: &[&str] = &[
    "torch",
    "soul_torch",
    "redstone_torch",
    "lantern",
    "ladder",
    "scaffolding",
    "flower_pot",
    "rail",
    "powered_rail",
    "campfire",
];

/// Items that restore hunger, and by how much.
const FOOD: &[(&str, f32)] = &[
    ("apple", 4.0),
    ("golden_apple", 4.0),
    ("enchanted_golden_apple", 4.0),
    ("bread", 5.0),
    ("cooked_beef", 8.0),
    ("beef", 3.0),
    ("cooked_porkchop", 8.0),
    ("porkchop", 3.0),
    ("cooked_chicken", 6.0),
    ("chicken", 2.0),
    ("cooked_mutton", 6.0),
    ("mutton", 2.0),
    ("cooked_rabbit", 5.0),
    ("rabbit", 3.0),
    ("cooked_cod", 5.0),
    ("cod", 2.0),
    ("cooked_salmon", 6.0),
    ("salmon", 2.0),
    ("tropical_fish", 1.0),
    ("pufferfish", 1.0),
    ("carrot", 3.0),
    ("golden_carrot", 6.0),
    ("potato", 1.0),
    ("baked_potato", 5.0),
    ("poisonous_potato", 2.0),
    ("beetroot", 1.0),
    ("beetroot_soup", 6.0),
    ("mushroom_stew", 6.0),
    ("rabbit_stew", 10.0),
    ("melon_slice", 2.0),
    ("sweet_berries", 2.0),
    ("glow_berries", 2.0),
    ("dried_kelp", 1.0),
    ("honey_bottle", 6.0),
    ("cookie", 2.0),
    ("pumpkin_pie", 8.0),
    ("rotten_flesh", 4.0),
    ("spider_eye", 2.0),
    ("chorus_fruit", 4.0),
];

/// Entity kinds that cannot be attacked: dropped items, projectiles and decorations.
const NON_ATTACKABLE: &[&str] = &[
    "item",
    "experience_orb",
    "xp_orb",
    "arrow",
    "spectral_arrow",
    "trident",
    "snowball",
    "egg",
    "ender_pearl",
    "eye_of_ender",
    "experience_bottle",
    "potion",
    "splash_potion",
    "lingering_potion",
    "fireball",
    "small_fireball",
    "dragon_fireball",
    "wither_skull",
    "firework_rocket",
    "shulker_bullet",
    "llama_spit",
    "wind_charge",
    "breeze_wind_charge",
    "fishing_bobber",
    "armor_stand",
    "painting",
    "item_frame",
    "glow_item_frame",
    "leash_knot",
    "area_effect_cloud",
    "lightning_bolt",
    "marker",
    "interaction",
    "display",
    "block_display",
    "item_display",
    "text_display",
    "evoker_fangs",
    "end_crystal",
    "tnt",
    "falling_block",
    "ominous_item_spawner",
];

fn is_attackable(name: &str) -> bool {
    let bare = normalized(name);
    !bare.is_empty()
        && !NON_ATTACKABLE.contains(&bare.as_str())
        && !bare.ends_with("_boat")
        && !bare.ends_with("_minecart")
}

fn is_hostile(name: &str) -> bool {
    crate::survival::is_hostile(name)
}

fn is_ranged(name: &str) -> bool {
    crate::survival::is_ranged(name)
}

// ---------------------------------------------------------------------------------------
// Recipes
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ingredient {
    /// A named canonical item.
    Item(&'static str),
    /// Any of the wooden planks.
    Planks,
}

impl Ingredient {
    fn label(self) -> String {
        match self {
            Self::Item(item) => item.to_owned(),
            Self::Planks => "minecraft:planks (any wood)".to_owned(),
        }
    }

    fn have(self, items: &BTreeMap<String, u32>) -> u32 {
        match self {
            Self::Item(item) => items.get(item).copied().unwrap_or(0),
            Self::Planks => items
                .iter()
                .filter(|(item, _)| normalized(item).ends_with("_planks"))
                .fold(0u32, |total, (_, count)| total.saturating_add(*count)),
        }
    }
}

struct Recipe {
    output: &'static str,
    count: u32,
    inputs: &'static [(Ingredient, u32)],
}

/// One-step recipes. Multi-step chains (log -> planks -> table) are reported one step at a
/// time, so a caller always sees the step that is possible now.
static RECIPES: &[Recipe] = &[
    Recipe {
        output: "minecraft:oak_planks",
        count: 4,
        inputs: &[(Ingredient::Item("minecraft:oak_log"), 1)],
    },
    Recipe {
        output: "minecraft:spruce_planks",
        count: 4,
        inputs: &[(Ingredient::Item("minecraft:spruce_log"), 1)],
    },
    Recipe {
        output: "minecraft:birch_planks",
        count: 4,
        inputs: &[(Ingredient::Item("minecraft:birch_log"), 1)],
    },
    Recipe {
        output: "minecraft:jungle_planks",
        count: 4,
        inputs: &[(Ingredient::Item("minecraft:jungle_log"), 1)],
    },
    Recipe {
        output: "minecraft:acacia_planks",
        count: 4,
        inputs: &[(Ingredient::Item("minecraft:acacia_log"), 1)],
    },
    Recipe {
        output: "minecraft:dark_oak_planks",
        count: 4,
        inputs: &[(Ingredient::Item("minecraft:dark_oak_log"), 1)],
    },
    Recipe {
        output: "minecraft:mangrove_planks",
        count: 4,
        inputs: &[(Ingredient::Item("minecraft:mangrove_log"), 1)],
    },
    Recipe {
        output: "minecraft:cherry_planks",
        count: 4,
        inputs: &[(Ingredient::Item("minecraft:cherry_log"), 1)],
    },
    Recipe {
        output: "minecraft:pale_oak_planks",
        count: 4,
        inputs: &[(Ingredient::Item("minecraft:pale_oak_log"), 1)],
    },
    Recipe {
        output: "minecraft:crimson_planks",
        count: 4,
        inputs: &[(Ingredient::Item("minecraft:crimson_stem"), 1)],
    },
    Recipe {
        output: "minecraft:warped_planks",
        count: 4,
        inputs: &[(Ingredient::Item("minecraft:warped_stem"), 1)],
    },
    Recipe {
        output: "minecraft:bamboo_planks",
        count: 2,
        inputs: &[(Ingredient::Item("minecraft:bamboo_block"), 1)],
    },
    Recipe {
        output: "minecraft:stick",
        count: 4,
        inputs: &[(Ingredient::Planks, 2)],
    },
    Recipe {
        output: "minecraft:crafting_table",
        count: 1,
        inputs: &[(Ingredient::Planks, 4)],
    },
    Recipe {
        output: "minecraft:chest",
        count: 1,
        inputs: &[(Ingredient::Planks, 8)],
    },
    Recipe {
        output: "minecraft:bowl",
        count: 4,
        inputs: &[(Ingredient::Planks, 3)],
    },
    Recipe {
        output: "minecraft:torch",
        count: 4,
        inputs: &[
            (Ingredient::Item("minecraft:coal"), 1),
            (Ingredient::Item("minecraft:stick"), 1),
        ],
    },
    Recipe {
        output: "minecraft:furnace",
        count: 1,
        inputs: &[(Ingredient::Item("minecraft:cobblestone"), 8)],
    },
    Recipe {
        output: "minecraft:bread",
        count: 1,
        inputs: &[(Ingredient::Item("minecraft:wheat"), 3)],
    },
    Recipe {
        output: "minecraft:wooden_pickaxe",
        count: 1,
        inputs: &[
            (Ingredient::Planks, 3),
            (Ingredient::Item("minecraft:stick"), 2),
        ],
    },
    Recipe {
        output: "minecraft:wooden_axe",
        count: 1,
        inputs: &[
            (Ingredient::Planks, 3),
            (Ingredient::Item("minecraft:stick"), 2),
        ],
    },
    Recipe {
        output: "minecraft:wooden_shovel",
        count: 1,
        inputs: &[
            (Ingredient::Planks, 1),
            (Ingredient::Item("minecraft:stick"), 2),
        ],
    },
    Recipe {
        output: "minecraft:wooden_sword",
        count: 1,
        inputs: &[
            (Ingredient::Planks, 2),
            (Ingredient::Item("minecraft:stick"), 1),
        ],
    },
    Recipe {
        output: "minecraft:wooden_hoe",
        count: 1,
        inputs: &[
            (Ingredient::Planks, 2),
            (Ingredient::Item("minecraft:stick"), 2),
        ],
    },
    Recipe {
        output: "minecraft:stone_pickaxe",
        count: 1,
        inputs: &[
            (Ingredient::Item("minecraft:cobblestone"), 3),
            (Ingredient::Item("minecraft:stick"), 2),
        ],
    },
    Recipe {
        output: "minecraft:stone_axe",
        count: 1,
        inputs: &[
            (Ingredient::Item("minecraft:cobblestone"), 3),
            (Ingredient::Item("minecraft:stick"), 2),
        ],
    },
    Recipe {
        output: "minecraft:stone_shovel",
        count: 1,
        inputs: &[
            (Ingredient::Item("minecraft:cobblestone"), 1),
            (Ingredient::Item("minecraft:stick"), 2),
        ],
    },
    Recipe {
        output: "minecraft:stone_sword",
        count: 1,
        inputs: &[
            (Ingredient::Item("minecraft:cobblestone"), 2),
            (Ingredient::Item("minecraft:stick"), 1),
        ],
    },
    Recipe {
        output: "minecraft:stone_hoe",
        count: 1,
        inputs: &[
            (Ingredient::Item("minecraft:cobblestone"), 2),
            (Ingredient::Item("minecraft:stick"), 2),
        ],
    },
];

fn crafts_available(recipe: &Recipe, items: &BTreeMap<String, u32>) -> u32 {
    recipe
        .inputs
        .iter()
        .map(|(ingredient, need)| ingredient.have(items) / (*need).max(1))
        .min()
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------------------
// Query results
// ---------------------------------------------------------------------------------------

/// One observed harvestable cell. `distance_m` is `None` only when the query had no centre
/// — a browse page with no observed bot position.
#[derive(Clone, Debug, PartialEq)]
pub struct HarvestOption {
    pub name: String,
    pub position: Position,
    pub distance_m: Option<f64>,
    /// Expected drop of `name`; never absent for a harvestable cell.
    pub drop_item: String,
    pub cursor: CellKey,
}

/// One observed cell a held block could be placed into.
#[derive(Clone, Debug, PartialEq)]
pub struct BuildSpot {
    pub position: Position,
    /// The observed block occupying the cell; a placed block replaces it.
    pub existing: String,
    /// The observed block that would carry the placement.
    pub support: String,
    pub support_position: Position,
    /// The item that would be placed: the held block when it is placeable, otherwise the
    /// first placeable item the inventory holds. `None` when nothing placeable is held.
    pub place: Option<String>,
    pub distance_m: Option<f64>,
    pub cursor: CellKey,
}

/// One observed entity, as reported back to a caller.
#[derive(Clone, Debug, PartialEq)]
pub struct EntityView {
    pub name: String,
    pub position: Position,
    pub health: Option<f32>,
    /// Distance to the queried centre; `None` when the query had no centre (browse).
    pub distance_m: Option<f64>,
    /// Whether the entity kind is a hostile mob.
    pub hostile: bool,
}

/// One inventory item that restores hunger.
#[derive(Clone, Debug, PartialEq)]
pub struct FoodOption {
    pub item: String,
    pub count: u32,
    pub hunger: f32,
}

/// One recipe the current inventory satisfies at least once.
#[derive(Clone, Debug, PartialEq)]
pub struct CraftOption {
    pub item: String,
    /// Items one craft produces.
    pub count: u32,
    /// How many times the recipe can be crafted right now.
    pub crafts: u32,
    pub ingredients: Vec<(String, u32)>,
}

/// One inventory item that can be placed as a block.
#[derive(Clone, Debug, PartialEq)]
pub struct PlaceOption {
    pub item: String,
    pub count: u32,
}

/// What one `apply` changed. Every field is exact, so a caller can check that an unchanged
/// observation really did nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ApplyReport {
    /// Cells the observation listed.
    pub observed_cells: usize,
    /// Cells the observation listed that could not be bucketed (non-finite or out of range).
    pub cells_rejected: usize,
    pub cells_added: usize,
    /// Cells already known whose name or position this observation changed.
    pub cells_renamed: usize,
    /// Cells the index held that this observation no longer reports.
    pub cells_removed: usize,
    pub entities_added: usize,
    pub entities_updated: usize,
    pub entities_removed: usize,
    /// Set when the world epoch or dimension changed, so the index started from nothing.
    pub world_changed: bool,
}

/// One entity observed outside an [`Observation`], carrying the health the observation
/// channel does not have.
#[derive(Clone, Debug, PartialEq)]
pub struct ObservedEntity {
    pub name: String,
    pub position: Position,
    pub health: Option<f32>,
}

impl ObservedEntity {
    pub fn new(name: impl Into<String>, position: Position, health: Option<f32>) -> Self {
        Self {
            name: name.into(),
            position,
            health,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EntityFilter {
    Any,
    Threat,
    Attackable,
}

/// Position of one item in the browsable space. `start` begins at the first harvestable
/// cell; every cursor a query returns can be passed straight back in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cursor {
    section: u8,
    chunk: Option<ChunkKey>,
    slot: u16,
    entity: Option<EntityId>,
    index: Option<u32>,
}

impl Cursor {
    /// The first position of the whole space.
    pub fn start() -> Self {
        Self::default()
    }

    /// Which section of the space this cursor sits in.
    pub fn section(&self) -> usize {
        usize::from(self.section)
    }
}

/// One item of the browsable possibility space.
#[derive(Clone, Debug, PartialEq)]
pub enum BrowseItem {
    Harvestable(HarvestOption),
    Buildable(BuildSpot),
    Entity(EntityView),
    Edible(FoodOption),
    Craftable(CraftOption),
    Placeable(PlaceOption),
}

/// One page of the possibility space. `next` is where the following page resumes; `done`
/// says the space is exhausted, so a caller may stop without asking again.
#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    pub items: Vec<BrowseItem>,
    pub next: Option<Cursor>,
    pub done: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct CellHit {
    key: CellKey,
    position: Position,
    distance_m: Option<f64>,
    name_id: u32,
}

#[derive(Clone, Debug)]
struct EntityHit {
    position: Position,
    health: Option<f32>,
    name_id: u32,
    distance_m: Option<f64>,
    hostile: bool,
}

#[derive(Clone, Debug)]
struct IncomingEntity {
    name: u32,
    position: Position,
    health: Option<f32>,
}

#[derive(Default)]
struct EntityChanges {
    added: usize,
    updated: usize,
    removed: usize,
    unique: usize,
    grew: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CellOutcome {
    Added,
    Renamed,
    Unchanged,
}

const SECTION_HARVESTABLE: usize = 0;
const SECTION_BUILDABLE: usize = 1;
const SECTION_ENTITIES: usize = 2;
const SECTION_EDIBLE: usize = 3;
const SECTION_CRAFTABLE: usize = 4;
const SECTION_PLACEABLE: usize = 5;

// ---------------------------------------------------------------------------------------
// The index
// ---------------------------------------------------------------------------------------

/// Everything observed so far, bucketed, incrementally maintained and queryable by
/// capability. See the module documentation for the layout and the cost model.
#[derive(Default)]
pub struct WorldIndex {
    names: Vec<Box<str>>,
    name_ids: FastMap<Box<str>, u32>,
    name_bits: Vec<u8>,
    chunks: BTreeMap<ChunkKey, BTreeMap<u16, Cell>>,
    entity_chunks: BTreeMap<ChunkKey, BTreeMap<EntityId, EntityRecord>>,
    items: BTreeMap<String, u32>,
    health: f32,
    food: f32,
    time_of_day: Option<u64>,
    held_item: Option<String>,
    bot_position: Option<Position>,
    sequence: u64,
    world_epoch: u64,
    dimension: Option<String>,
    entities_incomplete: bool,
    applied: u64,
    cell_count: usize,
    entity_count: usize,
    cell_generation: u32,
    entity_generation: u32,
}

impl WorldIndex {
    /// An index that has observed nothing.
    pub fn new() -> Self {
        Self::default()
    }

    // -- ingestion ----------------------------------------------------------------------

    /// Apply one observation. The observation is cumulative: it reports the observed world,
    /// so a cell it does not mention is dropped and an entity it does not mention is
    /// dropped. Cost is O(listed cells) for the comparison pass plus O(changed cells) for
    /// every write, insertion and derived-bit refresh; nothing is rebuilt while the world
    /// epoch and dimension are unchanged.
    pub fn apply(&mut self, observation: &Observation) -> ApplyReport {
        let mut report = ApplyReport::default();
        if observation.world_epoch != self.world_epoch || observation.dimension != self.dimension {
            if self.applied > 0 {
                self.clear_world();
                report.world_changed = true;
            }
            self.world_epoch = observation.world_epoch;
            self.dimension = observation.dimension.clone();
        }
        self.applied += 1;

        self.apply_cells(&observation.blocks, &mut report);

        let incoming: Vec<IncomingEntity> = observation
            .entities
            .iter()
            .filter_map(|entity| self.incoming(&entity.name, &entity.position, None))
            .collect();
        let generation = self.next_entity_generation();
        let changes = self.ingest_entities(&incoming, generation);
        report.entities_added = changes.added;
        report.entities_updated = changes.updated;
        report.entities_removed = changes.removed;

        self.items = canonical_items(&observation.items);
        self.health = observation.health;
        self.food = observation.food;
        self.time_of_day = observation.time_of_day;
        self.held_item = observation
            .held_item
            .as_deref()
            .map(str::trim)
            .filter(|held| !held.is_empty())
            .map(canonical_item);
        self.bot_position = Some(observation.position.clone());
        self.sequence = observation.sequence;
        self.entities_incomplete = observation.entity_scan_incomplete;
        report
    }

    /// Apply only the observed entities, for adapters that know their health. Name and
    /// position identify an entity; the same entity keeps the health it was last given when
    /// a later call passes `None`.
    pub fn apply_entities(&mut self, entities: &[ObservedEntity]) -> ApplyReport {
        let incoming: Vec<IncomingEntity> = entities
            .iter()
            .filter_map(|entity| self.incoming(&entity.name, &entity.position, entity.health))
            .collect();
        let generation = self.next_entity_generation();
        let changes = self.ingest_entities(&incoming, generation);
        ApplyReport {
            entities_added: changes.added,
            entities_updated: changes.updated,
            entities_removed: changes.removed,
            ..ApplyReport::default()
        }
    }

    /// Forget everything observed: the next observation is applied as a first one.
    pub fn clear(&mut self) {
        self.clear_world();
        self.items.clear();
        self.health = 0.0;
        self.food = 0.0;
        self.time_of_day = None;
        self.held_item = None;
        self.bot_position = None;
        self.sequence = 0;
        self.world_epoch = 0;
        self.dimension = None;
        self.entities_incomplete = false;
        self.applied = 0;
    }

    fn clear_world(&mut self) {
        self.chunks.clear();
        self.entity_chunks.clear();
        self.cell_count = 0;
        self.entity_count = 0;
    }

    fn next_cell_generation(&mut self) -> u32 {
        self.cell_generation = self.cell_generation.wrapping_add(1);
        if self.cell_generation == 0 {
            // After 2^32 applies the generation would alias the stored one, so the world is
            // dropped and the current observation rebuilds it.
            self.chunks.clear();
            self.cell_count = 0;
            self.cell_generation = 1;
        }
        self.cell_generation
    }

    fn next_entity_generation(&mut self) -> u32 {
        self.entity_generation = self.entity_generation.wrapping_add(1);
        if self.entity_generation == 0 {
            self.entity_chunks.clear();
            self.entity_count = 0;
            self.entity_generation = 1;
        }
        self.entity_generation
    }

    fn intern(&mut self, name: &str) -> u32 {
        if let Some(id) = self.name_ids.get(name) {
            return *id;
        }
        let id = self.names.len() as u32;
        let owned: Box<str> = name.into();
        self.name_bits.push(name_bits(&normalized(&owned)));
        self.name_ids.insert(owned.clone(), id);
        self.names.push(owned);
        id
    }

    fn incoming(
        &mut self,
        name: &str,
        position: &Position,
        health: Option<f32>,
    ) -> Option<IncomingEntity> {
        ChunkKey::of(position)?;
        Some(IncomingEntity {
            name: self.intern(name),
            position: position.clone(),
            health,
        })
    }

    fn apply_cells(&mut self, blocks: &[Landmark], report: &mut ApplyReport) {
        let generation = self.next_cell_generation();
        let before = self.cell_count;
        let mut added = 0usize;
        let mut unique = 0usize;
        let mut grew = false;
        for landmark in blocks {
            report.observed_cells += 1;
            let Some(chunk) = ChunkKey::of(&landmark.position) else {
                report.cells_rejected += 1;
                continue;
            };
            let index = local_index(chunk, &landmark.position);
            let name = self.intern(&landmark.name);
            let bits = self.name_bits[name as usize];
            let replaceable = bits & (category::AIR | NAME_REPLACEABLE) != 0;
            let buildable = replaceable && self.cell_is_solid(below(chunk, index));
            let cell = Cell {
                name,
                category: (bits & category::MASK)
                    | if buildable {
                        category::BUILDABLE_SURFACE
                    } else {
                        0
                    },
                offsets: cell_offsets(chunk, &landmark.position),
                seen: generation,
            };
            let outcome = {
                let cells = self.chunks.entry(chunk).or_default();
                match cells.entry(index) {
                    Entry::Vacant(slot) => {
                        slot.insert(cell);
                        grew = true;
                        unique += 1;
                        added += 1;
                        CellOutcome::Added
                    }
                    Entry::Occupied(mut slot) => {
                        let stored = slot.get_mut();
                        let duplicate = stored.seen == generation;
                        if !duplicate {
                            unique += 1;
                        }
                        let changed = stored.name != cell.name
                            || stored.category != cell.category
                            || stored.offsets != cell.offsets;
                        stored.name = cell.name;
                        stored.category = cell.category;
                        stored.offsets = cell.offsets;
                        stored.seen = generation;
                        if changed {
                            CellOutcome::Renamed
                        } else {
                            CellOutcome::Unchanged
                        }
                    }
                }
            };
            match outcome {
                CellOutcome::Added => {
                    report.cells_added += 1;
                    self.refresh_derived(chunk, index);
                }
                CellOutcome::Renamed => {
                    report.cells_renamed += 1;
                    self.refresh_derived(chunk, index);
                }
                CellOutcome::Unchanged => {}
            }
        }
        if grew || unique != before {
            let removed = self.sweep_cells(generation);
            report.cells_removed = removed;
            self.cell_count = before + added - removed;
        }
    }

    fn sweep_cells(&mut self, generation: u32) -> usize {
        let mut removed: Vec<(ChunkKey, u16)> = Vec::new();
        self.chunks.retain(|chunk, cells| {
            cells.retain(|index, cell| {
                if cell.seen == generation {
                    true
                } else {
                    removed.push((*chunk, *index));
                    false
                }
            });
            !cells.is_empty()
        });
        for (chunk, index) in &removed {
            let (chunk_above, index_above) = above(*chunk, *index);
            self.refresh_buildable(chunk_above, index_above);
        }
        removed.len()
    }

    fn ingest_entities(&mut self, incoming: &[IncomingEntity], generation: u32) -> EntityChanges {
        let before = self.entity_count;
        let mut changes = EntityChanges::default();
        for entity in incoming {
            let Some(chunk) = ChunkKey::of(&entity.position) else {
                continue;
            };
            let id = EntityId {
                name: entity.name,
                x: entity.position.x.to_bits(),
                y: entity.position.y.to_bits(),
                z: entity.position.z.to_bits(),
            };
            let bucket = self.entity_chunks.entry(chunk).or_default();
            match bucket.entry(id) {
                Entry::Vacant(slot) => {
                    slot.insert(EntityRecord {
                        position: entity.position.clone(),
                        health: entity.health,
                        seen: generation,
                    });
                    changes.grew = true;
                    changes.unique += 1;
                    changes.added += 1;
                }
                Entry::Occupied(mut slot) => {
                    let record = slot.get_mut();
                    if record.seen != generation {
                        record.seen = generation;
                        changes.unique += 1;
                    }
                    let health = entity.health.or(record.health);
                    if record.position != entity.position || record.health != health {
                        record.position = entity.position.clone();
                        record.health = health;
                        changes.updated += 1;
                    }
                }
            }
        }
        if changes.grew || changes.unique != before {
            changes.removed = self.sweep_entities(generation);
            self.entity_count = before + changes.added - changes.removed;
        }
        changes
    }

    fn sweep_entities(&mut self, generation: u32) -> usize {
        let mut removed = 0usize;
        self.entity_chunks.retain(|_, bucket| {
            bucket.retain(|_, record| {
                if record.seen == generation {
                    true
                } else {
                    removed += 1;
                    false
                }
            });
            !bucket.is_empty()
        });
        removed
    }

    fn cell_is_solid(&self, at: (ChunkKey, u16)) -> bool {
        self.chunks
            .get(&at.0)
            .and_then(|cells| cells.get(&at.1))
            .is_some_and(|cell| cell.category & category::SOLID_SUPPORT != 0)
    }

    /// Recompute the derived bits of one cell and of the cell above it, whose support this
    /// cell is.
    fn refresh_derived(&mut self, chunk: ChunkKey, index: u16) {
        self.refresh_buildable(chunk, index);
        let (chunk_above, index_above) = above(chunk, index);
        self.refresh_buildable(chunk_above, index_above);
    }

    fn refresh_buildable(&mut self, chunk: ChunkKey, index: u16) {
        let Some(cell) = self
            .chunks
            .get(&chunk)
            .and_then(|cells| cells.get(&index))
            .copied()
        else {
            return;
        };
        let bits = self.name_bits[cell.name as usize];
        let replaceable = bits & (category::AIR | NAME_REPLACEABLE) != 0;
        let buildable = replaceable && self.cell_is_solid(below(chunk, index));
        if let Some(stored) = self
            .chunks
            .get_mut(&chunk)
            .and_then(|cells| cells.get_mut(&index))
        {
            if buildable {
                stored.category |= category::BUILDABLE_SURFACE;
            } else {
                stored.category &= !category::BUILDABLE_SURFACE;
            }
        }
    }

    // -- accessors ----------------------------------------------------------------------

    pub fn cell_count(&self) -> usize {
        self.cell_count
    }

    pub fn entity_count(&self) -> usize {
        self.entity_count
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// Observations applied since the index was created or cleared.
    pub fn applied(&self) -> u64 {
        self.applied
    }

    pub fn is_empty(&self) -> bool {
        self.cell_count == 0 && self.entity_count == 0
    }

    pub fn inventory(&self) -> &BTreeMap<String, u32> {
        &self.items
    }

    pub fn health(&self) -> f32 {
        self.health
    }

    pub fn food(&self) -> f32 {
        self.food
    }

    pub fn time_of_day(&self) -> Option<u64> {
        self.time_of_day
    }

    pub fn held_item(&self) -> Option<&str> {
        self.held_item.as_deref()
    }

    pub fn bot_position(&self) -> Option<&Position> {
        self.bot_position.as_ref()
    }

    pub fn dimension(&self) -> Option<&str> {
        self.dimension.as_deref()
    }

    pub fn world_epoch(&self) -> u64 {
        self.world_epoch
    }

    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Whether the latest observation said its entity scan was truncated. An incomplete
    /// scan is not evidence of safety.
    pub fn entity_scan_incomplete(&self) -> bool {
        self.entities_incomplete
    }

    /// One observed cell, by exact address.
    pub fn cell(&self, key: CellKey) -> Option<ObservedCell> {
        let cell = self.chunks.get(&key.chunk)?.get(&key.index)?;
        Some(self.observed_cell(key.chunk, key.index, cell))
    }

    /// One observed cell, by the position it was observed at.
    pub fn cell_at(&self, position: &Position) -> Option<ObservedCell> {
        self.cell(CellKey::of(position)?)
    }

    /// Observed cells in one bucket, in index order.
    pub fn cells_in_chunk(&self, chunk: ChunkKey) -> Vec<ObservedCell> {
        self.chunks
            .get(&chunk)
            .map(|cells| {
                cells
                    .iter()
                    .map(|(index, cell)| self.observed_cell(chunk, *index, cell))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Stream every observed cell without building a list or allocating a name.
    pub fn for_each_cell<F: FnMut(&Position, &str, u8)>(&self, mut visit: F) {
        for (chunk, cells) in &self.chunks {
            for (index, cell) in cells {
                let position = CellKey {
                    chunk: *chunk,
                    index: *index,
                }
                .position();
                visit(&position, &self.names[cell.name as usize], cell.category);
            }
        }
    }

    fn observed_cell(&self, chunk: ChunkKey, index: u16, cell: &Cell) -> ObservedCell {
        let name = &self.names[cell.name as usize];
        ObservedCell {
            name: name.to_string(),
            position: CellKey { chunk, index }.position(),
            category: cell.category,
            drop_item: if cell.category & category::HARVESTABLE != 0 {
                drop_item(name)
            } else {
                None
            },
        }
    }

    // -- cell queries -------------------------------------------------------------------

    /// Every observed harvestable cell within `radius` of `center`, with its expected drop,
    /// in stable bucket-and-index order. Stops once `cap` results are collected.
    pub fn harvestable_within(
        &self,
        center: &Position,
        radius: f64,
        cap: usize,
    ) -> Vec<HarvestOption> {
        self.harvestable_since(center, radius, cap, None)
    }

    /// [`WorldIndex::harvestable_within`], resuming strictly after `cursor` so a caller can
    /// page through a large region without asking for it all at once.
    pub fn harvestable_since(
        &self,
        center: &Position,
        radius: f64,
        cap: usize,
        cursor: Option<CellKey>,
    ) -> Vec<HarvestOption> {
        self.scan_cells(center, radius, cap, category::HARVESTABLE, cursor)
            .into_iter()
            .map(|hit| self.harvest_option(hit))
            .collect()
    }

    /// Every observed cell a held block could be placed into, with the block that would be
    /// placed, in stable bucket-and-index order.
    pub fn buildable_within(&self, center: &Position, radius: f64, cap: usize) -> Vec<BuildSpot> {
        self.buildable_since(center, radius, cap, None)
    }

    /// [`WorldIndex::buildable_within`], resuming strictly after `cursor`.
    pub fn buildable_since(
        &self,
        center: &Position,
        radius: f64,
        cap: usize,
        cursor: Option<CellKey>,
    ) -> Vec<BuildSpot> {
        let place = self.placement_item();
        self.scan_cells(center, radius, cap, category::BUILDABLE_SURFACE, cursor)
            .into_iter()
            .map(|hit| self.build_spot(hit, place.clone()))
            .collect()
    }

    /// The item a placement would use: the held block when it is placeable, otherwise the
    /// first placeable item the inventory holds in name order.
    pub fn placement_item(&self) -> Option<String> {
        if let Some(held) = self.held_item.as_deref()
            && is_placeable(held)
        {
            return Some(held.to_owned());
        }
        self.items
            .iter()
            .find(|(item, count)| **count > 0 && is_placeable(item))
            .map(|(item, _)| item.clone())
    }

    // -- entity queries -----------------------------------------------------------------

    /// Every observed entity within `radius` of `center`, whatever its kind, bounded by
    /// `cap`. This is the unfiltered sibling of [`WorldIndex::threats_within`].
    pub fn entities_within(&self, center: &Position, radius: f64, cap: usize) -> Vec<EntityView> {
        self.scan_entities(center, radius, cap, EntityFilter::Any)
            .into_iter()
            .map(|hit| self.entity_view(&hit))
            .collect()
    }

    /// Every observed hostile entity within `radius` of `center`, with name, distance and
    /// health. Mobs that shoot are matched out to their own longer detection radius.
    pub fn threats_within(&self, center: &Position, radius: f64, cap: usize) -> Vec<EntityView> {
        self.scan_entities(center, radius, cap, EntityFilter::Threat)
            .into_iter()
            .map(|hit| self.entity_view(&hit))
            .collect()
    }

    /// Every observed entity the bot could attack within `radius` of its last position,
    /// bounded by [`MAX_ATTACK_TARGETS`].
    pub fn attackable_now(&self, radius: f64) -> Vec<EntityView> {
        let Some(center) = self.bot_position.clone() else {
            return Vec::new();
        };
        self.scan_entities(
            &center,
            radius,
            MAX_ATTACK_TARGETS,
            EntityFilter::Attackable,
        )
        .into_iter()
        .map(|hit| self.entity_view(&hit))
        .collect()
    }

    fn entity_view(&self, hit: &EntityHit) -> EntityView {
        EntityView {
            name: self.names[hit.name_id as usize].to_string(),
            position: hit.position.clone(),
            health: hit.health,
            distance_m: hit.distance_m,
            hostile: hit.hostile,
        }
    }

    // -- inventory queries --------------------------------------------------------------

    /// Every item in the inventory that restores hunger, in name order. The bot's own food
    /// level is [`WorldIndex::food`]; this query does not filter on it.
    pub fn edible_now(&self) -> Vec<FoodOption> {
        self.items
            .iter()
            .filter_map(|(item, count)| {
                food_value(item).map(|hunger| FoodOption {
                    item: item.clone(),
                    count: *count,
                    hunger,
                })
            })
            .take(MAX_DERIVED_OPTIONS)
            .collect()
    }

    /// Every recipe the current inventory satisfies at least once, in recipe-table order.
    pub fn craftable_now(&self) -> Vec<CraftOption> {
        RECIPES
            .iter()
            .filter_map(|recipe| {
                let crafts = crafts_available(recipe, &self.items);
                (crafts > 0).then(|| CraftOption {
                    item: recipe.output.to_owned(),
                    count: recipe.count,
                    crafts,
                    ingredients: recipe
                        .inputs
                        .iter()
                        .map(|(ingredient, need)| (ingredient.label(), *need))
                        .collect(),
                })
            })
            .take(MAX_DERIVED_OPTIONS)
            .collect()
    }

    /// Every inventory item that can be placed as a block, in name order.
    pub fn placeable_now(&self) -> Vec<PlaceOption> {
        self.items
            .iter()
            .filter(|(item, count)| **count > 0 && is_placeable(item))
            .map(|(item, count)| PlaceOption {
                item: item.clone(),
                count: *count,
            })
            .take(MAX_DERIVED_OPTIONS)
            .collect()
    }

    // -- browsing -----------------------------------------------------------------------

    /// The next page of the whole possibility space, in stable order: harvestable cells,
    /// then buildable cells, then entities, then edible, craftable and placeable inventory
    /// options. A caller that starts at [`Cursor::start`] and follows `Page::next` sees
    /// everything once, a page at a time.
    pub fn browse(&self, cursor: Cursor, page_size: usize) -> Page {
        let page_size = if page_size == 0 {
            MIN_PAGE_SIZE
        } else {
            page_size.min(MAX_PAGE_SIZE)
        };
        let mut items: Vec<BrowseItem> = Vec::with_capacity(page_size);
        let mut next = None;
        let mut section = usize::from(cursor.section).min(SECTION_COUNT);
        let mut resumed = true;
        while items.len() < page_size && section < SECTION_COUNT {
            let at = if resumed {
                cursor
            } else {
                Cursor {
                    section: section as u8,
                    ..Cursor::default()
                }
            };
            let want = page_size - items.len();
            let (mut produced, resume, exhausted) = self.browse_section(section, at, want);
            let count = produced.len();
            items.append(&mut produced);
            if let Some(resume) = resume {
                next = Some(resume);
            }
            resumed = false;
            if count == want && !exhausted {
                break;
            }
            section += 1;
        }
        let done = section >= SECTION_COUNT;
        Page {
            items,
            next: if done { None } else { next },
            done,
        }
    }

    fn browse_section(
        &self,
        section: usize,
        cursor: Cursor,
        limit: usize,
    ) -> (Vec<BrowseItem>, Option<Cursor>, bool) {
        match section {
            SECTION_HARVESTABLE => {
                let (hits, resume) = self.browse_cells(category::HARVESTABLE, cursor, limit);
                let count = hits.len();
                (
                    hits.into_iter()
                        .map(|hit| BrowseItem::Harvestable(self.harvest_option(hit)))
                        .collect(),
                    resume,
                    count < limit,
                )
            }
            SECTION_BUILDABLE => {
                let (hits, resume) = self.browse_cells(category::BUILDABLE_SURFACE, cursor, limit);
                let count = hits.len();
                let place = self.placement_item();
                (
                    hits.into_iter()
                        .map(|hit| BrowseItem::Buildable(self.build_spot(hit, place.clone())))
                        .collect(),
                    resume,
                    count < limit,
                )
            }
            SECTION_ENTITIES => {
                let (hits, resume) = self.browse_entities(cursor, limit);
                let count = hits.len();
                (
                    hits.iter()
                        .map(|hit| BrowseItem::Entity(self.entity_view(hit)))
                        .collect(),
                    resume,
                    count < limit,
                )
            }
            _ => {
                let list = self.inventory_section(section);
                let from = usize::try_from(cursor.index.unwrap_or(0))
                    .unwrap_or(usize::MAX)
                    .min(list.len());
                let end = from.saturating_add(limit).min(list.len());
                let page: Vec<BrowseItem> = list[from..end].to_vec();
                let resume = (end > from).then(|| Cursor {
                    section: section as u8,
                    index: Some(end as u32),
                    ..Cursor::default()
                });
                (page, resume, end >= list.len())
            }
        }
    }

    fn inventory_section(&self, section: usize) -> Vec<BrowseItem> {
        match section {
            SECTION_EDIBLE => self
                .edible_now()
                .into_iter()
                .map(BrowseItem::Edible)
                .collect(),
            SECTION_CRAFTABLE => self
                .craftable_now()
                .into_iter()
                .map(BrowseItem::Craftable)
                .collect(),
            SECTION_PLACEABLE => self
                .placeable_now()
                .into_iter()
                .map(BrowseItem::Placeable)
                .collect(),
            _ => Vec::new(),
        }
    }

    fn harvest_option(&self, hit: CellHit) -> HarvestOption {
        let name = &self.names[hit.name_id as usize];
        HarvestOption {
            name: name.to_string(),
            drop_item: drop_item(name).unwrap_or_else(|| name.to_string()),
            position: hit.position,
            distance_m: hit.distance_m,
            cursor: hit.key,
        }
    }

    fn build_spot(&self, hit: CellHit, place: Option<String>) -> BuildSpot {
        let support = self.support_of(hit.key);
        let support_name = support
            .as_ref()
            .map(|(cell, _)| self.names[cell.name as usize].to_string())
            .unwrap_or_default();
        let support_position = support.map_or_else(Position::default, |(_, position)| position);
        BuildSpot {
            position: hit.position,
            existing: self.names[hit.name_id as usize].to_string(),
            support: support_name,
            support_position,
            place,
            distance_m: hit.distance_m,
            cursor: hit.key,
        }
    }

    fn support_of(&self, key: CellKey) -> Option<(Cell, Position)> {
        let (chunk, index) = below(key.chunk, key.index);
        let cell = *self.chunks.get(&chunk)?.get(&index)?;
        Some((cell, CellKey { chunk, index }.position()))
    }

    fn browse_cells(
        &self,
        mask: u8,
        cursor: Cursor,
        limit: usize,
    ) -> (Vec<CellHit>, Option<Cursor>) {
        let mut out = Vec::new();
        let start = match cursor.chunk {
            Some(chunk) => Bound::Included(chunk),
            None => Bound::Unbounded,
        };
        for (chunk, cells) in self.chunks.range((start, Bound::Unbounded)) {
            let from = if Some(*chunk) == cursor.chunk {
                cursor.slot.saturating_add(1)
            } else {
                0
            };
            for (index, cell) in cells.range(from..) {
                if cell.category & mask == 0 {
                    continue;
                }
                let key = CellKey {
                    chunk: *chunk,
                    index: *index,
                };
                out.push(CellHit {
                    key,
                    position: key.position(),
                    distance_m: self
                        .bot_position
                        .as_ref()
                        .map(|bot| distance_squared(bot, &key.position()).sqrt()),
                    name_id: cell.name,
                });
                if out.len() >= limit {
                    return (
                        out,
                        Some(Cursor {
                            section: cursor.section,
                            chunk: Some(*chunk),
                            slot: *index,
                            ..Cursor::default()
                        }),
                    );
                }
            }
        }
        (out, None)
    }

    fn browse_entities(&self, cursor: Cursor, limit: usize) -> (Vec<EntityHit>, Option<Cursor>) {
        let mut out = Vec::new();
        let start = match cursor.chunk {
            Some(chunk) => Bound::Included(chunk),
            None => Bound::Unbounded,
        };
        for (chunk, bucket) in self.entity_chunks.range((start, Bound::Unbounded)) {
            let start_at = match (Some(*chunk) == cursor.chunk, cursor.entity) {
                (true, Some(entity)) => Bound::Excluded(entity),
                _ => Bound::Unbounded,
            };
            for (id, record) in bucket.range((start_at, Bound::Unbounded)) {
                let hit = self.entity_hit(id, record, self.bot_position.as_ref(), None);
                let Some(hit) = hit else {
                    continue;
                };
                out.push(hit);
                if out.len() >= limit {
                    return (
                        out,
                        Some(Cursor {
                            section: cursor.section,
                            chunk: Some(*chunk),
                            entity: Some(*id),
                            ..Cursor::default()
                        }),
                    );
                }
            }
        }
        (out, None)
    }

    // -- scanning -----------------------------------------------------------------------

    fn scan_cells(
        &self,
        center: &Position,
        radius: f64,
        cap: usize,
        mask: u8,
        cursor: Option<CellKey>,
    ) -> Vec<CellHit> {
        let cap = cap.min(MAX_QUERY_CAP);
        let radius = radius.min(MAX_QUERY_RADIUS_M);
        if cap == 0 || radius < 0.0 || !usable_center(center) {
            return Vec::new();
        }
        let mut out = Vec::new();
        let radius_squared = radius * radius;
        let (lo, hi) = buckets_around(center, radius);
        for x in lo.0..=hi.0 {
            for y in lo.1..=hi.1 {
                for z in lo.2..=hi.2 {
                    let chunk = ChunkKey { x, y, z };
                    let Some(cells) = self.chunks.get(&chunk) else {
                        continue;
                    };
                    let from = match cursor {
                        Some(at) if at.chunk == chunk => at.index.saturating_add(1),
                        _ => 0,
                    };
                    for (index, cell) in cells.range(from..) {
                        if cell.category & mask == 0 {
                            continue;
                        }
                        let key = CellKey {
                            chunk,
                            index: *index,
                        };
                        let position = key.position();
                        let distance_squared = distance_squared(center, &position);
                        if distance_squared > radius_squared {
                            continue;
                        }
                        out.push(CellHit {
                            key,
                            position,
                            distance_m: Some(distance_squared.sqrt()),
                            name_id: cell.name,
                        });
                        if out.len() >= cap {
                            return out;
                        }
                    }
                }
            }
        }
        out
    }

    fn scan_entities(
        &self,
        center: &Position,
        radius: f64,
        cap: usize,
        filter: EntityFilter,
    ) -> Vec<EntityHit> {
        let cap = cap.min(MAX_QUERY_CAP);
        let radius = radius.min(MAX_QUERY_RADIUS_M);
        if cap == 0 || radius < 0.0 || !usable_center(center) {
            return Vec::new();
        }
        let boxed = if filter == EntityFilter::Threat {
            radius.max(crate::survival::RANGED_THREAT_RADIUS_M)
        } else {
            radius
        };
        let mut out = Vec::new();
        let (lo, hi) = buckets_around(center, boxed);
        for x in lo.0..=hi.0 {
            for y in lo.1..=hi.1 {
                for z in lo.2..=hi.2 {
                    let Some(bucket) = self.entity_chunks.get(&ChunkKey { x, y, z }) else {
                        continue;
                    };
                    for (id, record) in bucket {
                        let hostile = is_hostile(&self.names[id.name as usize]);
                        match filter {
                            EntityFilter::Any => {}
                            EntityFilter::Threat if !hostile => continue,
                            EntityFilter::Attackable
                                if !is_attackable(&self.names[id.name as usize]) =>
                            {
                                continue;
                            }
                            _ => {}
                        }
                        let reach = if hostile && is_ranged(&self.names[id.name as usize]) {
                            radius.max(crate::survival::RANGED_THREAT_RADIUS_M)
                        } else {
                            radius
                        };
                        let Some(hit) = self.entity_hit(id, record, Some(center), Some(reach))
                        else {
                            continue;
                        };
                        out.push(hit);
                        if out.len() >= cap {
                            return out;
                        }
                    }
                }
            }
        }
        out
    }

    /// One entity as a hit, optionally filtered by distance from `center`. `radius` None
    /// computes the distance without filtering it.
    fn entity_hit(
        &self,
        id: &EntityId,
        record: &EntityRecord,
        center: Option<&Position>,
        radius: Option<f64>,
    ) -> Option<EntityHit> {
        let distance_m = match center {
            Some(center) => {
                let distance = distance_squared(center, &record.position).sqrt();
                if radius.is_some_and(|radius| distance > radius) {
                    return None;
                }
                Some(distance)
            }
            None => None,
        };
        Some(EntityHit {
            position: record.position.clone(),
            health: record.health,
            name_id: id.name,
            distance_m,
            hostile: is_hostile(&self.names[id.name as usize]),
        })
    }
}

fn canonical_items(items: &BTreeMap<String, u32>) -> BTreeMap<String, u32> {
    let mut canonical = BTreeMap::new();
    for (item, count) in items {
        if *count == 0 {
            continue;
        }
        let entry = canonical.entry(canonical_item(item)).or_insert(0u32);
        *entry = entry.saturating_add(*count);
    }
    canonical
}
