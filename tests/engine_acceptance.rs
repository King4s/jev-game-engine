use std::time::{Duration, Instant};

use jev_game_engine::{engine::EngineHandle, model::*};

fn wait_for(engine: &EngineHandle, description: &str, predicate: impl Fn(&View) -> bool) -> View {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let view = engine.snapshot();
        if predicate(&view) {
            return view;
        }
        assert!(
            Instant::now() < deadline,
            "Timed out waiting for {description}; status={}, error={:?}, events={:?}",
            view.status,
            view.last_error,
            view.events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Negative assertions must cover the delayed fixture response, not one snapshot.
fn remains(engine: &EngineHandle, predicate: impl Fn(&View) -> bool) {
    let deadline = Instant::now() + Duration::from_millis(450);
    loop {
        let view = engine.snapshot();
        assert!(predicate(&view), "Unexpected later state: {view:?}");
        if Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn connect(settings: Settings) -> EngineHandle {
    let engine = EngineHandle::new();
    engine.send(Command::Connect(settings));
    wait_for(&engine, "connected fixture", |v| {
        v.observation.as_ref().is_some_and(|o| o.connected)
    });
    engine
}

fn count(view: &View, kind: &str) -> usize {
    view.events.iter().filter(|e| e.kind == kind).count()
}

fn manual_wait(view: &View) -> Command {
    Command::Manual {
        candidate: Candidate {
            id: "wait".into(),
            description: "Wait without moving".into(),
            target: None,
            duration_ms: view.goal_ms,
        },
        world_epoch: view.observation.as_ref().map_or(0, |o| o.world_epoch),
        dimension: view.observation.as_ref().and_then(|o| o.dimension.clone()),
    }
}

fn pending(engine: &EngineHandle) {
    engine.send(Command::Start);
    let view = wait_for(engine, "pending fixture request", |v| {
        count(v, "request") == 1
    });
    assert_eq!(
        count(&view, "action"),
        0,
        "Fixture response must still be pending for this cancellation test"
    );
}

#[test]
fn step_executes_exactly_one_bounded_goal_with_truthful_fixture_provenance() {
    let engine = connect(Settings::default());
    engine.send(Command::Step);
    let view = wait_for(&engine, "completed single goal", |v| {
        count(v, "executor") == 1
    });
    assert_eq!(view.requests, 1);
    assert_eq!(count(&view, "action"), 1);
    assert!(view.active_goal.is_none());
    let decision = view
        .events
        .iter()
        .find_map(|e| e.decision.as_ref())
        .unwrap();
    assert_eq!(decision.model, "offline-fixture");
    assert!(decision.confidence.is_none());
    assert!(decision.probabilities.is_empty());
    assert!(view.p50_ms.is_some());
    remains(&engine, |v| {
        v.requests == 1 && count(v, "action") == 1 && v.active_goal.is_none()
    });
}

#[test]
fn pause_invalidates_pending_answer_and_keeps_fixture_connected() {
    let engine = connect(Settings::default());
    pending(&engine);
    engine.send(Command::Pause);
    wait_for(&engine, "paused", |v| v.status.starts_with("Paused"));
    remains(&engine, |v| {
        count(v, "action") == 0
            && v.active_goal.is_none()
            && v.observation.as_ref().is_some_and(|o| o.connected)
    });
}

#[test]
fn stop_invalidates_pending_answer_and_prevents_later_actions() {
    let engine = connect(Settings::default());
    pending(&engine);
    engine.send(Command::Stop);
    wait_for(&engine, "stopped", |v| v.status == "Stopped");
    remains(&engine, |v| {
        count(v, "action") == 0
            && v.active_goal.is_none()
            && v.observation.as_ref().is_some_and(|o| !o.connected)
    });
}

#[test]
fn reset_invalidates_pending_answer_and_clears_previous_session() {
    let engine = connect(Settings::default());
    pending(&engine);
    engine.send(Command::Reset);
    wait_for(&engine, "reset", |v| {
        v.status == "Ready" && v.events.is_empty()
    });
    remains(&engine, |v| {
        v.events.is_empty() && v.requests == 0 && v.observation.is_none() && v.active_goal.is_none()
    });
}

#[test]
fn continuous_mode_stops_at_request_budget_without_an_extra_request() {
    let engine = connect(Settings {
        max_requests: 1,
        ..Settings::default()
    });
    engine.send(Command::Start);
    let view = wait_for(&engine, "request budget stop", |v| {
        v.last_error.as_deref() == Some("Request budget reached")
    });
    assert_eq!(view.requests, 1);
    assert_eq!(count(&view, "action"), 1);
    remains(&engine, |v| {
        v.requests == 1 && count(v, "action") == 1 && v.active_goal.is_none()
    });
}

#[test]
fn time_budget_stops_active_goal_and_rejects_manual_execution() {
    let engine = connect(Settings {
        max_seconds: 1,
        ..Settings::default()
    });
    engine.send(Command::Start);
    let stopped = wait_for(&engine, "time budget stop", |v| {
        v.last_error.as_deref() == Some("Session time budget reached")
    });
    let actions = count(&stopped, "action");
    assert_eq!(actions, 1);
    engine.send(manual_wait(&engine.snapshot()));
    remains(&engine, |v| {
        count(v, "action") == actions && v.active_goal.is_none()
    });
}

#[test]
fn manual_takeover_cancels_pending_model_and_records_separate_origin() {
    let engine = connect(Settings::default());
    pending(&engine);
    engine.send(manual_wait(&engine.snapshot()));
    let view = wait_for(&engine, "manual action", |v| count(v, "action") == 1);
    let event = view.events.iter().find(|e| e.kind == "action").unwrap();
    assert!(event.message.contains("Manual action; mixed control"));
    assert!(event.decision.is_none());
    remains(&engine, |v| {
        count(v, "action") == 1 && count(v, "decision") == 0
    });
}

#[test]
fn replay_is_immutable_even_when_play_and_manual_commands_are_sent() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("replay.json");
    let recording = Recording {
        schema_version: 1,
        id: "saved-session".into(),
        settings: Settings::default(),
        events: vec![Event {
            sequence: 1,
            elapsed_ms: 60_000,
            kind: "request".into(),
            message: "Recorded request".into(),
            observation: None,
            candidates: vec![],
            decision: None,
        }],
    };
    let original = serde_json::to_vec(&recording).unwrap();
    std::fs::write(&path, &original).unwrap();
    let engine = connect(Settings::default());
    pending(&engine);
    engine.send(Command::Replay(path.to_string_lossy().into_owned()));
    let replay = wait_for(&engine, "loaded replay", |v| v.replay);
    let events = serde_json::to_value(&replay.events).unwrap();
    engine.send(Command::Start);
    engine.send(Command::Step);
    engine.send(manual_wait(&engine.snapshot()));
    remains(&engine, |v| {
        v.replay
            && v.requests == 1
            && v.active_goal.is_none()
            && serde_json::to_value(&v.events).unwrap() == events
    });
    assert_eq!(std::fs::read(path).unwrap(), original);
}
