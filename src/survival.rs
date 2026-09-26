//! Threat-aware goals for the live adapter.
//!
//! The engine's navigation candidates come from observed standable cells. This module
//! adds the ones that name the live survival risk a plain waypoint list does not: a
//! hostile entity close enough to hurt the bot. It is pure — given an observation it
//! returns candidates — so the rule is unit-testable without a server, and the engine
//! decides arrival and stopping exactly as it does for any other bounded goal.
//!
//! Entity names arrive from the adapter as the debug name of the protocol entity kind
//! (`format!("{kind:?}")`, for example `ZombifiedPiglin`), so matching normalises case
//! and separators instead of assuming one spelling. Projectiles are deliberately not
//! threats: walking away from an arrow in flight is not a goal the bot can act on.

use crate::model::{ARRIVAL_TOLERANCE_M, Candidate, Observation, Position};

/// Detection radius used when listing threat-aware candidates in a request.
pub const THREAT_RADIUS_M: f64 = 12.0;
/// A flight target must put at least this much more distance between bot and threat
/// before it is offered as a goal; smaller steps only jitter around the threat.
pub const MIN_FLEE_GAIN_M: f64 = 1.5;
/// Upper bound on flight candidates offered in one request.
pub const MAX_FLEE_CANDIDATES: usize = 3;
/// Reaction threshold for the engine's own safety reflex: a hostile entity this close
/// has to be answered in the tick that notices it, not after a model round trip.
/// It is a reaction threshold, not a guarantee — a sprinting mob can still close it.
pub const REFLEX_RADIUS_M: f64 = 6.0;
/// Minimum gap between two reflex actions, so one persistent threat cannot chain
/// bounded goals into continuous running.
pub const REFLEX_COOLDOWN_MS: u64 = 1_500;
/// Detection and reflex radius for mobs that shoot. A skeleton notices a player within
/// 16 blocks and shoots within 15 with a clear line of sight (Minecraft Wiki, Skeleton);
/// mindcraft's `cowardice` mode reacts to hostiles at the same 16 blocks. Waiting until a
/// shooter is 6 blocks away means standing inside its range for several shots.
pub const RANGED_THREAT_RADIUS_M: f64 = 16.0;
/// Farthest observed cell a flight goal may target, the same bound as any navigation goal.
pub const MAX_FLEE_STEP_M: f64 = 12.0;
/// A flight goal from a shooter must be at least this long: a short hop stays on the
/// line of fire, and the wiki's 3-second shot interval (Normal) leaves time to run.
pub const RANGED_MIN_RUN_M: f64 = 3.0;
/// Walking speed assumed when sizing a flight goal's duration, deliberately low so the
/// bounded goal outlasts the run instead of expiring half-way.
const FLEE_SPEED_M_PER_S: f64 = 2.0;

/// Mobs that attack from range with projectiles aimed along a line of fire.
const RANGED_KINDS: &[&str] = &["bogged", "pillager", "skeleton", "stray"];

/// Hostile entity kinds in the protocol spelling. Neutral mobs that only retaliate,
/// passive animals, players and projectiles are not listed: the bot cannot walk away
/// from a player who chooses to follow, and treating an arrow as a goal is noise.
const HOSTILE_KINDS: &[&str] = &[
    "blaze",
    "bogged",
    "breeze",
    "cave_spider",
    "creeper",
    "drowned",
    "elder_guardian",
    "enderman",
    "endermite",
    "ender_dragon",
    "evoker",
    "ghast",
    "guardian",
    "hoglin",
    "husk",
    "illusioner",
    "magma_cube",
    "phantom",
    "piglin",
    "piglin_brute",
    "pillager",
    "ravager",
    "shulker",
    "silverfish",
    "skeleton",
    "slime",
    "spider",
    "stray",
    "vex",
    "vindicator",
    "warden",
    "witch",
    "wither",
    "wither_skeleton",
    "zoglin",
    "zombie",
    "zombie_villager",
    "zombified_piglin",
];

fn normalize(name: &str) -> String {
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Whether a recorded entity name is a hostile mob the bot should move away from.
pub fn is_hostile(name: &str) -> bool {
    let name = normalize(name);
    !name.is_empty() && HOSTILE_KINDS.iter().any(|kind| normalize(kind) == name)
}

/// Whether a recorded entity name is a hostile mob that shoots from range.
pub fn is_ranged(name: &str) -> bool {
    let name = normalize(name);
    !name.is_empty() && RANGED_KINDS.iter().any(|kind| normalize(kind) == name)
}

/// The radius at which a hostile kind counts as a threat: `radius` for melee mobs, at
/// least [`RANGED_THREAT_RADIUS_M`] for shooters, which hurt from farther away.
fn threat_radius(name: &str, radius: f64) -> f64 {
    if is_ranged(name) {
        radius.max(RANGED_THREAT_RADIUS_M)
    } else {
        radius
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Threat {
    pub kind: String,
    pub position: Position,
    pub distance_m: f64,
}

/// The nearest hostile entity within `radius`, if any; shooters count out to at least
/// [`RANGED_THREAT_RADIUS_M`]. `f64::total_cmp` keeps the ordering total even for a
/// non-finite distance, which the caller filters out.
pub fn nearest_threat_within(observation: &Observation, radius: f64) -> Option<Threat> {
    observation
        .entities
        .iter()
        .filter(|entity| is_hostile(&entity.name))
        .map(|entity| Threat {
            kind: entity.name.clone(),
            position: entity.position.clone(),
            distance_m: distance(&observation.position, &entity.position),
        })
        .filter(|threat| {
            threat.distance_m.is_finite()
                && threat.distance_m <= threat_radius(&threat.kind, radius)
        })
        .min_by(|a, b| a.distance_m.total_cmp(&b.distance_m))
}

/// The nearest hostile entity inside the request-level detection radius.
pub fn nearest_threat(observation: &Observation) -> Option<Threat> {
    nearest_threat_within(observation, THREAT_RADIUS_M)
}

/// Bounded flight goals, best first. Empty when no threat is inside the detection radius
/// or when no observed cell helps — the honest case where the bot has nothing better
/// than waiting.
///
/// From a melee mob, a goal must put at least [`MIN_FLEE_GAIN_M`] more distance between
/// bot and threat, and the farthest-from-threat cells come first. From a shooter, running
/// straight away keeps the bot on the line of fire (a skeleton follows and shoots every
/// few seconds), so a goal must be a real run of at least [`RANGED_MIN_RUN_M`] that does
/// not close the distance, and cells are ranked by how far they move the bot *across* the
/// line of fire, with distance gained as the tie-breaker. A shooter's goal is timed to
/// cover its run at a walking pace, so it keeps running instead of expiring half-way.
///
/// Targets obey the same legality rules as any other navigate candidate: an observed
/// `waypoint:` cell, farther than the arrival tolerance and no more than
/// [`MAX_FLEE_STEP_M`] away.
pub fn flee_candidates(observation: &Observation, duration_ms: u64) -> Vec<Candidate> {
    let Some(threat) = nearest_threat(observation) else {
        return Vec::new();
    };
    let ranged = is_ranged(&threat.kind);
    // Unit vector from threat to bot on the ground plane: the line of fire.
    let (fx, fz) = {
        let dx = observation.position.x - threat.position.x;
        let dz = observation.position.z - threat.position.z;
        let length = (dx * dx + dz * dz).sqrt();
        if length > f64::EPSILON {
            (dx / length, dz / length)
        } else {
            (0.0, 0.0)
        }
    };
    let mut scored: Vec<(usize, f64, f64, f64)> = observation
        .blocks
        .iter()
        .enumerate()
        .filter_map(|(index, landmark)| {
            if !landmark.name.starts_with("waypoint:") {
                return None;
            }
            let from_bot = distance(&observation.position, &landmark.position);
            if !(from_bot.is_finite()
                && from_bot <= MAX_FLEE_STEP_M
                && from_bot > ARRIVAL_TOLERANCE_M)
            {
                return None;
            }
            let from_threat = distance(&threat.position, &landmark.position);
            if !from_threat.is_finite() {
                return None;
            }
            let score = if ranged {
                if from_bot < RANGED_MIN_RUN_M || from_threat < threat.distance_m {
                    return None;
                }
                let vx = landmark.position.x - observation.position.x;
                let vz = landmark.position.z - observation.position.z;
                let across = (fx * vz - fz * vx).abs();
                across + 0.25 * (from_threat - threat.distance_m)
            } else {
                if from_threat < threat.distance_m + MIN_FLEE_GAIN_M {
                    return None;
                }
                from_threat
            };
            Some((index, score, from_threat, from_bot))
        })
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    scored.truncate(MAX_FLEE_CANDIDATES);
    scored
        .into_iter()
        .map(|(index, _, from_threat, from_bot)| {
            let landmark = &observation.blocks[index];
            let (description, duration_ms) = if ranged {
                let run_ms = (from_bot / FLEE_SPEED_M_PER_S * 1_000.0).ceil() as u64;
                (
                    format!(
                        "Run across the line of fire of {} ({:.1} blocks away): {} ({:.1} \
                         blocks away) ends {:.1} blocks from it; pathfinder must not mine",
                        threat.kind, threat.distance_m, landmark.name, from_bot, from_threat
                    ),
                    duration_ms.max(run_ms),
                )
            } else {
                (
                    format!(
                        "Move away from {} ({:.1} blocks away): {} ({:.1} blocks away) puts \
                         {:.1} blocks between them; pathfinder must not mine",
                        threat.kind, threat.distance_m, landmark.name, from_bot, from_threat
                    ),
                    duration_ms,
                )
            };
            Candidate {
                id: format!("flee_{index}"),
                description,
                target: Some(landmark.position.clone()),
                duration_ms: duration_ms.clamp(1, 10_000),
            }
        })
        .collect()
}

fn distance(a: &Position, b: &Position) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2) + (a.z - b.z).powi(2)).sqrt()
}
