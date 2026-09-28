//! One observation must see more than the single wedge the bot happens to face.
//!
//! The live recordings in `runs/level1-resume/` show two observed spruce trees with one
//! known log each, `next: []` and no gather candidate: a 120-degree cone about the
//! server-reported look direction left the walkable terrain around the bot unknown, so
//! no harvest stance or route could be verified. These tests pin that defect with the
//! recorded geometry and check the bounded look-around frame that replaces it.
use azalea::{BlockPos, Vec3, block::BlockState, registry::builtin::BlockKind};
use jev_game_engine::{
    harness::safety_reflex_warning,
    model::{Mode, Position},
    perception::{self, LOOK_SECTORS, Sight, Views},
    wood_tree::{self, Access, Cell, Terrain, TreeMemory, TreeView},
};

/// Recorded Start position in `live-turn23.json`: feet at 70.0, eye 1.62 above.
const BOT_FEET: (f64, f64, f64) = (-35.5, 70.0, 80.5);
const EYE: (f64, f64, f64) = (-35.5, 71.62, 80.5);
/// The recorded spruce logs, as block cells (the recording prints block centres):
/// `(-29.5, 69.5, 88.5)` and `(-24.5, 69.5, 90.5)`, 10.0m and 14.9m away.
const NEAR_LOG: Cell = (-30, 69, 88);
const FAR_LOG: Cell = (-25, 69, 90);
/// Solid ground up to and including the bot's own foot level: the recorded log cells sit
/// in that surface layer, and the recording proves they were visible from the bot's eye.
const GROUND_TOP: i32 = 69;
const LOG_NAME: &str = "minecraft:spruce_log";

fn eye() -> Vec3 {
    Vec3::new(EYE.0, EYE.1, EYE.2)
}

fn occupied() -> BlockPos {
    BlockPos::new(
        BOT_FEET.0.floor() as i32,
        BOT_FEET.1.floor() as i32,
        BOT_FEET.2.floor() as i32,
    )
}

fn origin() -> Position {
    Position {
        x: BOT_FEET.0,
        y: BOT_FEET.1,
        z: BOT_FEET.2,
    }
}

fn cell_of(position: &Position) -> Cell {
    (
        position.x.floor() as i32,
        position.y.floor() as i32,
        position.z.floor() as i32,
    )
}

/// A synthetic current world: air, stone ground, the named log cells, and cells the
/// harness must treat as unloaded (no state at all, so they stay unknown).
fn scene<'a>(
    logs: &'a [Cell],
    unloaded: impl Fn(Cell) -> bool + 'a,
) -> impl Fn(BlockPos) -> Option<BlockState> + 'a {
    move |point: BlockPos| {
        let cell = (point.x, point.y, point.z);
        if unloaded(cell) {
            return None;
        }
        let kind = if logs.contains(&cell) {
            BlockKind::SpruceLog
        } else if point.y <= GROUND_TOP {
            BlockKind::Stone
        } else {
            BlockKind::Air
        };
        Some(BlockState::from(kind))
    }
}

/// Only cells the frame actually observed may enter the tree inspector's terrain map,
/// except the bot's own body/support column, which is current contact geometry. This
/// mirrors `minecraft::observe`'s lookup, so the test exercises the same map.
fn terrains<'a, W>(
    frame: &'a perception::VisibleFrame,
    logs: &'a [Cell],
    world: &'a W,
) -> impl Fn(Cell) -> Option<Terrain> + 'a
where
    W: Fn(BlockPos) -> Option<BlockState> + 'a,
{
    let contact = occupied();
    move |cell| {
        let point = BlockPos::new(cell.0, cell.1, cell.2);
        let is_contact = cell.0 == contact.x
            && cell.2 == contact.z
            && (contact.y - 1..=contact.y + 1).contains(&cell.1);
        let block = if is_contact {
            world(point)
        } else {
            frame.get(point)
        }?;
        let kind = BlockKind::from(block);
        Some(Terrain {
            air: perception::is_air(block),
            support: perception::full_support(block),
            log: (kind == BlockKind::SpruceLog && logs.contains(&cell))
                .then(|| kind.to_str().to_owned()),
        })
    }
}

fn inspect_frame<W>(frame: &perception::VisibleFrame, logs: &[Cell], world: &W) -> Vec<TreeView>
where
    W: Fn(BlockPos) -> Option<BlockState>,
{
    wood_tree::inspect(&origin(), terrains(frame, logs, world))
}

fn candidates(trees: &[TreeView]) -> Vec<Access> {
    trees
        .iter()
        .flat_map(|tree| tree.next.iter().cloned())
        .collect()
}

fn tree_with(trees: &[TreeView], log: Cell) -> &TreeView {
    trees
        .iter()
        .find(|tree| tree.logs.iter().any(|p| cell_of(p) == log))
        .unwrap_or_else(|| panic!("no tree view contains {log:?}; views: {trees:?}"))
}

/// The look direction that points at the recorded trunks, measured from the recorded
/// geometry (the bot's eye and the two recorded log cells) rather than invented. The
/// recordings do not store the client's yaw, so no test may claim that value; this is a
/// bearing that must have been in view for the recording to list both trunks among the
/// observed resources.
fn bearing_to_recorded_trunks() -> f32 {
    let mid_x = (NEAR_LOG.0 as f64 + FAR_LOG.0 as f64) / 2.0 + 0.5;
    let mid_z = (NEAR_LOG.2 as f64 + FAR_LOG.2 as f64) / 2.0 + 0.5;
    (-(mid_x - EYE.0)).atan2(mid_z - EYE.2).to_degrees() as f32
}

#[test]
fn one_wedge_misses_the_terrain_the_bounded_lookaround_frame_observes() {
    let logs = [NEAR_LOG, FAR_LOG];
    let loaded = scene(&logs, |_| false);
    let bearing = bearing_to_recorded_trunks();

    // One view in the direction of the recorded trunks does see that wood: the recorded
    // runs listed both trunks as observed resources, so the missing piece was the walkable
    // terrain around the stance, not the wood itself. Without this check the empty
    // candidate list below would be trivially true.
    let wedge = Sight::new(eye(), bearing, 0.0).scan(occupied(), &loaded);
    for log in logs {
        assert!(
            wedge.get(BlockPos::new(log.0, log.1, log.2)).is_some(),
            "the single wedge must see the recorded trunk {log:?}, or this test pins nothing"
        );
    }
    let wedge_trees = inspect_frame(&wedge, &logs, &loaded);
    assert!(
        candidates(&wedge_trees).is_empty(),
        "a single wedge produced a harvest candidate where the live recordings had none"
    );

    // Measured over the whole circle: no single view direction verifies a harvest step,
    // even the directions that do see the wood, while the look-around frame verifies one
    // per recorded trunk at every direction.
    let mut wedge_directions_that_see_wood = 0;
    for step in 0..24 {
        let yaw = bearing + step as f32 * 15.0;
        let one = Sight::new(eye(), yaw, 0.0).scan(occupied(), &loaded);
        if logs
            .iter()
            .any(|log| one.get(BlockPos::new(log.0, log.1, log.2)).is_some())
        {
            wedge_directions_that_see_wood += 1;
        }
        assert!(
            candidates(&inspect_frame(&one, &logs, &loaded)).is_empty(),
            "one wedge about {yaw} degrees verified a harvest step"
        );
        let (around, _cost) = Views::new(eye(), yaw, 0.0).scan(occupied(), &loaded);
        let around_trees = inspect_frame(&around, &logs, &loaded);
        for log in logs {
            assert!(
                !tree_with(&around_trees, log).next.is_empty(),
                "the look-around frame about {yaw} degrees verified no step for {log:?}"
            );
        }
    }
    assert!(
        wedge_directions_that_see_wood >= 2,
        "the sweep must contain directions that do see wood, or it pins nothing"
    );

    // The same terrain from the same eye with the bounded look-around frame, at the same
    // bearing as the wedge above.
    let (frame, cost) = Views::new(eye(), bearing, 0.0).scan(occupied(), &loaded);
    assert_eq!(cost.directions, LOOK_SECTORS.len());
    assert!(cost.reads > 0, "the frame reported no ray reads");
    assert!(!cost.exhausted, "the frame ran out of budget on this scene");
    assert_eq!(
        frame
            .get(BlockPos::new(NEAR_LOG.0, NEAR_LOG.1, NEAR_LOG.2))
            .map(BlockKind::from),
        Some(BlockKind::SpruceLog),
        "the look-around frame must observe the recorded trunk"
    );

    let trees = inspect_frame(&frame, &logs, &loaded);
    // The counters must separate this scene from the unknown-ground one. `unknown_visibility` is
    // conservative by design and stays true here — a bounded frame never observes the cells under
    // the terrain — so the harvestable scene is identified by its stand/route rejection count
    // dropping to zero while the unknown-ground scene reports exactly one.
    let known = tree_with(&trees, NEAR_LOG);
    assert!(
        known.diagnostics.unknown_visibility,
        "the flag is a scan-boundary flag and a bounded frame always has one: {:?}",
        known.diagnostics
    );
    assert_eq!(
        known.diagnostics.stance_or_route, 0,
        "a scene where every target has a verified stance rejects nothing: {:?}",
        known.diagnostics
    );
    assert_eq!(
        known.diagnostics.memory_veto, 0,
        "nothing is vetoed from memory in this scene: {:?}",
        known.diagnostics
    );
    let found = candidates(&trees);
    assert!(
        !found.is_empty(),
        "the look-around frame still produced no harvest candidate: {trees:?}"
    );
    for log in logs {
        let tree = tree_with(&trees, log);
        assert!(
            !tree.next.is_empty(),
            "the look-around frame must verify a candidate for the recorded trunk {log:?}: {tree:?}"
        );
    }
    for step in &found {
        assert!(
            logs.iter().any(|log| cell_of(&step.target) == *log),
            "candidate target is not an observed log: {step:?}"
        );
        assert_eq!(
            step.route.first().map(cell_of),
            Some(occupied_cell()),
            "every route starts where the bot stands"
        );
        for position in &step.route {
            let cell = cell_of(position);
            assert!(
                frame.get(BlockPos::new(cell.0, cell.1, cell.2)).is_some()
                    || cell == occupied_cell(),
                "route cell {cell:?} is not observed and not the bot's own contact cell"
            );
        }
    }
}

fn occupied_cell() -> Cell {
    (occupied().x, occupied().y, occupied().z)
}

#[test]
fn unknown_ground_keeps_blocking_and_the_status_names_the_reason() {
    let logs = [NEAR_LOG, FAR_LOG];
    // The trunks are visible, but the ground the bot would have to stand on beside the
    // near trunk was never loaded. Nothing may be harvested from a guess.
    let unloaded = |cell: Cell| {
        cell.1 == GROUND_TOP
            && (cell.0 - NEAR_LOG.0).abs() <= 1
            && (cell.2 - NEAR_LOG.2).abs() <= 1
            && !logs.contains(&cell)
    };
    let world = scene(&logs, unloaded);
    let (frame, cost) = Views::new(eye(), 180.0, 0.0).scan(occupied(), &world);
    assert!(!cost.exhausted);
    assert_eq!(
        frame
            .get(BlockPos::new(NEAR_LOG.0, NEAR_LOG.1, NEAR_LOG.2))
            .map(BlockKind::from),
        Some(BlockKind::SpruceLog),
        "the near trunk itself is visible; the missing part is the ground beside it"
    );

    let trees = inspect_frame(&frame, &logs, &world);
    let near = tree_with(&trees, NEAR_LOG);
    assert!(
        near.next.is_empty(),
        "a target with unknown standing ground must not become a candidate: {near:?}"
    );
    assert!(
        near.incomplete,
        "the missing cells must be reported as unresolved"
    );
    // The counters must name THIS scene's reason, not just "something blocked it": the ground
    // beside the near trunk is unloaded, so visibility is the unknown part and the other three
    // filters have nothing to report. A regression that counted the wrong category fails here.
    let diagnostics = &near.diagnostics;
    assert!(
        diagnostics.unknown_visibility,
        "unloaded ground beside the target is unknown visibility: {diagnostics:?}"
    );
    assert_eq!(
        diagnostics.stance_or_route, 1,
        "exactly one visible target here has no verified stance: {diagnostics:?}"
    );
    assert_eq!(
        diagnostics.support_or_upper_log, 0,
        "this scene withholds nothing for support or higher wood: {diagnostics:?}"
    );
    assert_eq!(
        diagnostics.memory_veto, 0,
        "no remembered dependency exists in this scene: {diagnostics:?}"
    );

    let mut memory = TreeMemory::default();
    let reconciled = memory.update(1, trees.clone(), |cell, name| {
        if unloaded(cell) {
            None
        } else {
            Some(logs.contains(&cell) && name == LOG_NAME)
        }
    });
    let reported = tree_with(&reconciled, NEAR_LOG);
    assert!(
        reported.status.contains("rejection summary"),
        "blocked status must break the reason down, was {:?}",
        reported.status
    );
    // The status must print the counters themselves, not merely their names: build the line from
    // the tree's own diagnostics and require it verbatim, so a stale or invented number fails.
    let summary = format!(
        "rejection summary: unknown visibility {}, stance/route {}, support/upper-log {}, memory veto {}",
        reported.diagnostics.unknown_visibility,
        reported.diagnostics.stance_or_route,
        reported.diagnostics.support_or_upper_log,
        reported.diagnostics.memory_veto
    );
    assert!(
        reported.status.contains(&summary),
        "the status must carry {summary:?}, was {:?}",
        reported.status
    );
}

#[test]
fn ground_sectors_keep_looking_down_when_the_bot_looks_up() {
    let logs = [NEAR_LOG, FAR_LOG];
    let loaded = scene(&logs, |_| false);
    // The head is tilted up, so a tilt measured from the reported pitch would point at the
    // sky and lose the ground the stance verification depends on.
    let (frame, cost) = Views::new(eye(), 0.0, -60.0).scan(occupied(), &loaded);
    assert!(!cost.exhausted);
    assert_eq!(cost.directions, LOOK_SECTORS.len());
    assert_eq!(
        frame
            .get(BlockPos::new(occupied().x + 2, GROUND_TOP, occupied().z))
            .map(BlockKind::from),
        Some(BlockKind::Stone),
        "a bot looking up must still observe the ground beside it"
    );
    // The bot's own sector still carries the reported view, upward and all.
    assert!(
        frame
            .get(BlockPos::new(occupied().x, occupied().y + 6, occupied().z))
            .is_some(),
        "the reported upward view must still be sampled"
    );
}

#[test]
fn frame_budget_is_bounded_and_no_block_content_survives_a_frame() {
    let logs = [NEAR_LOG, FAR_LOG];
    let small = Views::new(eye(), 0.0, 0.0)
        .with_budget(1)
        .scan(occupied(), scene(&logs, |_| false));
    assert_eq!(
        small.1.directions, 1,
        "a spent budget stops the look-around"
    );
    assert!(
        small.1.exhausted,
        "a truncated frame must report that it ran out of budget"
    );

    let first = Views::new(eye(), 180.0, 0.0).scan(occupied(), scene(&logs, |_| false));
    assert_eq!(
        first
            .0
            .get(BlockPos::new(NEAR_LOG.0, NEAR_LOG.1, NEAR_LOG.2))
            .map(BlockKind::from),
        Some(BlockKind::SpruceLog)
    );
    // The trunk is gone in the next current world: the new frame must re-read state.
    let after = Views::new(eye(), 180.0, 0.0).scan(occupied(), scene(&[], |_| false));
    assert_eq!(
        after
            .0
            .get(BlockPos::new(NEAR_LOG.0, NEAR_LOG.1, NEAR_LOG.2))
            .map(BlockKind::from),
        Some(BlockKind::Stone),
        "a later frame must not repeat an earlier frame's block content"
    );
}

#[test]
fn safety_warning_names_the_risk_only_for_live_wood_without_the_reflex() {
    let warning = safety_reflex_warning(&Mode::Live, true, false)
        .expect("live wood skills without the reflex must warn");
    assert!(warning.starts_with("WARNING:"));
    assert!(
        warning.contains("--safety-reflex"),
        "the warning must name the flag that fixes it: {warning}"
    );
    assert!(
        warning.contains("mobs"),
        "the warning must name the risk: {warning}"
    );
    assert!(safety_reflex_warning(&Mode::Live, true, true).is_none());
    assert!(safety_reflex_warning(&Mode::Live, false, false).is_none());
    assert!(safety_reflex_warning(&Mode::Demo, true, false).is_none());
}
