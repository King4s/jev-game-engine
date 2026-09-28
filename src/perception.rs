//! Current-frame visibility, separate from collision and interaction semantics.
//! Sources: pinned Azalea block_state.rs, fluid_state.rs, collision/blocks.rs and
//! clip::traverse_blocks; vanilla 26.2 glass registration (see block-perception.md).
use crate::model::Position;
use crate::wood_tree::facing_point;
use azalea::{
    BlockPos, Vec3,
    block::{
        BlockState,
        fluid_state::{FluidKind, FluidState},
    },
    physics::{clip::traverse_blocks, collision::BlockWithShape},
    registry::builtin::BlockKind,
};
use serde::{Deserialize, Serialize};
use std::{cell::Cell, collections::BTreeMap, sync::LazyLock};

pub const VIEW_DISTANCE: f64 = 16.0;
pub const CELL_BUDGET: usize = 100_000;
/// Per-direction budget for the entity sweep, so block scanning cannot consume it.
pub const ENTITY_CELL_BUDGET: usize = 4_096;
/// Field of view of one view, in degrees. A cell is inside a view when its ray is
/// within half this angle of that view's direction.
pub const VIEW_CONE_DEGREES: f64 = 120.0;
/// cos(120/2 degrees). Kept as a literal because the cone angle is fixed policy.
const VIEW_CONE_COSINE: f64 = 0.5;

/// Horizontal look sectors sampled inside one observation, as a bearing offset and a
/// downward tilt, both in degrees, relative to the bot's own direction. One 120-degree
/// cone about the server-reported direction left the walkable terrain around the bot
/// unknown, so `wood_tree::inspect` could never verify a harvest stance or route and
/// `TreeView.next` stayed empty (see runs/level1-resume/live-turn23.json). A player
/// turns their head and glances at the ground; the frame now does both. The bot's own
/// direction comes first so a truncated budget keeps the view it actually has; the
/// other horizontal sectors are right, left, then behind. The same four bearings are
/// then repeated tilted down, because a ground cell beside the bot sits about 65 degrees
/// below the eye and so falls outside any horizontal cone. The tilt is measured from the
/// horizontal plane, never from an upward view: a bot whose reported pitch points up still
/// samples the ground below it, because `Views::direction` clamps that base pitch.
/// Eight overlapping views cover
/// every bearing and the near ground below; the fixed length keeps the cost bounded.
pub const LOOK_SECTORS: [(f32, f32); 8] = [
    (0.0, 0.0),
    (90.0, 0.0),
    (-90.0, 0.0),
    (180.0, 0.0),
    (0.0, LOOK_TILT_DEGREES),
    (90.0, LOOK_TILT_DEGREES),
    (-90.0, LOOK_TILT_DEGREES),
    (180.0, LOOK_TILT_DEGREES),
];
/// Downward tilt added to the bot's own pitch for the ground-facing sectors. Positive
/// pitch looks down in the client's convention.
pub const LOOK_TILT_DEGREES: f32 = 45.0;

/// Every valid native state has an explicit optical disposition. Unknown blocks
/// stop rays; this does not assert that their real material is opaque.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Optical {
    Clear,
    FluidContextRequired,
    Unverified,
}

pub fn is_air(state: BlockState) -> bool {
    matches!(
        BlockKind::from(state),
        BlockKind::Air | BlockKind::CaveAir | BlockKind::VoidAir
    )
}

pub fn optical(state: BlockState) -> Optical {
    // Native amount can be zero for falling fluid levels; kind determines presence.
    if FluidState::from(state).kind != FluidKind::Empty {
        return Optical::FluidContextRequired;
    }
    let name = state.to_trait().id();
    if is_air(state)
        || name == "glass"
        || name == "glass_pane"
        || name.ends_with("_stained_glass")
        || name.ends_with("_stained_glass_pane")
    {
        Optical::Clear
    } else {
        Optical::Unverified
    }
}

/// Air variants are clear for standing. Other empty shapes are not automatically
/// harmless: fluids, fire and contextual effects require separate rules.
pub fn clear_stance(state: BlockState) -> bool {
    is_air(state)
        && state.is_collision_shape_empty()
        && FluidState::from(state).kind == FluidKind::Empty
}

/// Conservative existing full-block support policy, not a complete hazard model.
pub fn full_support(state: BlockState) -> bool {
    FluidState::from(state).kind == FluidKind::Empty
        && state.is_collision_shape_full()
        && azalea::pathfinder::world::is_block_state_solid(state)
}

// The six face normals and the face sample offset live in `crate::wood_tree`, so this
// observation pass and the executor's stance guard aim at the same point of the same block.
/// A new instance is created for each snapshot. No coordinate/state memory is
/// carried between frames. 120-degree cone is an explicit perception policy.
pub struct Sight {
    eye: Vec3,
    forward: Vec3,
    budget: usize,
    reads: Cell<usize>,
    unknown: Cell<usize>,
}
impl Sight {
    pub fn new(eye: Vec3, yaw_degrees: f32, pitch_degrees: f32) -> Self {
        let yaw = f64::from(yaw_degrees).to_radians();
        let pitch = f64::from(pitch_degrees).to_radians();
        Self {
            eye,
            forward: Vec3::new(
                -yaw.sin() * pitch.cos(),
                -pitch.sin(),
                yaw.cos() * pitch.cos(),
            ),
            budget: CELL_BUDGET,
            reads: Cell::new(0),
            unknown: Cell::new(0),
        }
    }
    pub fn with_budget(mut self, budget: usize) -> Self {
        self.budget = budget.min(CELL_BUDGET);
        self
    }
    /// Capture once, reuse only within this frame. The static cache contains
    /// relative scan order only, never block contents or historical positions.
    pub fn scan(
        &self,
        center: BlockPos,
        lookup: impl Fn(BlockPos) -> Option<BlockState>,
    ) -> VisibleFrame {
        static OFFSETS: LazyLock<Vec<(i32, i32, i32)>> = LazyLock::new(|| {
            let mut offsets: Vec<_> = crate::resources::block_scan_offsets().collect();
            offsets.sort_by_key(|&(x, y, z)| (x * x + y * y + z * z, x, y, z));
            offsets
        });
        let mut cells = BTreeMap::new();
        for &(x, y, z) in OFFSETS.iter() {
            if self.exhausted() {
                break;
            }
            let point = BlockPos::new(center.x + x, center.y + y, center.z + z);
            if let Some(state) = self.block(point, &lookup) {
                cells.insert((point.x, point.y, point.z), state);
            }
        }
        VisibleFrame { cells }
    }
    pub fn reads(&self) -> usize {
        self.reads.get()
    }
    pub fn unknown(&self) -> usize {
        self.unknown.get()
    }
    pub fn exhausted(&self) -> bool {
        self.reads.get() >= self.budget
    }

    pub fn block(
        &self,
        target: BlockPos,
        lookup: impl Fn(BlockPos) -> Option<BlockState>,
    ) -> Option<BlockState> {
        let point = Vec3::new(
            f64::from(target.x) + 0.5,
            f64::from(target.y) + 0.5,
            f64::from(target.z) + 0.5,
        );
        self.visible(point, true, &lookup)
            .then(|| self.sample(target, &lookup))
            .flatten()
    }

    /// Surface targets may be seen at their near face. Entities must also have a
    /// clear target cell, so entities inside a wall cannot leak through this API.
    pub fn visible(
        &self,
        point: Vec3,
        surface: bool,
        lookup: impl Fn(BlockPos) -> Option<BlockState>,
    ) -> bool {
        if ![
            point.x,
            point.y,
            point.z,
            self.eye.x,
            self.eye.y,
            self.eye.z,
            self.forward.x,
            self.forward.y,
            self.forward.z,
        ]
        .iter()
        .all(|v| v.is_finite())
        {
            return false;
        }
        let delta = point - self.eye;
        let distance = delta.length_squared().sqrt();
        if distance > VIEW_DISTANCE || self.exhausted() {
            return false;
        }
        if distance > 0.001
            && (delta.x * self.forward.x + delta.y * self.forward.y + delta.z * self.forward.z)
                / distance
                < VIEW_CONE_COSINE
        {
            return false;
        }
        let target = BlockPos::from(point);
        if distance <= 0.001 {
            return self
                .sample(target, &lookup)
                .is_some_and(|b| optical(b) == Optical::Clear);
        }
        let aim = if surface {
            self.surface_point(point, target, &lookup)
        } else {
            point
        };
        traverse_blocks(
            self.eye,
            aim,
            (),
            |_, cell| {
                let Some(state) = self.sample(cell, &lookup) else {
                    return Some(false);
                };
                if cell == target && surface {
                    return Some(true);
                }
                match optical(state) {
                    Optical::Clear => None,
                    Optical::FluidContextRequired | Optical::Unverified => {
                        self.unknown.set(self.unknown.get() + 1);
                        Some(false)
                    }
                }
            },
            |_| true,
        )
    }
    /// Where a player's gaze meets the target cell: the centre of the face that touches
    /// see-through space and points most toward the eye. A player sees surfaces. Aiming
    /// at a cell's centre makes the ray meet the ground surface in front of a buried
    /// block instead, so the ground under a stance was never observed beyond the bot's
    /// own contact column and no route to a tree could be verified at all.
    fn surface_point(
        &self,
        centre: Vec3,
        target: BlockPos,
        lookup: &impl Fn(BlockPos) -> Option<BlockState>,
    ) -> Vec3 {
        if self
            .sample(target, lookup)
            .is_some_and(|state| optical(state) == Optical::Clear)
        {
            return centre;
        }
        // One definition of "the point on this block this eye can see", shared with the
        // executor's stance guard. Reading through `sample` keeps this pass's own cell budget
        // and unknown-cell accounting.
        let eye = self.eye;
        let point = facing_point(
            &Position {
                x: eye.x,
                y: eye.y,
                z: eye.z,
            },
            (target.x, target.y, target.z),
            |cell| {
                self.sample(BlockPos::new(cell.0, cell.1, cell.2), lookup)
                    .is_some_and(|state| optical(state) == Optical::Clear)
            },
        );
        Vec3::new(point.x, point.y, point.z)
    }

    fn sample(
        &self,
        point: BlockPos,
        lookup: &impl Fn(BlockPos) -> Option<BlockState>,
    ) -> Option<BlockState> {
        if self.exhausted() {
            return None;
        }
        self.reads.set(self.reads.get() + 1);
        let value = lookup(point);
        if value.is_none() {
            self.unknown.set(self.unknown.get() + 1);
        }
        value
    }
}

/// What one look-around frame cost, and whether it was truncated. Reported in the
/// observation so a run is measurable instead of merely asserted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ViewCost {
    /// Look directions actually sampled.
    pub directions: usize,
    /// Ray cell reads spent across every direction of this frame.
    pub reads: usize,
    /// Reads of a cell whose current state was unavailable (unloaded or unknown).
    pub unknown: usize,
    /// A direction stopped early because the shared frame budget ran out, so
    /// omitted cells stay unknown.
    pub exhausted: bool,
}

/// The bot's eye sampling a bounded set of look directions inside one observation.
/// Every direction shares one frame budget, so turning the head cannot make the
/// frame unbounded. Nothing but the shared counters survives a call: a new frame is
/// read from current server-fed state each time.
pub struct Views {
    eye: Vec3,
    yaw: f32,
    pitch: f32,
    budget_per_direction: usize,
    reads: Cell<usize>,
    unknown: Cell<usize>,
    directions: Cell<usize>,
    exhausted: Cell<bool>,
}
impl Views {
    pub fn new(eye: Vec3, yaw_degrees: f32, pitch_degrees: f32) -> Self {
        Self {
            eye,
            yaw: yaw_degrees,
            pitch: pitch_degrees,
            budget_per_direction: CELL_BUDGET,
            reads: Cell::new(0),
            unknown: Cell::new(0),
            directions: Cell::new(0),
            exhausted: Cell::new(false),
        }
    }
    /// Budget per direction; the frame total is this times the sector count.
    pub fn with_budget(mut self, budget: usize) -> Self {
        self.budget_per_direction = budget.min(CELL_BUDGET);
        self
    }
    pub fn cost(&self) -> ViewCost {
        ViewCost {
            directions: self.directions.get(),
            reads: self.reads.get(),
            unknown: self.unknown.get(),
            exhausted: self.exhausted.get(),
        }
    }
    fn total_budget(&self) -> usize {
        self.budget_per_direction.saturating_mul(LOOK_SECTORS.len())
    }
    fn remaining(&self) -> usize {
        let remaining = self.total_budget().saturating_sub(self.reads.get());
        if remaining == 0 {
            self.exhausted.set(true);
        }
        remaining
    }
    fn absorb(&self, sight: &Sight) {
        self.reads.set(self.reads.get() + sight.reads());
        self.unknown.set(self.unknown.get() + sight.unknown());
    }
    fn direction(&self, sector: (f32, f32)) -> Sight {
        let budget = self.remaining();
        // The bot's own sector keeps the reported pitch. A ground-facing sector must look
        // down from the horizontal plane even when the reported pitch is upward: the tilt
        // is a glance at the ground, not a rotation of an upward view.
        let pitch = if sector.1 == 0.0 {
            self.pitch
        } else {
            self.pitch.max(0.0) + sector.1
        }
        .clamp(-90.0, 90.0);
        if budget == 0 {
            return Sight::new(self.eye, self.yaw + sector.0, pitch).with_budget(0);
        }
        self.directions.set(self.directions.get() + 1);
        Sight::new(self.eye, self.yaw + sector.0, pitch).with_budget(budget)
    }
    /// Fuse every sector into one immutable frame. A cell absent from all sectors
    /// stays unknown; a later sector's current state wins for a cell seen twice.
    pub fn scan(
        &self,
        center: BlockPos,
        lookup: impl Fn(BlockPos) -> Option<BlockState>,
    ) -> (VisibleFrame, ViewCost) {
        let mut frame = VisibleFrame::default();
        for sector in LOOK_SECTORS {
            if self.remaining() == 0 {
                break;
            }
            let sight = self.direction(sector);
            let view = sight.scan(center, &lookup);
            self.absorb(&sight);
            if sight.exhausted() {
                self.exhausted.set(true);
            }
            frame.merge(view);
        }
        (frame, self.cost())
    }
    /// True when any sampled sector sees the point. Entities inside a wall stay
    /// hidden: every sector still requires a clear ray through optical-clear cells.
    pub fn visible(
        &self,
        point: Vec3,
        surface: bool,
        lookup: impl Fn(BlockPos) -> Option<BlockState>,
    ) -> bool {
        for sector in LOOK_SECTORS {
            if self.remaining() == 0 {
                return false;
            }
            let sight = self.direction(sector);
            let seen = sight.visible(point, surface, &lookup);
            self.absorb(&sight);
            if seen {
                return true;
            }
        }
        false
    }
}

/// Version-specific facts for a currently visible block. Boxes use block-local
/// coordinates (including native position-dependent shape offsets). These facts
/// deliberately do not turn unimplemented conditional rules into boolean claims.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlockFacts {
    pub position: crate::model::Position,
    pub name: String,
    pub state_id_26_2: u16,
    pub properties: BTreeMap<String, String>,
    pub optical: Optical,
    pub collision_boxes: Vec<[f64; 6]>,
    pub interaction_boxes: Vec<[f64; 6]>,
    pub fluid: String,
    pub fluid_amount: u8,
    pub friction: f32,
    pub jump_factor: f32,
    pub destroy_time: f32,
    pub explosion_resistance: f32,
    pub requires_correct_tool_for_drops: bool,
    pub unresolved: String,
}

pub fn facts(position: BlockPos, state: BlockState) -> BlockFacts {
    let behavior = state.to_trait().behavior();
    let fluid = FluidState::from(state);
    let boxes = |shape: &azalea::physics::collision::VoxelShape| {
        shape
            .to_aabbs()
            .iter()
            .map(|b| [b.min.x, b.min.y, b.min.z, b.max.x, b.max.y, b.max.z])
            .collect()
    };
    BlockFacts {
        position: crate::model::Position {x: f64::from(position.x), y:f64::from(position.y), z:f64::from(position.z)},
        name: BlockKind::from(state).to_str().to_owned(),
        state_id_26_2: state.id(),
        properties: state.to_trait().property_map().into_iter().map(|(k,v)|(k.to_owned(),v.to_owned())).collect(),
        optical: optical(state),
        collision_boxes: boxes(&state.collision_shape(position)),
        interaction_boxes: boxes(&state.outline_shape(position)),
        fluid: match fluid.kind {FluidKind::Empty=>"empty", FluidKind::Water=>"water", FluidKind::Lava=>"lava"}.into(),
        fluid_amount: fluid.amount,
        friction: behavior.friction, jump_factor:behavior.jump_factor,
        destroy_time:behavior.destroy_time, explosion_resistance:behavior.explosion_resistance,
        requires_correct_tool_for_drops:behavior.requires_correct_tool_for_drops,
        unresolved: "Lighting/fog; entity-dependent collision and hazards; placement, drops and interaction conditions; neighbor/tick effects. Static shape and tool flag alone do not prove safety or drops.".into(),
    }
}

/// Observed cells in one immutable frame. A missing key means unknown, not air.
/// Drop this value after producing the observation; execution re-reads the world.
#[derive(Default)]
pub struct VisibleFrame {
    cells: BTreeMap<(i32, i32, i32), BlockState>,
}
impl VisibleFrame {
    /// Fuse another view of the same world into this frame. Cells the other view did
    /// not observe are left untouched, so unknown stays unknown.
    pub fn merge(&mut self, other: VisibleFrame) {
        for (cell, state) in other.cells {
            self.cells.insert(cell, state);
        }
    }
    pub fn get(&self, position: BlockPos) -> Option<BlockState> {
        self.cells
            .get(&(position.x, position.y, position.z))
            .copied()
    }
    pub fn iter(&self) -> impl Iterator<Item = (BlockPos, BlockState)> + '_ {
        self.cells
            .iter()
            .map(|(&(x, y, z), &state)| (BlockPos::new(x, y, z), state))
    }
}
