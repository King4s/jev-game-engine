//! The one mover. Every movement the engine makes — waypoint navigation, harvest
//! approach, collection, flight and placement — is a [`Plan`] handed to [`follow`],
//! which owns the only walk/jump/reorient loop, the only arrival test and the only
//! tolerance constant (`PLAYER_HALF_WIDTH`, the player's own 0.6-wide body). A goal
//! that needs a new kind of step adds it here; no other module may walk the bot or
//! invent a tolerance of its own.
use crate::model::Position;

/// Half of the player's 0.6-wide body. One tolerance governs every stance, transition
/// and swept-volume check in the engine. A tighter box rejected real, completing steps
/// (live evidence: a bot stood 0.26 blocks past the next cell centre, inside its own
/// body) — never reintroduce a per-module width.
pub const PLAYER_HALF_WIDTH: f64 = 0.3;

/// How far inside its own cell a standing player's centre must be: `0.5 +/- this`.
/// Derived from the body width, so one change moves every stance check at once.
pub const STANCE_INSET: f64 = 0.5 - PLAYER_HALF_WIDTH;

/// Arrival tolerance of one landing, in blocks.
pub const LANDED_HORIZONTAL_M: f64 = 0.15;
pub const LANDED_VERTICAL_M: f64 = 0.04;

/// A stable stance: finite, feet level with the cell floor, body inside the cell.
pub fn stance_aligned(position: &Position) -> bool {
    [position.x, position.y, position.z]
        .iter()
        .all(|p| p.is_finite())
        && (position.y - position.y.round()).abs() <= LANDED_VERTICAL_M
        && (0.5 - STANCE_INSET..=0.5 + STANCE_INSET).contains(&(position.x - position.x.floor()))
        && (0.5 - STANCE_INSET..=0.5 + STANCE_INSET).contains(&(position.z - position.z.floor()))
}

/// The single arrival test: grounded, level with the target cell floor and inside
/// [`LANDED_HORIZONTAL_M`] of its centre.
pub fn landed(position: &Position, target: &Position, grounded: bool) -> bool {
    grounded
        && (position.x - target.x).hypot(position.z - target.z) <= LANDED_HORIZONTAL_M
        && (position.y - target.y).abs() <= LANDED_VERTICAL_M
}

/// The one transition test for one cardinal step: the body may sit up to its own
/// half-width past a cell centre while the step completes.
pub fn inside_transition(position: &Position, from: &Position, to: &Position) -> bool {
    position.x >= from.x.min(to.x) - PLAYER_HALF_WIDTH
        && position.x <= from.x.max(to.x) + PLAYER_HALF_WIDTH
        && position.z >= from.z.min(to.z) - PLAYER_HALF_WIDTH
        && position.z <= from.z.max(to.z) + PLAYER_HALF_WIDTH
        && position.y >= from.y.min(to.y) - 0.1
        && position.y <= from.y.max(to.y) + 1.3
}

/// The one stance test for work done without walking: the observed position is still the
/// stance the work was verified from. Placement stands still and holds this stance while
/// it reaches an observed site.
pub fn holds_stance(position: &Position, stance: &Position) -> bool {
    (position.x - stance.x).abs() <= LANDED_HORIZONTAL_M
        && (position.y - stance.y).abs() <= LANDED_HORIZONTAL_M
        && (position.z - stance.z).abs() <= LANDED_HORIZONTAL_M
}

/// The world-facing side of the mover: what it can read, verify and ask for. The
/// native adapter implements it over the live client; tests implement it over a
/// scripted world, so one loop serves both.
pub trait Nav {
    fn position(&self) -> Result<Position, &'static str>;
    fn grounded(&self) -> Result<bool, &'static str>;
    /// Revalidate a whole loaded route before taking its next step. The route itself
    /// may only be rejected here; no alternate route may be invented.
    fn verify_route(&mut self, route: &[Position]) -> Result<(), &'static str>;
    /// Fresh bounded sight of the next swept transition. `Ok(false)` pauses the step
    /// honestly, without authorizing a different one.
    fn observed_step(
        &mut self,
        position: &Position,
        from: &Position,
        to: &Position,
        grounded: bool,
    ) -> Result<bool, &'static str>;
    /// One walk/jump decision, in the world's own movement verbs.
    fn step(&mut self, forward: bool, jump: bool);
    /// Hand a long-range goal to the world's own pathfinder (observed standable cell).
    fn start_goal(&mut self, target: &Position) -> Result<(), &'static str>;
}

/// What a caller wants the bot to do next.
pub enum Plan<'a> {
    /// A verified cardinal route: follow it step by step, in order.
    Route {
        route: &'a [Position],
        index: &'a mut usize,
    },
    /// An observed standable cell, reached by the world's own pathfinder.
    Goal { target: &'a Position },
}

impl Plan<'_> {
    /// The movement family this plan belongs to, so a caller and a test can name the
    /// same mover without matching on the payload.
    pub fn kind(&self) -> &'static str {
        match self {
            Plan::Route { .. } => "route",
            Plan::Goal { .. } => "goal",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Progress {
    /// The plan is complete: the route ended or the goal is already reached.
    Arrived,
    /// Still moving; the caller keeps ticking.
    Advancing,
}

/// The single entry point of the validator. A goal already inside the arrival
/// tolerance is `Arrived` without touching the world; otherwise the plan is enqueued
/// and the caller re-observes.
pub fn follow(nav: &mut dyn Nav, plan: Plan<'_>) -> Result<Progress, &'static str> {
    match plan {
        Plan::Route { route, index } => follow_route(nav, route, index),
        Plan::Goal { target } => {
            let position = nav.position()?;
            if landed(&position, target, true) {
                return Ok(Progress::Arrived);
            }
            nav.start_goal(target)?;
            Ok(Progress::Advancing)
        }
    }
}

/// The one walk/jump/reorient loop. Steps are taken only between adjacent cells of a
/// route that was verified when it was planned and is re-verified against the loaded
/// world before every step; the next transition needs fresh sight.
pub fn follow_route(
    nav: &mut dyn Nav,
    route: &[Position],
    index: &mut usize,
) -> Result<Progress, &'static str> {
    let target = route.get(*index).ok_or("Motion route missing")?.clone();
    let from = route[(*index).saturating_sub(1)].clone();
    let position = nav.position()?;
    let grounded = nav.grounded()?;
    if !inside_transition(&position, &from, &target) {
        return Err("Motion left the verified transition");
    }
    nav.verify_route(route)?;
    if !landed(&position, &target, grounded)
        && !nav.observed_step(&position, &from, &target, grounded)?
    {
        nav.step(false, false);
        return Ok(Progress::Advancing);
    }
    if landed(&position, &target, grounded) {
        nav.step(false, false);
        if *index + 1 == route.len() {
            return Ok(Progress::Arrived);
        }
        *index += 1;
    } else {
        nav.step(true, grounded && target.y > position.y + 0.25);
    }
    Ok(Progress::Advancing)
}
