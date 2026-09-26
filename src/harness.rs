//! Headless multi-goal session harness.
//!
//! This is the unattended counterpart to the desktop app's *Start agent* button: it
//! connects, starts the continuous loop with the operator's settings (objective, pacing,
//! safety reflex, budgets), waits until the engine ends the session on its own budget,
//! exports the recording and summarises it. It drives the real [`EngineHandle`] through
//! the same commands the UI sends, so nothing here chooses a goal: the model, the opt-in
//! reflex or nobody does, exactly as in an interactive session.
//!
//! The library entry point exists so the loop can be exercised offline against the
//! synthetic fixture (`Mode::Demo`) by an integration test, with no provider call and no
//! Minecraft connection. What a fixture run proves is that the harness drives the engine
//! through a whole budgeted session and that its summary agrees with the recording it
//! exported; it proves nothing about Minecraft, about a hostile mob or about the model.

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use serde::Serialize;

use crate::{
    engine::EngineHandle,
    model::{Command, Event, Mode, Observation, Position, Settings, View},
    origin::{ActionOrigin, event_origin},
    recording,
};

const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// Longest an objective may be, matching the engine's own connection validation.
pub const MAX_OBJECTIVE_CHARS: usize = 400;
/// Upper bound on the pacing interval, in seconds (one day).
pub const MAX_REQUEST_INTERVAL_S: u64 = 86_400;
pub const MAX_REQUESTS: u64 = 100_000;
/// Upper bound on the session time budget, in seconds (seven days).
pub const MAX_SESSION_SECONDS: u64 = 604_800;

/// Everything a session run needs; validated by [`SessionOptions::validate`].
#[derive(Clone, Debug)]
pub struct SessionOptions {
    pub settings: Settings,
    /// Budget for the first connected observation.
    pub connect_timeout: Duration,
    /// Where to move the exported recording; `None` leaves it under `runs/`.
    pub export: Option<PathBuf>,
    /// Leave the bot connected at the end instead of stopping and resetting.
    pub keep_connected: bool,
    /// Echo each recorded event to stdout as it appears.
    pub verbose: bool,
}

impl SessionOptions {
    /// Rejects what the engine would reject at `Connect` and what the harness itself
    /// cannot honour, with one line each, so a night run fails before it starts rather
    /// than after the bot has joined.
    pub fn validate(&self) -> Result<()> {
        validate_objective(&self.settings.objective)?;
        ensure!(
            self.settings.request_interval_ms / 1_000 <= MAX_REQUEST_INTERVAL_S,
            "--request-interval-seconds must be from 0 to {MAX_REQUEST_INTERVAL_S}"
        );
        ensure!(
            (1..=MAX_REQUESTS).contains(&u64::from(self.settings.max_requests)),
            "--max-requests must be from 1 to {MAX_REQUESTS}"
        );
        ensure!(
            (1..=MAX_SESSION_SECONDS).contains(&self.settings.max_seconds),
            "--max-seconds must be from 1 to {MAX_SESSION_SECONDS}"
        );
        ensure!(
            !self.connect_timeout.is_zero(),
            "the connect budget must be positive"
        );
        if let Some(path) = &self.export {
            validate_export_path(path)?;
        }
        Ok(())
    }
}

/// The engine's objective rule, exposed so a CLI can fail before connecting.
pub fn validate_objective(objective: &str) -> Result<()> {
    ensure!(
        objective.chars().count() <= MAX_OBJECTIVE_CHARS,
        "--objective must be at most {MAX_OBJECTIVE_CHARS} characters"
    );
    ensure!(
        !objective.chars().any(|c| c.is_control() && c != '\n'),
        "--objective must not contain control characters"
    );
    Ok(())
}

/// An export path must be a `.json` file in an existing directory, so the failure is
/// reported before the session and not after the bot has spent its budget.
pub fn validate_export_path(path: &Path) -> Result<()> {
    ensure!(
        path.extension().is_some_and(|ext| ext == "json"),
        "--export must name a .json file"
    );
    ensure!(!path.is_dir(), "--export must name a file, not a directory");
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    ensure!(
        parent.is_dir(),
        "--export directory does not exist: {}",
        parent.display()
    );
    Ok(())
}

/// How the session ended. Only `Budget` with the bot still connected is a success.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    /// The engine ended the session on its own request or time budget.
    Budget,
    /// No connected observation arrived inside the connect budget, or the adapter failed
    /// before the loop started.
    ConnectionFailed,
    /// The provider (TypeSafe) failed or a key was missing.
    ProviderFailed,
    /// The bot lost its connection while the session was running.
    Disconnected,
    /// The session neither ended nor failed inside the wall-clock guard.
    Stalled,
}

impl EndReason {
    /// Process exit code for a CLI wrapper. Distinct per failure so a script can tell them
    /// apart without parsing text.
    pub fn exit_code(self) -> i32 {
        match self {
            Self::Budget => 0,
            Self::ConnectionFailed => 1,
            Self::ProviderFailed => 6,
            Self::Disconnected => 7,
            Self::Stalled => 8,
        }
    }
}

/// Counts read from the session's recorded events, not from the engine's status text.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Counts {
    pub events: usize,
    pub requests: u32,
    /// Model answers, counted once each on their `decision` event.
    pub answers: u32,
    pub answers_wait: u32,
    pub answers_waypoint: u32,
    pub answers_flee: u32,
    pub answers_other: u32,
    /// Engine safety-reflex actions: `dispatched` rows the attribution rule labels
    /// `SAFETY-REFLEX`. They carry no model decision and are not answers.
    pub reflex_actions: u32,
    pub accepted_actions: u32,
    pub arrival_verdicts: u32,
    pub arrived: u32,
}

/// Summary of one run: the counts, the movement and the end state.
#[derive(Clone, Debug, Serialize)]
pub struct SessionSummary {
    pub mode: String,
    pub objective: String,
    pub safety_reflex: bool,
    pub request_interval_ms: u64,
    pub max_requests: u32,
    pub max_seconds: u64,
    pub end_reason: EndReason,
    pub end_detail: String,
    pub counts: Counts,
    /// Straight-line distance between the first and the last connected observation.
    pub displacement_m: Option<f64>,
    pub final_health: Option<f64>,
    pub connected_at_end: bool,
    pub p50_ms: Option<u64>,
    pub p95_ms: Option<u64>,
    pub recording_path: Option<String>,
}

/// Derives the counts from recorded events. Shared by the live summary and the offline
/// test so the two cannot disagree about what the recording says.
pub fn counts(events: &[Event]) -> Counts {
    let mut counts = Counts {
        events: events.len(),
        ..Counts::default()
    };
    for event in events {
        match event.kind.as_str() {
            "request" => counts.requests += 1,
            "action" => counts.accepted_actions += 1,
            "decision" => {
                if let Some(decision) = &event.decision {
                    counts.answers += 1;
                    let choice = decision.choice.as_str();
                    if choice == "wait" {
                        counts.answers_wait += 1;
                    } else if choice.starts_with("waypoint_") {
                        counts.answers_waypoint += 1;
                    } else if choice.starts_with("flee_") {
                        counts.answers_flee += 1;
                    } else {
                        counts.answers_other += 1;
                    }
                }
            }
            "dispatched" if event_origin(event) == Some(ActionOrigin::SafetyReflex) => {
                counts.reflex_actions += 1;
            }
            _ => {}
        }
        if let Some(verdict) = &event.arrival {
            counts.arrival_verdicts += 1;
            if verdict.arrived {
                counts.arrived += 1;
            }
        }
    }
    counts
}

/// Straight-line displacement between the first and last connected observation recorded
/// on the events, if there are at least two.
pub fn displacement(events: &[Event]) -> Option<f64> {
    let mut positions = events
        .iter()
        .filter_map(|event| event.observation.as_ref())
        .filter(|observation| observation.connected)
        .map(|observation| &observation.position);
    let first = positions.next()?;
    let last = positions.next_back()?;
    Some(distance(first, last))
}

fn distance(a: &Position, b: &Position) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2) + (a.z - b.z).powi(2)).sqrt()
}

/// Runs one budgeted session to its end and returns the summary. Live mode needs a
/// configured key; fixture mode (`Mode::Demo`) makes no provider or Minecraft call.
pub fn run_session(options: &SessionOptions) -> Result<SessionSummary> {
    options.validate()?;
    let settings = &options.settings;
    let engine = EngineHandle::new();
    engine.send(Command::Connect(settings.clone()));

    let mut printed = 0usize;
    let connected = wait_for_observation(
        &engine,
        options.connect_timeout,
        options.verbose,
        &mut printed,
    );
    let (end_reason, end_detail) = match connected {
        Ok(_) => {
            engine.send(Command::Start);
            wait_for_end(&engine, settings, options.verbose, &mut printed)
        }
        Err(detail) => (EndReason::ConnectionFailed, detail),
    };

    let view = engine.snapshot();
    engine.send(Command::Export);
    let mut recording_path = wait_for_recording(&engine, Duration::from_secs(5));
    if let (Some(path), Some(target)) = (&recording_path, &options.export) {
        move_recording(path, target)?;
        recording_path = Some(target.display().to_string());
    }

    if !options.keep_connected {
        engine.send(Command::Stop);
        std::thread::sleep(Duration::from_millis(300));
        engine.send(Command::Reset);
    }

    Ok(summarise(
        settings,
        &view,
        end_reason,
        end_detail,
        recording_path,
    ))
}

fn summarise(
    settings: &Settings,
    view: &View,
    end_reason: EndReason,
    end_detail: String,
    recording_path: Option<String>,
) -> SessionSummary {
    let last_observation = view
        .events
        .iter()
        .rev()
        .find_map(|event| event.observation.as_ref())
        .or(view.observation.as_ref());
    SessionSummary {
        mode: format!("{:?}", settings.mode),
        objective: settings.objective.trim().to_owned(),
        safety_reflex: settings.safety_reflex,
        request_interval_ms: settings.request_interval_ms,
        max_requests: settings.max_requests,
        max_seconds: settings.max_seconds,
        end_reason,
        end_detail,
        counts: counts(&view.events),
        displacement_m: displacement(&view.events),
        final_health: last_observation.map(|observation| f64::from(observation.health)),
        connected_at_end: view
            .observation
            .as_ref()
            .is_some_and(|observation| observation.connected),
        p50_ms: view.p50_ms,
        p95_ms: view.p95_ms,
        recording_path,
    }
}

fn print_new_events(view: &View, printed: &mut usize) {
    for event in view.events.iter().skip(*printed) {
        let origin = match event_origin(event) {
            Some(origin) => format!("{} ", origin.label()),
            None => String::new(),
        };
        println!(
            "  [{:>7} ms] {:<10} {origin}{}",
            event.elapsed_ms, event.kind, event.message
        );
    }
    *printed = view.events.len();
}

fn wait_for_observation(
    engine: &EngineHandle,
    budget: Duration,
    verbose: bool,
    printed: &mut usize,
) -> std::result::Result<Observation, String> {
    let deadline = Instant::now() + budget;
    loop {
        let view = engine.snapshot();
        if verbose {
            print_new_events(&view, printed);
        }
        if let Some(observation) = view.observation.filter(|observation| observation.connected) {
            return Ok(observation);
        }
        if let Some(error) = &view.last_error {
            return Err(format!("connection error: {error}"));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "no connected observation within {} s; status: {}",
                budget.as_secs(),
                view.status
            ));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Waits until the engine ends the session. The engine records the end as an `error`
/// event whose message names the budget; any other error is classified by its text, and
/// a lost connection is read from the observation, not from the text.
fn wait_for_end(
    engine: &EngineHandle,
    settings: &Settings,
    verbose: bool,
    printed: &mut usize,
) -> (EndReason, String) {
    // The session budget runs from Connect; give the engine one more goal horizon plus a
    // margin to notice its own deadline before calling the run stalled.
    let guard = Duration::from_secs(settings.max_seconds) + Duration::from_secs(30);
    let deadline = Instant::now() + guard;
    loop {
        let view = engine.snapshot();
        if verbose {
            print_new_events(&view, printed);
        }
        let connected = view
            .observation
            .as_ref()
            .is_some_and(|observation| observation.connected);
        if let Some(error) = &view.last_error {
            let reason = if error.contains("budget reached") {
                if connected {
                    EndReason::Budget
                } else {
                    EndReason::Disconnected
                }
            } else if !connected
                || error.contains("disconnect")
                || error.contains("Adapter")
                || error.contains("adapter")
            {
                EndReason::Disconnected
            } else {
                EndReason::ProviderFailed
            };
            return (reason, error.clone());
        }
        if !connected && settings.mode == Mode::Live {
            return (
                EndReason::Disconnected,
                format!(
                    "the observation is no longer connected; status: {}",
                    view.status
                ),
            );
        }
        if Instant::now() >= deadline {
            return (
                EndReason::Stalled,
                format!(
                    "the session did not end within {} s; status: {}",
                    guard.as_secs(),
                    view.status
                ),
            );
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn wait_for_recording(engine: &EngineHandle, budget: Duration) -> Option<String> {
    let deadline = Instant::now() + budget;
    loop {
        let view = engine.snapshot();
        if let Some(path) = view.recording_path.clone() {
            return Some(path);
        }
        if view.last_error.as_deref() == Some("Could not save recording") {
            return None;
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Moves the engine's exported recording to the requested path after re-validating it,
/// so `--export` names exactly the file the engine wrote and nothing else.
fn move_recording(from: &str, to: &Path) -> Result<()> {
    recording::load(from).context("The exported recording did not validate")?;
    if std::fs::rename(from, to).is_err() {
        std::fs::copy(from, to)
            .with_context(|| format!("Could not write the recording to {}", to.display()))?;
        std::fs::remove_file(from).ok();
    }
    Ok(())
}
