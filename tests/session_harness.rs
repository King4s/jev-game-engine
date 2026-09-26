//! Offline coverage for the headless session harness.
//!
//! Everything here runs against the synthetic fixture (`Mode::Demo`): no provider call, no
//! Minecraft connection, no API key. What it proves is that the harness drives the real
//! engine through a whole budgeted session with an objective and the safety reflex enabled,
//! that the recording it exports loads through the app's own loader and carries those
//! settings, that its summary agrees with that recording, and that invalid input is refused
//! before anything connects. It proves nothing about a live mob or about the model.

use std::{collections::BTreeMap, path::PathBuf, process::Command as Process, time::Duration};

use jev_game_engine::{
    harness::{self, EndReason, SessionOptions},
    model::{Decision, Event, FixtureWaypoint, Mode, Settings},
    origin::{ActionOrigin, event_origin},
    recording,
};

fn fixture_options(objective: &str, export: Option<PathBuf>) -> SessionOptions {
    SessionOptions {
        settings: Settings {
            mode: Mode::Demo,
            fixture_waypoint: FixtureWaypoint::Reachable,
            objective: objective.into(),
            safety_reflex: true,
            request_interval_ms: 0,
            max_requests: 2,
            max_seconds: 60,
            ..Settings::default()
        },
        connect_timeout: Duration::from_secs(8),
        export,
        keep_connected: false,
        verbose: false,
    }
}

#[test]
fn a_fixture_session_with_an_objective_and_the_reflex_ends_on_its_budget_and_exports() {
    let directory = tempfile::tempdir().unwrap();
    let export = directory.path().join("night.json");
    let objective = "Survive the night and keep health";
    let summary = harness::run_session(&fixture_options(objective, Some(export.clone())))
        .expect("the fixture session runs without a key or a server");

    assert_eq!(
        summary.end_reason,
        EndReason::Budget,
        "{}",
        summary.end_detail
    );
    assert_eq!(summary.end_reason.exit_code(), 0);
    assert!(summary.connected_at_end);
    assert_eq!(summary.mode, "Demo");
    assert_eq!(summary.objective, objective);
    assert!(summary.safety_reflex);
    assert_eq!(summary.counts.requests, 2, "one request per budgeted goal");
    assert_eq!(summary.counts.answers, 2);
    assert_eq!(
        summary.counts.answers_wait + summary.counts.answers_waypoint + summary.counts.answers_flee,
        2,
        "every fixture answer is a wait or a waypoint goal"
    );
    assert_eq!(
        summary.counts.reflex_actions, 0,
        "the fixture observes no entities, so the reflex never fires"
    );
    assert_eq!(
        summary.recording_path.as_deref(),
        Some(export.to_str().unwrap())
    );

    let recorded = recording::load(export.to_str().unwrap())
        .expect("the exported recording validates through the app's loader");
    assert_eq!(recorded.settings.objective, objective);
    assert!(recorded.settings.safety_reflex);
    assert_eq!(recorded.settings.mode, Mode::Demo);
    assert!(
        recorded.events.iter().any(|event| event.kind == "request"),
        "the recording carries the requests the summary counted"
    );
    assert_eq!(
        harness::counts(&recorded.events),
        summary.counts,
        "the summary is derived from the same events the recording holds"
    );
    // Positions round-trip through JSON, so compare the two displacements within a
    // micrometre rather than bit for bit.
    let recorded_displacement = harness::displacement(&recorded.events).expect("two observations");
    let summary_displacement = summary.displacement_m.expect("two observations");
    assert!(
        (recorded_displacement - summary_displacement).abs() < 1e-6,
        "recording {recorded_displacement} vs summary {summary_displacement}"
    );
    assert!(
        !recorded
            .events
            .iter()
            .any(|event| event_origin(event) == Some(ActionOrigin::Manual)),
        "the harness never takes over manually"
    );
    assert!(
        recorded
            .events
            .iter()
            .filter(|event| event.kind == "decision")
            .all(|event| event
                .decision
                .as_ref()
                .is_some_and(|d| d.model == "offline-fixture")),
        "every fixture answer is labelled as the offline fixture, never as live Jev"
    );
}

#[test]
fn without_an_export_path_the_recording_stays_where_the_engine_wrote_it() {
    let summary = harness::run_session(&fixture_options("", None)).unwrap();
    assert_eq!(
        summary.end_reason,
        EndReason::Budget,
        "{}",
        summary.end_detail
    );
    let path = summary
        .recording_path
        .expect("the engine exported a recording");
    assert!(path.starts_with("runs/"), "{path}");
    recording::load(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn invalid_options_are_refused_before_anything_connects() {
    let long = "x".repeat(401);
    let cases: Vec<(&str, SessionOptions)> = vec![
        (
            "objective over 400 characters",
            fixture_options(&long, None),
        ),
        (
            "objective with a control character",
            fixture_options("Survive\u{7}", None),
        ),
        ("request interval over one day", {
            let mut options = fixture_options("", None);
            options.settings.request_interval_ms = (86_400 + 1) * 1_000;
            options
        }),
        ("zero request budget", {
            let mut options = fixture_options("", None);
            options.settings.max_requests = 0;
            options
        }),
        ("request budget over 100000", {
            let mut options = fixture_options("", None);
            options.settings.max_requests = 100_001;
            options
        }),
        ("zero time budget", {
            let mut options = fixture_options("", None);
            options.settings.max_seconds = 0;
            options
        }),
        ("time budget over seven days", {
            let mut options = fixture_options("", None);
            options.settings.max_seconds = 604_801;
            options
        }),
        (
            "export into a directory that does not exist",
            fixture_options("", Some(PathBuf::from("no/such/directory/night.json"))),
        ),
        (
            "export that is not a .json file",
            fixture_options("", Some(std::env::temp_dir().join("night.txt"))),
        ),
    ];
    for (case, options) in cases {
        let error = options
            .validate()
            .expect_err(&format!("{case} must be refused"));
        let message = format!("{error:#}");
        assert!(!message.contains('\n'), "{case}: one line, got {message:?}");
        assert!(
            harness::run_session(&options).is_err(),
            "{case}: run_session must refuse it as well"
        );
    }

    let exactly_400 = "y".repeat(400);
    fixture_options(&exactly_400, None)
        .validate()
        .expect("400 characters is the inclusive limit");
    fixture_options("Line one\nline two", None)
        .validate()
        .expect("a newline is allowed inside an objective");
}

#[test]
fn every_failure_has_its_own_non_zero_exit_code() {
    let reasons = [
        EndReason::Budget,
        EndReason::ConnectionFailed,
        EndReason::ProviderFailed,
        EndReason::Disconnected,
        EndReason::Stalled,
    ];
    let codes: Vec<i32> = reasons.iter().map(|reason| reason.exit_code()).collect();
    assert_eq!(codes[0], 0, "only a budget end is a success");
    assert!(codes[1..].iter().all(|code| *code != 0));
    let unique: std::collections::BTreeSet<i32> = codes.iter().copied().collect();
    assert_eq!(
        unique.len(),
        reasons.len(),
        "codes must be distinct: {codes:?}"
    );
}

fn decision(choice: &str, model: &str) -> Decision {
    Decision {
        choice: choice.into(),
        probabilities: BTreeMap::new(),
        confidence: None,
        model: model.into(),
        input_tokens: None,
        output_tokens: None,
        latency_ms: 0,
    }
}

fn event(sequence: u64, kind: &str, message: &str, decision: Option<Decision>) -> Event {
    Event {
        sequence,
        elapsed_ms: sequence * 100,
        kind: kind.into(),
        message: message.into(),
        observation: None,
        candidates: Vec::new(),
        decision,
        arrival: None,
    }
}

#[test]
fn counts_separate_wait_waypoint_flee_answers_and_reflex_actions() {
    let jev = "jev-1.13.0";
    let events = vec![
        event(1, "request", "TypeSafe request 1 for observation 5", None),
        event(2, "decision", "wait", Some(decision("wait", jev))),
        event(
            3,
            "dispatched",
            "Jev selected goal; queued: wait",
            Some(decision("wait", jev)),
        ),
        event(
            4,
            "action",
            "Jev selected goal; adapter accepted bounded action: wait",
            None,
        ),
        event(5, "request", "TypeSafe request 2 for observation 9", None),
        event(
            6,
            "decision",
            "waypoint_3",
            Some(decision("waypoint_3", jev)),
        ),
        event(
            7,
            "dispatched",
            "Jev selected goal; queued: waypoint_3",
            Some(decision("waypoint_3", jev)),
        ),
        event(
            8,
            "action",
            "Jev selected goal; adapter accepted bounded action: waypoint_3",
            None,
        ),
        event(9, "request", "TypeSafe request 3 for observation 12", None),
        event(10, "decision", "flee_0", Some(decision("flee_0", jev))),
        event(
            11,
            "dispatched",
            "Jev selected goal; queued: flee_0",
            Some(decision("flee_0", jev)),
        ),
        event(
            12,
            "action",
            "Jev selected goal; adapter accepted bounded action: flee_0",
            None,
        ),
        event(
            13,
            "reflex",
            "Engine safety action: Zombie is 3.0 blocks away; no goal in flight and fleeing via flee_0",
            None,
        ),
        event(
            14,
            "dispatched",
            "SAFETY-REFLEX; engine-initiated bounded action; queued for adapter validation: flee_0",
            None,
        ),
        event(
            15,
            "action",
            "SAFETY-REFLEX; engine-initiated bounded action; adapter accepted bounded action: flee_0",
            None,
        ),
    ];
    let counts = harness::counts(&events);
    assert_eq!(counts.events, 15);
    assert_eq!(counts.requests, 3);
    assert_eq!(
        counts.answers, 3,
        "one per decision event, not per dispatch"
    );
    assert_eq!(counts.answers_wait, 1);
    assert_eq!(counts.answers_waypoint, 1);
    assert_eq!(counts.answers_flee, 1);
    assert_eq!(counts.answers_other, 0);
    assert_eq!(
        counts.reflex_actions, 1,
        "the reflex dispatch is counted once and never as an answer"
    );
    assert_eq!(counts.accepted_actions, 4);
    assert_eq!(counts.arrival_verdicts, 0);
}

fn session_run(args: &[&str], with_key: bool) -> (i32, String) {
    let mut command = Process::new(env!("CARGO_BIN_EXE_session-run"));
    command.args(args);
    if !with_key {
        command.env_remove("TYPESAFE_API_KEY");
        command.env_remove("TYPESAFE_API_KEY_FILE");
    }
    let output = command.output().expect("session-run runs");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn the_cli_refuses_invalid_input_with_one_line_and_exit_code_2() {
    let long = "x".repeat(401);
    let cases: Vec<(&str, Vec<&str>)> = vec![
        (
            "long objective",
            vec!["--fixture", "--objective", long.as_str()],
        ),
        (
            "control character",
            vec!["--fixture", "--objective", "Survive\u{7}"],
        ),
        (
            "interval over one day",
            vec!["--fixture", "--request-interval-seconds", "86401"],
        ),
        ("zero requests", vec!["--fixture", "--max-requests", "0"]),
        (
            "seconds over a week",
            vec!["--fixture", "--max-seconds", "604801"],
        ),
        (
            "export directory missing",
            vec!["--fixture", "--export", "no/such/directory/night.json"],
        ),
        (
            "objective without text",
            vec!["--fixture", "--objective", "   "],
        ),
    ];
    for (case, args) in cases {
        let (code, stderr) = session_run(&args, true);
        assert_eq!(code, 2, "{case}: {stderr}");
        assert_eq!(
            stderr.trim().lines().count(),
            1,
            "{case}: one line, got {stderr:?}"
        );
        assert!(stderr.starts_with("session-run: "), "{case}: {stderr}");
    }
}

#[test]
fn live_mode_without_a_key_is_refused_before_connecting() {
    let (code, stderr) = session_run(&["--max-requests", "1", "--max-seconds", "5"], false);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("TYPESAFE_API_KEY"), "{stderr}");
    assert!(
        !stderr.contains("Live session:"),
        "nothing was started: {stderr}"
    );
}

#[test]
fn pacing_holds_the_second_request_back_by_at_least_the_interval() {
    let mut options = fixture_options("Survive the night", None);
    options.settings.request_interval_ms = 1_000;
    options.settings.max_requests = 2;
    let summary = harness::run_session(&options).unwrap();
    assert_eq!(
        summary.end_reason,
        EndReason::Budget,
        "{}",
        summary.end_detail
    );
    let path = summary.recording_path.expect("exported");
    let recorded = recording::load(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    let requests: Vec<u64> = recorded
        .events
        .iter()
        .filter(|event| event.kind == "request")
        .map(|event| event.elapsed_ms)
        .collect();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert!(
        requests[1] - requests[0] >= 1_000,
        "the paced second request came {} ms after the first",
        requests[1] - requests[0]
    );
    assert_eq!(recorded.settings.request_interval_ms, 1_000);
}

#[test]
fn help_prints_every_flag_and_exits_zero() {
    let output = Process::new(env!("CARGO_BIN_EXE_session-run"))
        .arg("--help")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let text = String::from_utf8_lossy(&output.stdout);
    for flag in [
        "--fixture",
        "--objective",
        "--safety-reflex",
        "--request-interval-seconds",
        "--max-requests",
        "--max-seconds",
        "--export",
        "--keep-connected",
    ] {
        assert!(text.contains(flag), "--help must list {flag}");
    }
}
