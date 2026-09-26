//! Opt-in diagnostic: a bounded connection, optional one Jev request, no block edits.
use anyhow::{Context, Result, bail, ensure};
use jev_game_engine::{
    adapter::{ActionRequest, AdapterCommand, AdapterHandle},
    model::{Candidate, Mode, Observation, Position, Settings},
};
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let value = |key: &str| args.windows(2).find(|p| p[0] == key).map(|p| p[1].clone());
    ensure!(
        !args.iter().any(|s| s == "--action-ms") || value("--action-ms").is_some(),
        "--action-ms requires a value from 250 to 3000"
    );
    let action_ms: u64 = value("--action-ms")
        .unwrap_or_else(|| "500".into())
        .parse()
        .context("--action-ms must be an integer from 250 to 3000")?;
    ensure!(
        (250..=3000).contains(&action_ms),
        "--action-ms must be between 250 and 3000"
    );
    ensure!(
        !args.iter().any(|s| s == "--settle-seconds") || value("--settle-seconds").is_some(),
        "--settle-seconds requires a value from 0 to 30"
    );
    let settle_seconds: u64 = value("--settle-seconds")
        .unwrap_or_else(|| "0".into())
        .parse()
        .context("--settle-seconds must be an integer from 0 to 30")?;
    ensure!(
        settle_seconds <= 30,
        "--settle-seconds must be between 0 and 30"
    );
    ensure!(
        !(args.iter().any(|s| s == "--jev") && args.iter().any(|s| s == "--local-move")),
        "--local-move and --jev are mutually exclusive"
    );
    let settings = Settings {
        mode: Mode::Live,
        port: value("--port").unwrap_or_else(|| "25565".into()).parse()?,
        bot_name: value("--bot").unwrap_or_else(|| "JevBot".into()),
        legacy_forwarding: args.iter().any(|s| s == "--legacy-forwarding"),
        ..Settings::default()
    };
    let mut adapter = jev_game_engine::minecraft::spawn(settings);
    let result = tokio::time::timeout(
        Duration::from_secs(30 + settle_seconds),
        probe(&mut adapter, &args, settle_seconds, action_ms),
    )
    .await
    .context("Probe exceeded its 30-second active budget plus requested settling delay")
    .and_then(|result| result);
    // Always request local stop/disconnect, including provider or observation errors.
    let _ = adapter.commands.send(AdapterCommand::Stop);
    let _ = adapter.commands.send(AdapterCommand::Disconnect);
    let cleanup = tokio::time::timeout(Duration::from_secs(5), &mut adapter.task).await;
    match cleanup {
        Ok(Ok(())) => println!("Disconnected and adapter task finished"),
        Ok(Err(error)) => {
            if result.is_ok() {
                return Err(error.into());
            }
            eprintln!("Adapter cleanup task failed");
        }
        Err(_) => {
            if result.is_ok() {
                bail!("Adapter cleanup timed out");
            }
            eprintln!("Adapter cleanup timed out");
        }
    }
    result
}

async fn probe(
    adapter: &mut AdapterHandle,
    args: &[String],
    settle_seconds: u64,
    action_ms: u64,
) -> Result<()> {
    let started = Instant::now();
    let mut observation = loop {
        if let Some(error) = adapter.errors.borrow().clone() {
            bail!("{error}");
        }
        ensure!(
            !adapter.task.is_finished(),
            "Adapter task ended before receiving an observation"
        );
        if let Some(observation) = adapter
            .observations
            .borrow()
            .clone()
            .filter(|o| o.connected)
        {
            break observation;
        }
        ensure!(
            started.elapsed() < Duration::from_secs(25),
            "No server observation within 25 seconds"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    if settle_seconds > 0 {
        println!(
            "CONNECTED_WAITING_FOR_OPERATOR: settling for {settle_seconds} seconds before selecting an observation or requesting Jev; no movement requested"
        );
        tokio::time::sleep(Duration::from_secs(settle_seconds)).await;
        observation = current_observation(adapter)?;
        println!("SETTLE_COMPLETE: refreshed observation for this probe");
    }
    println!(
        "Observed real Minecraft state: position=({:.2},{:.2},{:.2}), health={}, inventory_slots={}, landmarks={}, entities={}",
        observation.position.x,
        observation.position.y,
        observation.position.z,
        observation.health,
        observation.inventory.len(),
        observation.blocks.len(),
        observation.entities.len()
    );
    let materials: std::collections::BTreeSet<_> = observation
        .blocks
        .iter()
        .filter(|b| !b.name.starts_with("waypoint:"))
        .map(|b| b.name.as_str())
        .collect();
    println!(
        "Observed materials: {materials:?}; verified waypoints: {}",
        observation
            .blocks
            .iter()
            .filter(|b| b.name.starts_with("waypoint:"))
            .count()
    );
    let local_move = args.iter().any(|s| s == "--local-move");
    if args.iter().any(|s| s == "--jev") || local_move {
        let mut candidates = vec![Candidate {
            id: "wait".into(),
            description: "Wait without moving".into(),
            target: None,
            duration_ms: action_ms,
        }];
        let waypoint = if local_move {
            observation
                .blocks
                .iter()
                .filter(|b| b.name.starts_with("waypoint:"))
                .min_by(|a, b| {
                    displacement(&observation.position, &a.position)
                        .total_cmp(&displacement(&observation.position, &b.position))
                })
        } else {
            observation
                .blocks
                .iter()
                .find(|b| b.name.starts_with("waypoint:"))
        };
        if (args.iter().any(|s| s == "--move") || local_move)
            && let Some(landmark) = waypoint
        {
            candidates.push(Candidate {
                id: "navigate".into(),
                description: "Navigate to the verified nearby waypoint without mining".into(),
                target: Some(landmark.position.clone()),
                duration_ms: action_ms,
            });
        }
        let selected = if local_move {
            let selected = candidates
                .into_iter()
                .find(|c| c.id == "navigate")
                .context("No verified nearby waypoint available for local movement diagnostic")?;
            println!(
                "LOCAL_MANUAL_DIAGNOSTIC: deterministic waypoint selection; no Jev request or model decision"
            );
            selected
        } else {
            let key = match std::env::var("TYPESAFE_API_KEY") {
                Ok(key) => key,
                Err(_) => std::fs::read_to_string(
                    std::env::var("TYPESAFE_API_KEY_FILE").context("Set TypeSafe key file")?,
                )
                .context("Unable to read key file")?,
            };
            let decision = jev_game_engine::provider::decide(
                key.trim(),
                &observation,
                &candidates,
                "",
                Duration::from_secs(5),
            )
            .await?;
            println!(
                "Validated live decision: model={}, choice={}, latency_ms={}, probabilities={:?}",
                decision.model, decision.choice, decision.latency_ms, decision.probabilities
            );
            candidates
                .into_iter()
                .find(|c| c.id == decision.choice)
                .context("Invalid choice")?
        };
        ensure!(
            started.elapsed() < Duration::from_secs(30 + settle_seconds),
            "Probe expired"
        );
        let before = current_observation(adapter)?;
        print_position("Before action", &before);
        let (request, acknowledged) = ActionRequest::new(selected, &observation);
        adapter.commands.send(AdapterCommand::Execute(request))?;
        tokio::time::timeout(Duration::from_secs(3), acknowledged)
            .await
            .context("Adapter action acknowledgement timed out")?
            .context("Adapter action acknowledgement channel closed")?
            .map_err(|error| anyhow::anyhow!("Adapter rejected action: {error}"))?;
        println!(
            "Adapter accepted action for observed world epoch {}; duration bound {action_ms} ms",
            observation.world_epoch
        );
        let action_started = Instant::now();
        tokio::time::sleep_until((action_started + Duration::from_millis(action_ms / 2)).into())
            .await;
        let during = current_observation(adapter)?;
        print_position(
            &format!("During action (~{} ms after acceptance)", action_ms / 2),
            &during,
        );
        tokio::time::sleep_until((action_started + Duration::from_millis(action_ms)).into()).await;
        adapter.commands.send(AdapterCommand::Stop)?;
        let at_stop = current_observation(adapter)?;
        print_position(
            &format!("At Stop (~{action_ms} ms after acceptance)"),
            &at_stop,
        );
        println!("Sent local Stop; no block placement/mining requested");
        tokio::time::sleep(Duration::from_millis(500)).await;
        let settled = current_observation(adapter)?;
        print_position("After Stop settle (~500 ms)", &settled);
        println!(
            "Observed displacement: action={:.4} blocks, after_stop={:.4} blocks, total={:.4} blocks",
            displacement(&before.position, &at_stop.position),
            displacement(&at_stop.position, &settled.position),
            displacement(&before.position, &settled.position)
        );
        if displacement(&before.position, &settled.position) < 0.001 {
            println!("No measurable displacement observed; this run does not demonstrate movement");
        }
        if settled.sequence <= before.sequence {
            println!("No newer observation sequence arrived; displacement evidence is stale");
        }
    }
    Ok(())
}

fn current_observation(adapter: &AdapterHandle) -> Result<Observation> {
    if let Some(error) = adapter.errors.borrow().clone() {
        bail!("{error}");
    }
    adapter
        .observations
        .borrow()
        .clone()
        .filter(|o| o.connected)
        .context("No connected observation available during movement probe")
}

fn print_position(label: &str, observation: &Observation) {
    println!(
        "{label}: sequence={}, position=({:.4},{:.4},{:.4}), health={}",
        observation.sequence,
        observation.position.x,
        observation.position.y,
        observation.position.z,
        observation.health
    );
}

fn displacement(a: &Position, b: &Position) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2) + (a.z - b.z).powi(2)).sqrt()
}
