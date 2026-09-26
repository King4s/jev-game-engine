//! Offline coverage for the attribution rule the desktop timeline and the offline recording
//! report share.
//!
//! These three origins are what keeps model-selected navigation, the synthetic fixture and an
//! operator takeover distinguishable inside one recording, so the tests pin both the
//! classification derived from an event's own fields and the exact labels that reach the screen.
//! Everything here runs against the synthetic fixture: no Minecraft connection, no provider call.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use jev_game_engine::{
    engine::EngineHandle,
    model::{Candidate, Command, Decision, Event, Settings, View},
    origin::{ActionOrigin, event_origin},
};

fn decision(model: &str) -> Decision {
    Decision {
        choice: "wait".into(),
        probabilities: BTreeMap::new(),
        confidence: None,
        model: model.into(),
        input_tokens: None,
        output_tokens: None,
        latency_ms: 0,
    }
}

/// A recorded event with only the fields the attribution rule reads.
fn event(kind: &str, message: &str, model: Option<&str>) -> Event {
    Event {
        sequence: 1,
        elapsed_ms: 0,
        kind: kind.into(),
        message: message.into(),
        observation: None,
        candidates: Vec::new(),
        decision: model.map(decision),
        arrival: None,
    }
}

fn wait_for(engine: &EngineHandle, description: &str, predicate: impl Fn(&View) -> bool) -> View {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let view = engine.snapshot();
        if predicate(&view) {
            return view;
        }
        assert!(
            Instant::now() < deadline,
            "Timed out waiting for {description}; status={}, error={:?}",
            view.status,
            view.last_error
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn count(view: &View, kind: &str) -> usize {
    view.events
        .iter()
        .filter(|event| event.kind == kind)
        .count()
}

fn connect_fixture() -> EngineHandle {
    let engine = EngineHandle::new();
    engine.send(Command::Connect(Settings::default()));
    wait_for(&engine, "connected fixture", |view| {
        view.observation
            .as_ref()
            .is_some_and(|observation| observation.connected)
    });
    engine
}

/// The manual choices the engine offers are bound to the current observation.
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

#[test]
fn a_manual_takeover_is_labelled_manual_and_outranks_a_model_decision() {
    let takeover = "Manual action; mixed control";
    assert_eq!(
        event_origin(&event("action", takeover, None)),
        Some(ActionOrigin::Manual)
    );
    // A takeover is what the operator did, so its recorded message outranks any decision the
    // engine attached to the same row rather than letting it read as a model choice.
    assert_eq!(
        event_origin(&event("dispatched", takeover, Some("jev-1.13.0"))),
        Some(ActionOrigin::Manual)
    );
    assert_eq!(
        event_origin(&event("dispatched", takeover, Some("offline-fixture"))),
        Some(ActionOrigin::Manual)
    );
}

#[test]
fn the_offline_fixture_identity_is_labelled_fixture_on_its_goal_rows() {
    for kind in ["decision", "dispatched"] {
        assert_eq!(
            event_origin(&event(
                kind,
                "synthetic fixture decision",
                Some("offline-fixture")
            )),
            Some(ActionOrigin::Fixture),
            "kind {kind} from the fixture must read as a fixture choice"
        );
    }
}

#[test]
fn a_configured_model_goal_is_labelled_jev_selected_on_all_four_goal_rows() {
    for kind in ["decision", "dispatched", "action", "executor"] {
        assert_eq!(
            event_origin(&event(kind, "Jev selected goal", Some("jev-1.13.0"))),
            Some(ActionOrigin::JevSelected),
            "kind {kind} from a configured model must read as a model choice"
        );
    }
}

#[test]
fn engine_bookkeeping_and_decision_less_goal_rows_record_no_origin() {
    for kind in [
        "connect",
        "control",
        "observation",
        "request",
        "error",
        "budget",
    ] {
        assert_eq!(
            event_origin(&event(kind, "engine bookkeeping", None)),
            None,
            "kind {kind} is engine bookkeeping and must stay unlabelled"
        );
    }
    // A decision attached to a row that is not part of a goal chain says nothing about who acted,
    // and an action or executor row whose decision was recorded on the dispatch carries no
    // decision of its own, so both stay unlabelled rather than guessing from the message.
    assert_eq!(
        event_origin(&event("request", "asked", Some("jev-1.13.0"))),
        None
    );
    assert_eq!(
        event_origin(&event("action", "adapter accepted bounded action", None)),
        None
    );
    assert_eq!(
        event_origin(&event("executor", "Local executor: target reached", None)),
        None
    );
}

#[test]
fn the_four_labels_are_exact_and_distinct() {
    assert_eq!(ActionOrigin::JevSelected.label(), "JEV-SELECTED");
    assert_eq!(ActionOrigin::Fixture.label(), "FIXTURE");
    assert_eq!(ActionOrigin::Manual.label(), "MANUAL");
    assert_eq!(ActionOrigin::SafetyReflex.label(), "SAFETY-REFLEX");
    let labels = [
        ActionOrigin::JevSelected.label(),
        ActionOrigin::Fixture.label(),
        ActionOrigin::Manual.label(),
        ActionOrigin::SafetyReflex.label(),
    ];
    let unique: std::collections::BTreeSet<&str> = labels.iter().copied().collect();
    assert_eq!(
        unique.len(),
        4,
        "the origins must be distinguishable, not four spellings of one label"
    );
}

#[test]
fn a_safety_reflex_action_is_labelled_as_engine_initiated_not_model_selected() {
    // The engine writes the reflex origin into the messages it records for that action
    // (`SAFETY-REFLEX; engine-initiated bounded action`) and precedes them with a `reflex`
    // event, and it attaches no model decision, so nothing here can be read as JEV-SELECTED.
    for (kind, message) in [
        (
            "reflex",
            "Engine safety action: Zombie is 3.0 blocks away; no goal in flight and fleeing via flee_0",
        ),
        (
            "dispatched",
            "SAFETY-REFLEX; engine-initiated bounded action; queued for adapter validation: flee_0",
        ),
        (
            "action",
            "SAFETY-REFLEX; engine-initiated bounded action; adapter accepted bounded action: flee_0",
        ),
    ] {
        assert_eq!(
            event_origin(&event(kind, message, None)),
            Some(ActionOrigin::SafetyReflex),
            "{kind} row"
        );
    }
    // A model-selected dispatch with the same candidate id keeps its own label.
    assert_eq!(
        event_origin(&event(
            "dispatched",
            "Jev selected goal; queued for adapter validation: flee_0",
            Some("jev-1.13.0")
        )),
        Some(ActionOrigin::JevSelected)
    );
}

#[test]
fn a_fixture_session_labels_its_recorded_rows_the_way_the_timeline_shows_them() {
    let engine = connect_fixture();
    engine.send(Command::Step);
    let view = wait_for(&engine, "fixture goal chain", |view| {
        count(view, "executor") == 1
    });

    let decision = view
        .events
        .iter()
        .find(|event| event.kind == "decision")
        .expect("the fixture records its decision");
    assert_eq!(
        decision
            .decision
            .as_ref()
            .map(|decision| decision.model.as_str()),
        Some("offline-fixture")
    );
    assert_eq!(event_origin(decision), Some(ActionOrigin::Fixture));

    let dispatched = view
        .events
        .iter()
        .find(|event| event.kind == "dispatched")
        .expect("the fixture dispatch is recorded");
    assert_eq!(event_origin(dispatched), Some(ActionOrigin::Fixture));

    // Everything the engine recorded around that goal is bookkeeping, so it carries no label and
    // cannot be mistaken for a choice.
    for kind in [
        "connect",
        "control",
        "observation",
        "request",
        "action",
        "executor",
    ] {
        for event in view.events.iter().filter(|event| event.kind == kind) {
            assert_eq!(
                event_origin(event),
                None,
                "kind {kind} recorded by the engine must stay unlabelled: {event:?}"
            );
        }
    }
}

#[test]
fn an_operator_takeover_is_labelled_manual_in_the_engine_recorded_events() {
    let engine = connect_fixture();
    engine.send(manual_wait(&engine.snapshot()));
    let view = wait_for(&engine, "recorded manual action", |view| {
        count(view, "action") == 1
    });

    let action = view
        .events
        .iter()
        .find(|event| event.kind == "action")
        .expect("the manual action is recorded");
    assert!(
        action.message.contains("Manual action"),
        "the takeover announces itself in the recorded message: {}",
        action.message
    );
    assert_eq!(event_origin(action), Some(ActionOrigin::Manual));

    // No part of this session may read as a model or fixture choice: the operator chose it.
    let labelled: Vec<ActionOrigin> = view.events.iter().filter_map(event_origin).collect();
    assert_eq!(
        labelled,
        vec![ActionOrigin::Manual; labelled.len()],
        "a takeover session records manual actions and nothing else"
    );
}
