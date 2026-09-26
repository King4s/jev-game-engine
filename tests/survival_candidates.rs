//! Threat-aware goal selection: what the engine offers when a hostile entity is close.
//!
//! These are the pure rules, so they are checked without a server: which recorded entity
//! names count as hostile, which observed cells become flight goals, and the honest
//! negative cases (no threat, threat out of range, no cell that improves the distance).

use jev_game_engine::model::{Landmark, Observation, Position};
use jev_game_engine::survival::{
    MAX_FLEE_CANDIDATES, THREAT_RADIUS_M, flee_candidates, is_hostile, nearest_threat,
    nearest_threat_within,
};

fn landmark(name: &str, x: f64, y: f64, z: f64) -> Landmark {
    Landmark {
        name: name.into(),
        position: Position { x, y, z },
    }
}

fn observation(entities: Vec<Landmark>, blocks: Vec<Landmark>) -> Observation {
    Observation {
        world_epoch: 1,
        dimension: Some("minecraft:overworld".into()),
        sequence: 7,
        connected: true,
        position: Position {
            x: 0.,
            y: 64.,
            z: 0.,
        },
        health: 20.,
        food: 20.,
        inventory: vec![],
        blocks,
        entities,
        note: "survival test fixture".into(),
    }
}

#[test]
fn hostile_names_match_both_recorded_spellings() {
    // The adapter records the protocol debug name; a fixture or a hand-written
    // observation may use the snake_case form. Both must match.
    for name in [
        "Zombie",
        "ZombifiedPiglin",
        "zombified_piglin",
        "CaveSpider",
        "cave_spider",
        "WitherSkeleton",
        "Bogged",
        "EnderDragon",
    ] {
        assert!(is_hostile(name), "{name} should be hostile");
    }
    for name in [
        "Cow",
        "Sheep",
        "Villager",
        "IronGolem",
        "Player",
        "Arrow",
        "Trident",
        "Fireball",
        "Item",
        "",
        "ZombieHorse",
    ] {
        assert!(!is_hostile(name), "{name} should not be hostile");
    }
}

#[test]
fn the_nearest_hostile_entity_inside_the_radius_is_the_threat() {
    let near = observation(
        vec![
            landmark("Zombie", 9.0, 64.0, 0.0),
            landmark("Skeleton", 2.0, 64.0, 0.0),
            landmark("Cow", 1.0, 64.0, 0.0),
        ],
        vec![],
    );
    let threat = nearest_threat(&near).expect("a threat inside the radius");
    assert_eq!(threat.kind, "Skeleton");
    assert_eq!(threat.distance_m, 2.0);

    let far = observation(
        vec![landmark("Zombie", THREAT_RADIUS_M + 1.0, 64.0, 0.0)],
        vec![],
    );
    assert!(nearest_threat(&far).is_none());
    assert!(nearest_threat_within(&far, THREAT_RADIUS_M + 2.0).is_some());
    assert!(flee_candidates(&far, 2_000).is_empty());
}

#[test]
fn flight_goals_are_ranked_by_gained_distance_and_exclude_worse_cells() {
    let observation = observation(
        vec![landmark("Zombie", 5.0, 64.0, 0.0)],
        vec![
            // Farther from the threat than the bot: the best flight goal.
            landmark("waypoint:-3:64:0", -3.5, 64.0, 0.5),
            // A smaller but sufficient gain.
            landmark("waypoint:-1:64:0", -1.5, 64.0, 0.5),
            // Closer to the threat than the bot: never offered.
            landmark("waypoint:3:64:0", 3.5, 64.0, 0.5),
            // Sideways: gains less than the minimum, so it is noise, not flight.
            landmark("waypoint:0:64:4", 0.5, 64.0, 4.5),
            // Not a navigation landmark at all.
            landmark("minecraft:grass_block", -2.5, 63.0, 0.5),
        ],
    );

    let candidates = flee_candidates(&observation, 2_000);

    let ids: Vec<_> = candidates.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, vec!["flee_0", "flee_1"]);
    assert!(candidates[0].description.contains("Zombie"));
    assert!(candidates[0].description.contains("5.0 blocks away"));
    assert!(candidates[0].target.is_some());
    assert_eq!(candidates[0].duration_ms, 2_000);

    let first = candidates[0].target.clone().unwrap();
    let second = candidates[1].target.clone().unwrap();
    assert!(
        first.x < second.x,
        "best flight goal first, away from the threat"
    );
}

#[test]
fn flight_goals_are_capped_and_the_duration_is_clamped_like_any_other_goal() {
    let blocks = (-6..=6)
        .map(|step| {
            landmark(
                &format!("waypoint:{step}:64:0"),
                step as f64 - 0.5,
                64.0,
                3.5,
            )
        })
        .collect();
    let observation = observation(vec![landmark("Creeper", 1.0, 64.0, 0.0)], blocks);

    let candidates = flee_candidates(&observation, 60_000);
    assert_eq!(candidates.len(), MAX_FLEE_CANDIDATES);
    assert_eq!(
        candidates[0].duration_ms, 10_000,
        "clamped to the engine bound"
    );
}
