//! Headless multi-goal session: the unattended counterpart to *Start agent*.
//!
//! It runs one budgeted continuous session through the real engine with the operator's
//! objective, pacing, safety-reflex and budget settings, exports the recording and prints
//! a JSON summary read from that recording. It chooses no goal itself: the model, the
//! opt-in reflex or nobody does. Exit 0 means the engine ended the session on its own
//! budget with the bot still connected; every failure has its own non-zero code.
//!
//! A night on a loopback server:
//! ```text
//! TYPESAFE_API_KEY_FILE=$HOME/.config/jev-loop/typesafe_api_key \
//!   cargo run --bin session-run -- --port <TUNNEL_PORT> --bot <BOT_NAME> --legacy-forwarding \
//!     --objective "Survive the night and keep health" --safety-reflex \
//!     --request-interval-seconds 20 --max-requests 60 --max-seconds 1500 \
//!     --export runs/night.json
//! ```
//! The same loop without a server or a key: `--fixture` (synthetic telemetry, no calls).
use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result, ensure};
use jev_game_engine::{
    harness::{self, MAX_REQUEST_INTERVAL_S, MAX_REQUESTS, MAX_SESSION_SECONDS, SessionOptions},
    model::{FixtureWaypoint, Mode, Settings},
};

const USAGE: &str = "\
session-run: one budgeted multi-goal session, headless, through the real engine.

  --fixture                          run against the offline fixture: no provider, no Minecraft
  --fixture-reachable-waypoint       fixture geometry in which one bounded goal can arrive
  --port <1-65535>                   loopback server port (default 25565)
  --bot <name>                       offline bot identity, alphanumeric/underscore (default JevBot)
  --legacy-forwarding                opt in to legacy forwarding for the configured bot identity
  --objective <text>                 operator's session goal, at most 400 characters
  --safety-reflex                    let the engine dispatch bounded flight goals itself
  --request-interval-seconds <0-86400>  minimum gap between model requests (default 0)
  --max-requests <1-100000>          request budget (default 30)
  --max-seconds <1-604800>           session time budget from Connect (default 300)
  --connect-seconds <1-300>          budget for the first connected observation (default 60)
  --export <file.json>               move the exported recording here (default: under runs/)
  --keep-connected                   leave the bot in the world at the end
  --quiet                            print only the summary

Exit codes: 0 the session ended on its own budget while connected, 1 connection failure,
2 invalid arguments or missing key, 6 provider failure, 7 the bot disconnected during the
session, 8 the session neither ended nor failed inside the wall-clock guard.
The harness never edits blocks and never changes server configuration.";

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("session-run: {error:#}");
            std::process::exit(2)
        }
    }
}

fn run() -> Result<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|argument| argument == "--help") {
        println!("{USAGE}");
        return Ok(0);
    }
    let flag = |name: &str| args.iter().any(|argument| argument == name);
    let value = |name: &str| {
        args.windows(2)
            .find(|pair| pair[0] == name)
            .map(|pair| pair[1].clone())
    };

    let mode = if flag("--fixture") {
        Mode::Demo
    } else {
        Mode::Live
    };
    let bot_name = value("--bot").unwrap_or_else(|| "JevBot".into());
    ensure!(
        !bot_name.is_empty()
            && bot_name.len() <= 16
            && bot_name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
        "--bot must be 1-16 alphanumeric or underscore characters"
    );
    let objective = value("--objective").unwrap_or_default();
    ensure!(
        !flag("--objective") || !objective.trim().is_empty(),
        "--objective requires the session goal as text"
    );
    harness::validate_objective(&objective)?;

    let settings = Settings {
        mode: mode.clone(),
        port: number(&args, "--port", 25_565, 1, 65_535)? as u16,
        bot_name,
        legacy_forwarding: flag("--legacy-forwarding"),
        fixture_waypoint: if flag("--fixture-reachable-waypoint") {
            FixtureWaypoint::Reachable
        } else {
            FixtureWaypoint::Distant
        },
        objective,
        safety_reflex: flag("--safety-reflex"),
        request_interval_ms: number(
            &args,
            "--request-interval-seconds",
            0,
            0,
            MAX_REQUEST_INTERVAL_S,
        )? * 1_000,
        max_requests: number(&args, "--max-requests", 30, 1, MAX_REQUESTS)? as u32,
        max_seconds: number(&args, "--max-seconds", 300, 1, MAX_SESSION_SECONDS)?,
    };
    if mode == Mode::Live {
        ensure!(
            key_is_configured(),
            "Live mode needs TYPESAFE_API_KEY or a readable TYPESAFE_API_KEY_FILE; the key is never printed"
        );
    }
    let options = SessionOptions {
        settings,
        connect_timeout: Duration::from_secs(number(&args, "--connect-seconds", 60, 1, 300)?),
        export: value("--export").map(PathBuf::from),
        keep_connected: flag("--keep-connected"),
        verbose: !flag("--quiet"),
    };
    options.validate()?;

    println!(
        "{} session: objective {:?}, safety reflex {}, pacing {} s, budgets {} requests / {} s",
        match mode {
            Mode::Demo => "Offline fixture",
            Mode::Live => "Live",
        },
        options.settings.objective.trim(),
        options.settings.safety_reflex,
        options.settings.request_interval_ms / 1_000,
        options.settings.max_requests,
        options.settings.max_seconds
    );
    println!("The model or the opt-in reflex selects goals; the engine stops locally.");

    let summary = harness::run_session(&options)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&summary).context("Could not render the summary")?
    );
    match summary.recording_path.as_deref() {
        Some(path) => println!("Recording exported: {path}"),
        None => eprintln!("Recording was not exported; the summary above is from memory only"),
    }
    let code = summary.end_reason.exit_code();
    if code != 0 {
        eprintln!(
            "Session ended: {:?}: {}",
            summary.end_reason, summary.end_detail
        );
    }
    Ok(code)
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
