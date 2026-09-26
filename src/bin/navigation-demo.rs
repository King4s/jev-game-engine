//! Opt-in operator harness for ONE model-selected bounded navigation goal on a
//! loopback Minecraft server.
//!
//! It drives the real engine (`EngineHandle`) through the same code path as the
//! desktop UI: one observation snapshot, one model decision over the bounded
//! candidates, one bounded local action, the engine's own arrival verdict and the
//! local Stop. It never picks a goal itself, so it is not the manual takeover
//! diagnostic, and it does not claim that the model chose a sensible target.
//!
//! It prints the correlated chain as JSON so a live run can be inspected and
//! exported, and exits with 0 only when the engine recorded a reached target
//! inside the arrival tolerance.
//!
//! Run from the project root so the exported recording lands in `runs/`:
//! ```text
//! TYPESAFE_API_KEY_FILE=$HOME/.config/jev-loop/typesafe_api_key \
//!   cargo run --bin navigation-demo -- --port <TUNNEL_PORT> --bot <BOT_NAME> --legacy-forwarding
//! ```
use anyhow::{Context, Result, ensure};
use jev_game_engine::{
    engine::EngineHandle,
    model::{ArrivalVerdict, Command, Event, Mode, Observation, Settings, View},
};
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(50);

const USAGE: &str = "\
navigation-demo: one model-selected bounded navigation goal on a loopback server.

  --port <1-65535>            loopback server port (default 25565)
  --bot <name>               offline bot identity, alphanumeric/underscore (default JevBot)
  --legacy-forwarding        opt in to legacy forwarding for the configured bot identity
  --connect-seconds <1-300>  budget for the first fresh observation (default 60)
  --goal-seconds <1-300>     budget for one decision plus its bounded local action (default 30)
  --session-seconds <1-3600> engine session time budget (default 180)
  --keep-connected           do not disconnect at the end (leave the bot in the world)

Exit codes: 0 target reached inside the tolerance, 3 the bounded goal expired without
arrival, 4 no arrival verdict was recorded, 1 connection, provider or budget failure.
The harness never edits blocks, never changes server configuration and never sends a
second goal: the engine's request budget for this run is one request.";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|argument| argument == "--help") {
        println!("{USAGE}");
        return Ok(());
    }

    let port = number(&args, "--port", 25_565, 1, 65_535)? as u16;
    let bot_name = args
        .windows(2)
        .find(|pair| pair[0] == "--bot")
        .map(|pair| pair[1].clone())
        .unwrap_or_else(|| "JevBot".into());
    let connect_seconds = number(&args, "--connect-seconds", 60, 1, 300)?;
    let goal_seconds = number(&args, "--goal-seconds", 30, 1, 300)?;
    let session_seconds = number(&args, "--session-seconds", 180, 1, 3_600)?;
    let keep_connected = args.iter().any(|argument| argument == "--keep-connected");

    ensure!(
        connect_seconds + goal_seconds <= session_seconds,
        "--connect-seconds plus --goal-seconds must fit inside --session-seconds, because the \
         engine session budget starts when it connects"
    );
    ensure!(
        !bot_name.is_empty()
            && bot_name.len() <= 16
            && bot_name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
        "--bot must be 1-16 alphanumeric or underscore characters"
    );
    ensure!(
        key_is_configured(),
        "Live mode needs TYPESAFE_API_KEY or a readable TYPESAFE_API_KEY_FILE; the key is never printed"
    );

    let settings = Settings {
        mode: Mode::Live,
        port,
        bot_name: bot_name.clone(),
        legacy_forwarding: args
            .iter()
            .any(|argument| argument == "--legacy-forwarding"),
        // Exactly one bounded goal: the engine stops on its own request budget.
        max_requests: 1,
        max_seconds: session_seconds,
        // Live mode never builds the offline fixture, so its geometry is irrelevant here.
        ..Settings::default()
    };

    println!(
        "Live Jev-selected navigation attempt: port {port}, bot {bot_name}, one bounded goal, \
         connect budget {connect_seconds}s, goal budget {goal_seconds}s"
    );
    println!("The model selects the goal; the engine decides arrival and stops locally.");

    let engine = EngineHandle::new();
    engine.send(Command::Connect(settings));
    let observation = wait_for_observation(&engine, Duration::from_secs(connect_seconds))
        .context("The live adapter never published a connected observation")?;
    println!(
        "Observation {}: position ({:.2}, {:.2}, {:.2}), dimension {}, health {:.0}, \
         {} observed blocks, note: {}",
        observation.sequence,
        observation.position.x,
        observation.position.y,
        observation.position.z,
        observation.dimension.as_deref().unwrap_or("unknown"),
        observation.health,
        observation.blocks.len(),
        observation.note
    );

    engine.send(Command::Step);
    let outcome = wait_for_arrival(&engine, Duration::from_secs(goal_seconds));

    let view = engine.snapshot();
    let report = report(&view, &outcome);
    println!(
        "{}",
        serde_json::to_string_pretty(&report).context("Could not render the report")?
    );

    engine.send(Command::Export);
    if let Some(path) = wait_for_recording(&engine, Duration::from_secs(5)) {
        println!("Recording exported: {path}");
    } else {
        println!("Recording was not exported; inspect the live events above");
    }

    if !keep_connected {
        engine.send(Command::Stop);
        std::thread::sleep(Duration::from_millis(500));
        engine.send(Command::Reset);
    }

    match outcome {
        Outcome::Arrived { .. } => Ok(()),
        Outcome::Expired { .. } => {
            eprintln!("The bounded goal expired without arriving inside the tolerance");
            std::process::exit(3)
        }
        Outcome::NoVerdict(message) => {
            eprintln!("No arrival verdict was recorded: {message}");
            std::process::exit(4)
        }
        Outcome::Failed(message) => {
            eprintln!("The attempt failed before a verdict: {message}");
            std::process::exit(1)
        }
    }
}

enum Outcome {
    Arrived(Box<ArrivalVerdict>),
    Expired(Box<ArrivalVerdict>),
    NoVerdict(String),
    Failed(String),
}

fn wait_for_observation(engine: &EngineHandle, budget: Duration) -> Option<Observation> {
    let deadline = Instant::now() + budget;
    loop {
        let view = engine.snapshot();
        if let Some(observation) = view.observation.filter(|observation| observation.connected) {
            return Some(observation);
        }
        if let Some(error) = &view.last_error {
            eprintln!("Connection error: {error}");
            return None;
        }
        if Instant::now() >= deadline {
            eprintln!(
                "No observation within the connect budget; status: {}",
                view.status
            );
            return None;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn wait_for_arrival(engine: &EngineHandle, budget: Duration) -> Outcome {
    let deadline = Instant::now() + budget;
    let mut printed = 0usize;
    loop {
        let view = engine.snapshot();
        for event in view.events.iter().skip(printed) {
            println!(
                "  [{:>6} ms] {:<10} {}",
                event.elapsed_ms, event.kind, event.message
            );
        }
        printed = view.events.len();

        if let Some(verdict) = verdict(&view) {
            return match verdict.arrived {
                true => Outcome::Arrived(Box::new(verdict)),
                false => Outcome::Expired(Box::new(verdict)),
            };
        }
        if let Some(error) = &view.last_error {
            return Outcome::Failed(error.clone());
        }
        if Instant::now() >= deadline {
            return Outcome::NoVerdict(format!(
                "the goal budget of {}s elapsed; status: {}",
                budget.as_secs(),
                view.status
            ));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn verdict(view: &View) -> Option<ArrivalVerdict> {
    view.events
        .iter()
        .find(|event| event.kind == "executor" && event.arrival.is_some())
        .and_then(|event| event.arrival.clone())
}

fn wait_for_recording(engine: &EngineHandle, budget: Duration) -> Option<String> {
    let deadline = Instant::now() + budget;
    loop {
        let view = engine.snapshot();
        if let Some(path) = view.recording_path.clone() {
            return Some(path);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// The correlated chain, as recorded by the engine, without interpreting it.
fn report(view: &View, outcome: &Outcome) -> serde_json::Value {
    let event = |kind: &str| -> Option<&Event> { view.events.iter().find(|e| e.kind == kind) };
    let selected = event("decision")
        .and_then(|event| event.decision.as_ref())
        .map(|decision| {
            serde_json::json!({
                "choice": decision.choice,
                "probabilities": decision.probabilities,
                "confidence": decision.confidence,
                "model": decision.model,
                "latency_ms": decision.latency_ms,
            })
        });
    let candidates = event("request")
        .map(|event| {
            event
                .candidates
                .iter()
                .map(|candidate| {
                    serde_json::json!({
                        "id": candidate.id,
                        "description": candidate.description,
                        "target": candidate.target,
                        "duration_ms": candidate.duration_ms,
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let arrival = match outcome {
        Outcome::Arrived(verdict) | Outcome::Expired(verdict) => serde_json::json!({
            "arrived": verdict.arrived,
            "measured_distance_m": verdict.measured_distance_m,
            "tolerance_m": verdict.tolerance_m,
            "elapsed_ms": verdict.elapsed_ms,
            "duration_ms": verdict.duration_ms,
            "target": verdict.target,
        }),
        Outcome::NoVerdict(_) | Outcome::Failed(_) => serde_json::Value::Null,
    };
    let result = match outcome {
        Outcome::Arrived(_) => "arrived",
        Outcome::Expired(_) => "expired_without_arrival",
        Outcome::NoVerdict(_) => "no_verdict",
        Outcome::Failed(_) => "failed",
    };
    serde_json::json!({
        "result": result,
        "model_selected": true,
        "manual_takeover_used": false,
        "requests": view.requests,
        "p50_ms": view.p50_ms,
        "p95_ms": view.p95_ms,
        "candidates": candidates,
        "decision": selected,
        "arrival_verdict": arrival,
        "status": view.status,
        "last_error": view.last_error,
        "trace": view.events.iter().map(|event| serde_json::json!({
            "sequence": event.sequence,
            "elapsed_ms": event.elapsed_ms,
            "kind": event.kind,
            "message": event.message,
        })).collect::<Vec<_>>(),
    })
}

fn number(args: &[String], flag: &str, default: u64, low: u64, high: u64) -> Result<u64> {
    let raw = args
        .windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone());
    let Some(raw) = raw else {
        return Ok(default);
    };
    let value: u64 = raw
        .parse()
        .with_context(|| format!("{flag} must be an integer from {low} to {high}"))?;
    ensure!(
        (low..=high).contains(&value),
        "{flag} must be between {low} and {high}"
    );
    Ok(value)
}

/// Presence check only: the key value stays in the environment or the key file.
fn key_is_configured() -> bool {
    if std::env::var("TYPESAFE_API_KEY")
        .ok()
        .is_some_and(|key| !key.trim().is_empty())
    {
        return true;
    }
    std::env::var("TYPESAFE_API_KEY_FILE")
        .ok()
        .and_then(|path| std::fs::metadata(path).ok())
        .is_some_and(|metadata| metadata.is_file() && metadata.len() <= 16_384)
}
