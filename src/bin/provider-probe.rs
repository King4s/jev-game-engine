//! One explicit live API contract probe with synthetic state; no game connection.
use anyhow::{Context, Result};
use jev_game_engine::model::{Candidate, Observation, Position};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<()> {
    let key = match std::env::var("TYPESAFE_API_KEY") {
        Ok(key) => key,
        Err(_) => std::fs::read_to_string(
            std::env::var("TYPESAFE_API_KEY_FILE")
                .context("Set TYPESAFE_API_KEY or TYPESAFE_API_KEY_FILE")?,
        )
        .context("Unable to read configured key file")?,
    };
    let observation = Observation {
        world_epoch: 1,
        deaths: 0,
        dimension: Some("fixture:overworld".into()),
        sequence: 1,
        connected: false,
        position: Position {
            x: 0.,
            y: 0.,
            z: 0.,
        },
        health: 20.,
        food: 20.,
        inventory: vec![],
        blocks: vec![],
        entities: vec![],
        note: "Synthetic API contract probe. No game is connected; movement must not occur.".into(),
    };
    let candidates = vec![
        Candidate {
            id: "stop".into(),
            description: "Stop because no game is connected".into(),
            target: None,
            duration_ms: 1_000,
        },
        Candidate {
            id: "wait".into(),
            description: "Wait without movement for a connection".into(),
            target: None,
            duration_ms: 1_000,
        },
    ];
    let decision = jev_game_engine::provider::decide(
        key.trim(),
        &observation,
        &candidates,
        "",
        Duration::from_secs(10),
    )
    .await?;
    println!(
        "Real TypeSafe response to synthetic probe: {}",
        serde_json::to_string(&decision)?
    );
    Ok(())
}
