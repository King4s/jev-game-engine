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

#[derive(Clone, Debug, PartialEq)]
pub struct Threat {
    pub kind: String,
    pub position: Position,
    pub distance_m: f64,
}

/// The nearest hostile entity within `radius`, if any. `f64::total_cmp` keeps the
/// ordering total even for a non-finite distance, which the caller filters out.
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
        .filter(|threat| threat.distance_m.is_finite() && threat.distance_m <= radius)
        .min_by(|a, b| a.distance_m.total_cmp(&b.distance_m))
}

/// The nearest hostile entity inside the request-level detection radius.
pub fn nearest_threat(observation: &Observation) -> Option<Threat> {
    nearest_threat_within(observation, THREAT_RADIUS_M)
}

/// Bounded flight goals: observed standable cells that measurably increase the distance
/// to the nearest hostile entity by at least [`MIN_FLEE_GAIN_M`], best first. Empty when
/// no threat is inside [`THREAT_RADIUS_M`] or when no observed cell improves the distance
/// — the honest case where the bot has nothing better than waiting.
///
/// Targets obey the same legality rules as any other navigate candidate: an observed
/// `waypoint:` cell, farther than the arrival tolerance and no more than 12 blocks away.
pub fn flee_candidates(observation: &Observation, duration_ms: u64) -> Vec<Candidate> {
    let Some(threat) = nearest_threat(observation) else {
        return Vec::new();
    };
    let mut scored: Vec<(usize, f64, f64)> = observation
        .blocks
        .iter()
        .enumerate()
        .filter_map(|(index, landmark)| {
            if !landmark.name.starts_with("waypoint:") {
                return None;
            }
            let from_bot = distance(&observation.position, &landmark.position);
            if !(from_bot.is_finite()
                && from_bot <= THREAT_RADIUS_M
                && from_bot > ARRIVAL_TOLERANCE_M)
            {
                return None;
            }
            let from_threat = distance(&threat.position, &landmark.position);
            if !from_threat.is_finite() || from_threat < threat.distance_m + MIN_FLEE_GAIN_M {
                return None;
            }
            Some((index, from_threat, from_bot))
        })
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    scored.truncate(MAX_FLEE_CANDIDATES);
    scored
        .into_iter()
        .map(|(index, from_threat, from_bot)| {
            let landmark = &observation.blocks[index];
            Candidate {
                id: format!("flee_{index}"),
                description: format!(
                    "Move away from {} ({:.1} blocks away): {} ({:.1} blocks away) puts {:.1} \
                     blocks between them; pathfinder must not mine",
                    threat.kind, threat.distance_m, landmark.name, from_bot, from_threat
                ),
                target: Some(landmark.position.clone()),
                duration_ms: duration_ms.clamp(1, 10_000),
            }
        })
        .collect()
}

fn distance(a: &Position, b: &Position) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2) + (a.z - b.z).powi(2)).sqrt()
}
