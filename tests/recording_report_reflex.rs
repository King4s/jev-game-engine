//! `recording-report` must keep engine safety-reflex actions apart from model answers.
//!
//! A reflex action carries no model decision, so a report that only counted decisions would
//! silently drop it, and a report that counted every dispatch as an answer would fold it into
//! the model's choices. This runs the real binary on a hand-written recording that holds one
//! model answer and one reflex action, in text and `--json` form, offline.

use std::{collections::BTreeMap, process::Command};

use jev_game_engine::model::{
    Candidate, Decision, Event, FixtureWaypoint, Mode, Recording, Settings,
};

fn candidate(id: &str) -> Candidate {
    Candidate {
        id: id.into(),
        description: format!("candidate {id}"),
        target: None,
        duration_ms: 2_000,
    }
}

fn fixture_decision(choice: &str) -> Decision {
    Decision {
        choice: choice.into(),
        probabilities: BTreeMap::new(),
        confidence: None,
        model: "offline-fixture".into(),
        input_tokens: None,
        output_tokens: None,
        latency_ms: 1,
    }
}

fn event(
    sequence: u64,
    kind: &str,
    message: &str,
    candidates: Vec<Candidate>,
    decision: Option<Decision>,
) -> Event {
    Event {
        sequence,
        elapsed_ms: sequence * 100,
        kind: kind.into(),
        message: message.into(),
        observation: None,
        candidates,
        decision,
        arrival: None,
    }
}

/// One model answer (`wait`) followed by one engine reflex action (`flee_0`).
fn recording_with_one_answer_and_one_reflex() -> Recording {
    Recording {
        schema_version: 1,
        id: "reflex-report-test".into(),
        settings: Settings {
            mode: Mode::Demo,
            fixture_waypoint: FixtureWaypoint::Distant,
            safety_reflex: true,
            ..Settings::default()
        },
        events: vec![
            event(
                1,
                "request",
                "Synthetic fixture decision (no API call) 1 for observation 3",
                vec![candidate("wait")],
                None,
            ),
            event(
                2,
                "decision",
                "wait",
                vec![candidate("wait")],
                Some(fixture_decision("wait")),
            ),
            event(
                3,
                "dispatched",
                "Fixture selected goal; queued for adapter validation: wait",
                vec![candidate("wait")],
                Some(fixture_decision("wait")),
            ),
            event(
                4,
                "action",
                "Fixture selected goal; adapter accepted bounded action: wait",
                vec![candidate("wait")],
                None,
            ),
            event(
                5,
                "reflex",
                "Engine safety action: Zombie is 3.0 blocks away; no goal in flight and fleeing via flee_0",
                vec![candidate("flee_0"), candidate("wait")],
                None,
            ),
            event(
                6,
                "dispatched",
                "SAFETY-REFLEX; engine-initiated bounded action; queued for adapter validation: flee_0",
                vec![candidate("flee_0"), candidate("wait")],
                None,
            ),
            event(
                7,
                "action",
                "SAFETY-REFLEX; engine-initiated bounded action; adapter accepted bounded action: flee_0",
                vec![candidate("flee_0")],
                None,
            ),
        ],
    }
}

fn report(args: &[&str]) -> (i32, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_recording-report"))
        .args(args)
        .output()
        .expect("recording-report runs");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

#[test]
fn the_report_counts_the_reflex_action_apart_from_the_model_answer() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("reflex.json");
    let recording = recording_with_one_answer_and_one_reflex();
    jev_game_engine::recording::validate(&recording).expect("the hand-written recording is valid");
    std::fs::write(&path, serde_json::to_vec_pretty(&recording).unwrap()).unwrap();
    let path = path.to_str().unwrap();

    let (code, json) = report(&[path, "--json"]);
    assert_eq!(code, 0, "{json}");
    let summary: serde_json::Value = serde_json::from_str(&json).expect("one JSON document");
    assert_eq!(summary["answers"], 1, "the model answered once");
    assert_eq!(summary["answer_choices"]["wait"], 1);
    assert!(
        summary["answer_choices"].get("flee_0").is_none(),
        "the reflex's flight goal is not a model answer: {summary}"
    );
    assert_eq!(summary["reflex_actions"], 1);
    assert_eq!(summary["reflex_events"], 1);
    assert_eq!(summary["accepted_actions"], 2, "both actions were accepted");
    assert_eq!(summary["requests"], 1);

    let (code, text) = report(&[path]);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains("Engine safety-reflex actions 1"),
        "text output names the reflex count: {text}"
    );
    assert!(text.contains("SAFETY-REFLEX"), "{text}");
    assert!(text.contains("answers 1"), "{text}");
}

#[test]
fn a_recording_without_a_reflex_reports_zero_and_keeps_its_answer_count() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("no-reflex.json");
    let mut recording = recording_with_one_answer_and_one_reflex();
    recording.events.truncate(4);
    std::fs::write(&path, serde_json::to_vec_pretty(&recording).unwrap()).unwrap();

    let (code, json) = report(&[path.to_str().unwrap(), "--json"]);
    assert_eq!(code, 0, "{json}");
    let summary: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(summary["answers"], 1);
    assert_eq!(summary["reflex_actions"], 0);
    assert_eq!(summary["reflex_events"], 0);
}
