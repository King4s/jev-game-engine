//! Bounded loaded-terrain tree inspection. Paths are reversible cardinal steps;
//! transient airborne reach never qualifies as a mining stance.
use crate::model::Position;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub type Cell = (i32, i32, i32);

/// The six cell faces, in the order every sightline in this crate considers them.
pub const FACE_NORMALS: [(i32, i32, i32); 6] = [
    (1, 0, 0),
    (-1, 0, 0),
    (0, 1, 0),
    (0, -1, 0),
    (0, 0, 1),
    (0, 0, -1),
];
/// How far in front of a face its surface sample sits, in blocks.
pub const FACE_SAMPLE_OFFSET: f64 = 0.45;

/// The point on `target`'s silhouette an eye at `eye` can see: the centre of the nearest
/// face whose neighbour is a known air cell, or the cell centre when no face is exposed.
///
/// One definition, used by the observation's own sightline check
/// ([`crate::perception::Sight::block`]) and by [`can_mine`], so a stance the engine offered
/// as verified cannot be refused by the executor for aiming at a different point of the same
/// block. The live failure this prevents: `runs/level1-live/live-dt.json` event 20, the bot
/// arrived at the stance (12.70, 66.0, 6.57) for the block at (13, 68, 6), and the ray to
/// the block *centre* samples (13, 67, 6) -- the wall beside the bot -- while the block's
/// west face is plainly open.
pub fn facing_point(
    eye: &Position,
    target: Cell,
    mut exposed: impl FnMut(Cell) -> bool,
) -> Position {
    let centre = center(target);
    let raw = (centre.x - eye.x, centre.y - eye.y, centre.z - eye.z);
    let length = (raw.0 * raw.0 + raw.1 * raw.1 + raw.2 * raw.2)
        .sqrt()
        .max(f64::EPSILON);
    let unit = (raw.0 / length, raw.1 / length, raw.2 / length);
    let mut order = [(0.0_f64, 0_usize); 6];
    let mut count = 0;
    for (index, (nx, ny, nz)) in FACE_NORMALS.iter().copied().enumerate() {
        let facing = -(unit.0 * f64::from(nx) + unit.1 * f64::from(ny) + unit.2 * f64::from(nz));
        if facing > 0.0 {
            order[count] = (facing, index);
            count += 1;
        }
    }
    let candidates = &mut order[..count];
    candidates.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (_, index) in candidates.iter().copied() {
        let (nx, ny, nz) = FACE_NORMALS[index];
        if exposed((target.0 + nx, target.1 + ny, target.2 + nz)) {
            return Position {
                x: centre.x + f64::from(nx) * FACE_SAMPLE_OFFSET,
                y: centre.y + f64::from(ny) * FACE_SAMPLE_OFFSET,
                z: centre.z + f64::from(nz) * FACE_SAMPLE_OFFSET,
            };
        }
    }
    centre
}

/// Runtime mining guard, shared with geometry tests. A jump is not a stance.
pub fn can_mine(
    position: &Position,
    target: Cell,
    grounded: bool,
    mut lookup: impl FnMut(Cell) -> Option<Terrain>,
) -> bool {
    if !grounded || !crate::motion::stance_aligned(position) {
        return false;
    }
    let feet = (
        position.x.floor() as i32,
        position.y.round() as i32,
        position.z.floor() as i32,
    );
    if below(feet) == target
        || !lookup(below(feet)).is_some_and(|v| v.support)
        || !lookup(feet).is_some_and(|v| v.air)
        || !lookup((feet.0, feet.1 + 1, feet.2)).is_some_and(|v| v.air)
    {
        return false;
    }
    let eye = Position {
        x: position.x,
        y: position.y + 1.62,
        z: position.z,
    };
    // Aim at the face the eye can see, not at the block centre: the centre is inside the
    // block, so a centre ray can clip a neighbouring cell's corner and refuse a stance whose
    // target is plainly exposed. The observation's own sightline check aims here too.
    let aim = facing_point(&eye, target, |cell| {
        lookup(cell).is_some_and(|terrain| terrain.air)
    });
    let dx = aim.x - eye.x;
    let dy = aim.y - eye.y;
    let dz = aim.z - eye.z;
    if dx * dx + dy * dy + dz * dz > 9.0 {
        return false;
    }
    (1..32).all(|i| {
        let t = f64::from(i) / 32.0;
        let p = (
            (eye.x + dx * t).floor() as i32,
            (eye.y + dy * t).floor() as i32,
            (eye.z + dz * t).floor() as i32,
        );
        p == target || lookup(p).is_some_and(|v| v.air)
    })
}
#[derive(Clone, Default)]
pub struct Terrain {
    pub air: bool,
    pub support: bool,
    /// The harvestable block observed in this cell, whatever it is. One field for every
    /// material: a spruce log, a stone block, dirt and gravel are the same kind of fact,
    /// and [`crate::wood::is_harvestable`] is the only whitelist that decides which names
    /// may appear here.
    pub log: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Access {
    pub target: Position,
    pub log: String,
    pub route: Vec<Position>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TreeView {
    #[serde(default)]
    pub id: u64,
    #[serde(default)]
    pub wood: String,
    pub logs: Vec<Position>,
    pub incomplete: bool,
    /// This tree's own component exceeded the scan limit, so its own verified steps are
    /// withheld, and the memory keeps that loss for the whole epoch. Another tree's
    /// truncation never sets this on a fully enumerated tree.
    #[serde(default)]
    pub capacity_loss: bool,
    pub status: String,
    pub next: Vec<Access>,
    /// Aggregate reasons from this bounded scan and current memory filtering.
    /// Counts never identify a hidden cell or a rejected route.
    #[serde(default)]
    pub diagnostics: TreeDiagnostics,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TreeDiagnostics {
    /// A scan boundary, missing sightline cell, or unresolved remembered wood.
    pub unknown_visibility: bool,
    /// Visible targets without a verified reachable mining stance and clear ray.
    pub stance_or_route: u16,
    /// Verified scan options reserved for support or higher observed wood.
    pub support_or_upper_log: u16,
    /// Current scan options rejected by remembered access dependencies.
    pub memory_veto: u16,
}
impl TreeDiagnostics {
    fn absorb(&mut self, other: &Self) {
        self.unknown_visibility |= other.unknown_visibility;
        self.stance_or_route = self.stance_or_route.saturating_add(other.stance_or_route);
        self.support_or_upper_log = self
            .support_or_upper_log
            .saturating_add(other.support_or_upper_log);
        self.memory_veto = self.memory_veto.saturating_add(other.memory_veto);
    }
}
fn feet(p: Cell) -> Position {
    Position {
        x: f64::from(p.0) + 0.5,
        y: f64::from(p.1),
        z: f64::from(p.2) + 0.5,
    }
}
fn center(p: Cell) -> Position {
    let mut v = feet(p);
    v.y += 0.5;
    v
}
fn below(p: Cell) -> Cell {
    (p.0, p.1 - 1, p.2)
}
fn air(map: &BTreeMap<Cell, Terrain>, p: Cell) -> bool {
    map.get(&p).is_some_and(|v| v.air)
}
fn stand(map: &BTreeMap<Cell, Terrain>, p: Cell) -> bool {
    air(map, p) && air(map, (p.0, p.1 + 1, p.2)) && map.get(&below(p)).is_some_and(|v| v.support)
}
/// How many tree components one scan enumerates. A real spruce forest holds more than
/// eight trees inside the scan window, and a cap smaller than the forest would make every
/// harvest look like dropped data. The bound stays fixed so one observation stays bounded.
pub const MAX_TREES: usize = 24;
/// How many log cells one tree component may hold before its own enumeration counts as
/// truncated. Only that tree's verified steps are withheld; another component's
/// truncation leaves this tree's own verified route intact.
pub const MAX_COMPONENT_LOGS: usize = 128;
/// How many trees the memory may keep unresolved history for. Aligned with `MAX_TREES`
/// so one scan's trees can all be remembered; a smaller cap made a forest look like
/// dropped history and blocked every harvest until the world epoch reset.
pub const MAX_MEMORY_ENTRIES: usize = 24;

pub fn inspect(
    origin: &Position,
    mut lookup: impl FnMut(Cell) -> Option<Terrain>,
) -> Vec<TreeView> {
    let (root, map, parents) = reachable(origin, &mut lookup);
    let mut unseen: BTreeSet<Cell> = map
        .iter()
        .filter(|(_, v)| v.log.is_some())
        .map(|(p, _)| *p)
        .collect();
    let mut trees = Vec::new();
    let mut dropped_trees = 0;
    while let Some(seed) = unseen.pop_first() {
        if trees.len() >= MAX_TREES {
            // The cap stopped enumeration: the remaining components are not offered, and
            // that is reported, but it is no evidence against an enumerated tree's own
            // verified route.
            dropped_trees = unseen.len() + 1;
            break;
        }
        let name = map[&seed].log.as_ref().unwrap();
        let mut component = vec![seed];
        let mut cursor = 0;
        let mut incomplete = false;
        let mut component_truncated = false;
        while cursor < component.len() {
            let p = component[cursor];
            cursor += 1;
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        let q = (p.0 + dx, p.1 + dy, p.2 + dz);
                        if !map.contains_key(&q) {
                            incomplete = true;
                        }
                        if unseen.contains(&q) && map[&q].log.as_ref() == Some(name) {
                            if component.len() >= MAX_COMPONENT_LOGS {
                                incomplete = true;
                                // Truncation of a tree's own component is a loss of that tree's
                                // identity and withholds its steps for the whole epoch. A bulk
                                // material (a stone seam, a grass field) is not a tree: its
                                // component is only a scan group, so its truncation is reported as
                                // an incomplete view instead of poisoning every other target in
                                // the memory for the rest of the epoch.
                                component_truncated = crate::wood::is_usable_log(name);
                            } else {
                                unseen.remove(&q);
                                component.push(q);
                            }
                        }
                    }
                }
            }
        }
        let mut options = Vec::new();
        let mut diagnostics = TreeDiagnostics {
            unknown_visibility: incomplete,
            ..TreeDiagnostics::default()
        };
        for &target in &component {
            let mut best: Option<Vec<Cell>> = None;
            let mut unknown_ray = false;
            for &stance in parents.keys() {
                let dx = f64::from(stance.0 - target.0);
                let dz = f64::from(stance.2 - target.2);
                let dy = f64::from(stance.1 - target.1) + 1.12;
                if dx * dx + dz * dz > 2.25 || dx * dx + dz * dz + dy * dy > 9.0 {
                    continue;
                }
                let mut route = vec![stance];
                let mut p = stance;
                while p != root {
                    p = parents[&p];
                    route.push(p);
                }
                if route.iter().any(|p| below(*p) == target) {
                    continue;
                }
                // Only a clear ray to the exact target; solid leaves/other logs obstruct it.
                let start = Position {
                    x: f64::from(stance.0) + 0.5,
                    y: f64::from(stance.1) + 1.62,
                    z: f64::from(stance.2) + 0.5,
                };
                let end = center(target);
                if (1..32).any(|i| {
                    let t = f64::from(i) / 32.0;
                    let p = (
                        (start.x + (end.x - start.x) * t).floor() as i32,
                        (start.y + (end.y - start.y) * t).floor() as i32,
                        (start.z + (end.z - start.z) * t).floor() as i32,
                    );
                    if p == target {
                        false
                    } else if let Some(terrain) = map.get(&p) {
                        !terrain.air
                    } else {
                        unknown_ray = true;
                        true
                    }
                }) {
                    continue;
                }
                route.reverse();
                if best.as_ref().is_none_or(|old| route.len() < old.len()) {
                    best = Some(route);
                }
            }
            if let Some(route) = best {
                options.push((target, route));
            } else if unknown_ray {
                diagnostics.unknown_visibility = true;
            } else {
                diagnostics.stance_or_route = diagnostics.stance_or_route.saturating_add(1);
            }
        }
        let protected: BTreeSet<Cell> = options
            .iter()
            .flat_map(|(_, route)| route.iter().map(|p| below(*p)))
            .collect();
        let reachable: BTreeSet<Cell> = options.iter().map(|(target, _)| *target).collect();
        let partial = component.iter().any(|p| !reachable.contains(p));
        let before_support_veto = options.len();
        options.retain(|(target, _)| {
            !protected.contains(target)
                && !reachable
                    .iter()
                    .any(|p| p.0 == target.0 && p.2 == target.2 && p.1 > target.1)
        });
        diagnostics.support_or_upper_log = diagnostics
            .support_or_upper_log
            .saturating_add((before_support_veto - options.len()) as u16);
        options.sort_by_key(|(p, route)| (std::cmp::Reverse(p.1), route.len(), *p));
        let next = options
            .into_iter()
            .take(3)
            .map(|(p, route)| Access {
                target: center(p),
                log: name.clone(),
                route: route.into_iter().map(feet).collect(),
            })
            .collect::<Vec<_>>();
        let status = if next.is_empty() {
            "Blocked: no verified harvest stance preserving support and return"
        } else if incomplete || partial {
            "Partial view: boundary or inaccessible wood remains; safe partial harvest available"
        } else {
            "Observed connected wood; harvest upper accessible logs before lower access logs"
        }
        .into();
        trees.push(TreeView {
            id: 0,
            wood: name.clone(),
            logs: component.into_iter().map(center).collect(),
            incomplete,
            capacity_loss: component_truncated,
            status,
            next,
            diagnostics,
        });
    }
    // Dependencies survive component splits: reserve potential rooted steps for
    // higher observed wood, as well as every support used by another component.
    let mut protected: BTreeSet<Cell> = trees
        .iter()
        .flat_map(|tree| tree.next.iter())
        .flat_map(|step| step.route.iter().map(|p| below(cell(p))))
        .collect();
    let mut highest: BTreeMap<(i32, i32, &str), i32> = BTreeMap::new();
    for (&p, terrain) in &map {
        if let Some(name) = terrain.log.as_deref() {
            highest
                .entry((p.0, p.2, name))
                .and_modify(|y| *y = (*y).max(p.1))
                .or_insert(p.1);
        }
    }
    for (&p, terrain) in &map {
        if let Some(name) = terrain.log.as_deref()
            && map
                .get(&below(p))
                .is_some_and(|v| v.support && v.log.is_none())
            && highest.get(&(p.0, p.2, name)).is_some_and(|y| *y > p.1)
        {
            protected.insert(p);
        }
    }
    for tree in &mut trees {
        let before_support_veto = tree.next.len();
        tree.next
            .retain(|step| !protected.contains(&cell(&step.target)));
        tree.diagnostics.support_or_upper_log = tree
            .diagnostics
            .support_or_upper_log
            .saturating_add((before_support_veto - tree.next.len()) as u16);
        // Only a tree whose own component was cut off loses its steps: its own log list is
        // incomplete, so its verified options cannot be trusted. A blanket veto keyed on any
        // dropped component stopped every harvest in a forest denser than the scan cap,
        // which is exactly where a wood goal runs (measured live 2026-09-27).
        if tree.capacity_loss {
            tree.next.clear();
            tree.incomplete = true;
            tree.diagnostics.unknown_visibility = true;
        }
        if tree.next.is_empty() {
            tree.status =
                "Blocked: preserving support or unresolved upper access; no verified next harvest"
                    .into();
        }
        if tree.capacity_loss {
            tree.status.push_str(&format!(
                "; tree scan capacity dropped this tree's own wood beyond the {MAX_COMPONENT_LOGS}-log component limit"
            ));
        }
        if dropped_trees > 0 {
            tree.status.push_str(&format!(
                "; {dropped_trees} further observed tree(s) were beyond the {MAX_TREES}-tree scan cap and are not enumerated this pass"
            ));
        }
    }
    trees
}

fn cell(p: &Position) -> Cell {
    (p.x.floor() as i32, p.y.floor() as i32, p.z.floor() as i32)
}
#[derive(Default)]
pub struct TreeMemory {
    epoch: Option<u64>,
    next_id: u64,
    entries: Vec<TreeEntry>,
    overflow: bool,
}
struct TreeEntry {
    id: u64,
    wood: String,
    ever: BTreeSet<Cell>,
    remaining: BTreeSet<Cell>,
    dependencies: BTreeMap<Cell, BTreeSet<Cell>>,
    incomplete: bool,
}
impl TreeMemory {
    /// Retain known unresolved wood when it leaves the current loaded scan. Only
    /// an actual loaded non-wood cell removes it; an absent observation does not.
    pub fn update(
        &mut self,
        epoch: u64,
        observed: Vec<TreeView>,
        mut is_wood: impl FnMut(Cell, &str) -> Option<bool>,
    ) -> Vec<TreeView> {
        if self.epoch != Some(epoch) {
            *self = Self {
                epoch: Some(epoch),
                ..Self::default()
            };
        }
        // Publish a reconciled empty entry once, then release its slot on the
        // next update. Entries with unresolved wood must keep their history.
        self.entries.retain(|entry| !entry.remaining.is_empty());
        // Inspect withholds a tree's own steps when that tree's component was truncated, and
        // the memory keeps that loss for the whole epoch. Components dropped beyond the tree
        // cap are reported in the status instead: their absence cannot invalidate another
        // tree's verified route.
        self.overflow |= observed.iter().any(|tree| tree.capacity_loss);
        let mut actions: BTreeMap<u64, Vec<Access>> = BTreeMap::new();
        let mut diagnostics: BTreeMap<u64, TreeDiagnostics> = BTreeMap::new();
        for tree in observed {
            let cells: BTreeSet<Cell> = tree.logs.iter().map(cell).collect();
            let matches: Vec<usize> = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| {
                    entry.wood == tree.wood
                        && cells.iter().any(|p| {
                            entry.ever.iter().any(|q| {
                                (p.0 - q.0).abs() <= 1
                                    && (p.1 - q.1).abs() <= 1
                                    && (p.2 - q.2).abs() <= 1
                            })
                        })
                })
                .map(|(i, _)| i)
                .collect();
            let index = if let Some(&index) = matches.first() {
                for &other in matches.iter().skip(1).rev() {
                    let old = self.entries.remove(other);
                    if let Some(next) = actions.remove(&old.id) {
                        actions
                            .entry(self.entries[index].id)
                            .or_default()
                            .extend(next);
                    }
                    if let Some(old_diagnostics) = diagnostics.remove(&old.id) {
                        diagnostics
                            .entry(self.entries[index].id)
                            .or_default()
                            .absorb(&old_diagnostics);
                    }
                    self.entries[index].ever.extend(old.ever);
                    self.entries[index].remaining.extend(old.remaining);
                    self.entries[index].dependencies.extend(old.dependencies);
                    self.entries[index].incomplete |= old.incomplete;
                }
                index
            } else {
                if self.entries.len() >= MAX_MEMORY_ENTRIES {
                    self.overflow = true;
                    continue;
                }
                self.next_id += 1;
                self.entries.push(TreeEntry {
                    id: self.next_id,
                    wood: tree.wood.clone(),
                    ever: BTreeSet::new(),
                    remaining: BTreeSet::new(),
                    dependencies: BTreeMap::new(),
                    incomplete: false,
                });
                self.entries.len() - 1
            };
            let entry = &mut self.entries[index];
            diagnostics
                .entry(entry.id)
                .or_default()
                .absorb(&tree.diagnostics);
            entry.incomplete |= tree.incomplete;
            while entry.ever.len() > 256 {
                if let Some(p) = entry.ever.pop_last() {
                    entry.remaining.remove(&p);
                    entry.dependencies.remove(&p);
                }
                entry.incomplete = true;
                self.overflow = true;
            }
            for step in &tree.next {
                entry.dependencies.insert(
                    cell(&step.target),
                    step.route.iter().map(|p| below(cell(p))).collect(),
                );
            }
            for p in cells {
                if entry.ever.len() >= 256 && !entry.ever.contains(&p) {
                    entry.incomplete = true;
                    self.overflow = true;
                    continue;
                }
                entry.ever.insert(p);
                entry.remaining.insert(p);
            }
            actions.entry(entry.id).or_default().extend(tree.next);
        }
        let mut unknown_by_id = BTreeMap::new();
        for entry in &mut self.entries {
            let mut unknown = false;
            entry.remaining.retain(|p| match is_wood(*p, &entry.wood) {
                Some(present) => present,
                None => {
                    unknown = true;
                    true
                }
            });
            entry
                .dependencies
                .retain(|target, _| entry.remaining.contains(target));
            unknown_by_id.insert(entry.id, unknown);
        }
        // Routes to any unresolved target reserve their support, including
        // support in another memory entry or of another wood family.
        let protected: BTreeSet<Cell> = self
            .entries
            .iter()
            .flat_map(|entry| entry.dependencies.values())
            .flat_map(|supports| supports.iter().copied())
            .collect();
        self.entries
            .iter()
            .map(|entry| {
                let mut next = actions.remove(&entry.id).unwrap_or_default();
                let mut diagnostics = diagnostics.remove(&entry.id).unwrap_or_default();
                diagnostics.unknown_visibility |=
                    entry.incomplete || unknown_by_id[&entry.id];
                let mut targets = BTreeSet::new();
                next.retain(|a| {
                    let p = cell(&a.target);
                    if !entry.remaining.contains(&p) {
                        return false;
                    }
                    let reserved = protected.contains(&p)
                        || (entry
                            .remaining
                            .iter()
                            .any(|q| q.0 == p.0 && q.2 == p.2 && q.1 > p.1)
                            && !entry
                                .remaining
                                .iter()
                                .any(|q| q.0 == p.0 && q.2 == p.2 && q.1 < p.1));
                    if reserved {
                        diagnostics.memory_veto = diagnostics.memory_veto.saturating_add(1);
                        return false;
                    }
                    targets.insert(p)
                });
                next.sort_by(|a, b| b.target.y.total_cmp(&a.target.y));
                // Once capacity drops history, omitted wood may support an action
                // from any retained entry. Only a new world epoch can clear it.
                if self.overflow {
                    next.clear();
                }
                let incomplete = entry.incomplete || unknown_by_id[&entry.id] || self.overflow;
                let status = if self.overflow {
                    format!(
                        "Tree {}: {} known logs remain; blocked: tree memory capacity dropped unresolved wood or access dependencies until world epoch reset",
                        entry.id,
                        entry.remaining.len(),
                    )
                } else {
                    format!(
                        "Tree {}: {} known logs remain; {}; {}",
                        entry.id,
                        entry.remaining.len(),
                        if next.is_empty() {
                            "no verified harvest currently available"
                        } else {
                            "verified harvest available"
                        },
                        if incomplete {
                            "unresolved observation/history boundary"
                        } else {
                            "known wood reconciled against loaded cells"
                        }
                    )
                };
                let status = if next.is_empty() && !self.overflow {
                    format!(
                        "{status}; rejection summary: unknown visibility {}, stance/route {}, support/upper-log {}, memory veto {}",
                        diagnostics.unknown_visibility,
                        diagnostics.stance_or_route,
                        diagnostics.support_or_upper_log,
                        diagnostics.memory_veto,
                    )
                } else {
                    status
                };
                TreeView {
                    id: entry.id,
                    wood: entry.wood.clone(),
                    logs: entry.remaining.iter().map(|p| center(*p)).collect(),
                    incomplete,
                    capacity_loss: self.overflow,
                    status,
                    next,
                    diagnostics,
                }
            })
            .collect()
    }
}

type TerrainMap = BTreeMap<Cell, Terrain>;
type Parents = BTreeMap<Cell, Cell>;
fn reachable(
    origin: &Position,
    mut lookup: impl FnMut(Cell) -> Option<Terrain>,
) -> (Cell, TerrainMap, Parents) {
    let root = (
        origin.x.floor() as i32,
        origin.y.floor() as i32,
        origin.z.floor() as i32,
    );
    let mut map = BTreeMap::new();
    for x in -12..=12 {
        for z in -12..=12 {
            for y in -8..=8 {
                let p = (root.0 + x, root.1 + y, root.2 + z);
                if let Some(value) = lookup(p) {
                    map.insert(p, value);
                }
            }
        }
    }
    let mut parents = BTreeMap::new();
    let mut queue = VecDeque::new();
    if stand(&map, root) {
        parents.insert(root, root);
        queue.push_back(root);
    }
    while let Some(p) = queue.pop_front() {
        if parents.len() >= 1024 {
            break;
        }
        for (x, z) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            for dy in -1..=1 {
                let q = (p.0 + x, p.1 + dy, p.2 + z);
                let top = p.1.max(q.1) + 2;
                // Extra clearance at both ends permits the same one-block jump in reverse.
                if !parents.contains_key(&q)
                    && stand(&map, q)
                    && (dy == 0 || (air(&map, (p.0, top, p.2)) && air(&map, (q.0, top, q.2))))
                {
                    parents.insert(q, p);
                    queue.push_back(q);
                }
            }
        }
    }
    (root, map, parents)
}

/// Find a reversible loaded route to a stable stance close enough to pick up an item.
pub fn pickup_route(
    origin: &Position,
    item: &Position,
    lookup: impl FnMut(Cell) -> Option<Terrain>,
) -> Option<Vec<Position>> {
    let (root, _, parents) = reachable(origin, lookup);
    parents
        .keys()
        .filter_map(|&p| {
            let stance = feet(p);
            if (stance.x - item.x).powi(2)
                + (stance.y - item.y).powi(2)
                + (stance.z - item.z).powi(2)
                > 1.0
            {
                return None;
            }
            let mut route = vec![p];
            let mut at = p;
            while at != root {
                at = parents[&at];
                route.push(at);
            }
            route.reverse();
            Some(route)
        })
        .min_by_key(|route| route.len())
        .map(|route| route.into_iter().map(feet).collect())
}

/// The mover's arrival, transition and tolerance primitives. Re-exported here because
/// this module's callers and tests have always named them at this path; the single
/// definition lives in [`crate::motion`].
pub use crate::motion::{PLAYER_HALF_WIDTH, inside_transition, landed};
