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
    engine::EngineHandle,
    harness::{self, EndReason, SessionOptions},
    model::{
        Candidate, Command, CraftingSlot, Decision, Event, FixtureWaypoint, Mode, Observation,
        Settings,
    },
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
            // The fixture offers exactly one reachable waypoint. Once it is reached there is
            // nothing actionable, so the engine reports idling instead of spending a second
            // request and the run ends on its time budget: keep that budget short.
            max_seconds: 8,
            ..Settings::default()
        },
        connect_timeout: Duration::from_secs(8),
        prestart_timeout: Duration::ZERO,
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
    // The fixture offers one reachable waypoint, and staying put is a real option in every world.
    // So the engine asks once for the waypoint and, once that goal has arrived, asks again with the
    // honest remaining option rather than deciding for the model that nothing should happen.
    assert!(
        summary.counts.requests >= 1,
        "the fixture always has something to ask about: {:?}",
        summary.end_detail
    );
    assert_eq!(
        summary.counts.answers, summary.counts.requests,
        "every request in the fixture is answered"
    );
    assert_eq!(
        summary.counts.answers_wait + summary.counts.answers_waypoint + summary.counts.answers_flee,
        summary.counts.requests,
        "every fixture answer is one of the real decision kinds, never an empty choice"
    );
    // The completed waypoint is offered in exactly one request: once it has arrived, the geometry
    // proves no verified step to it exists, so it must not come back on the menu.
    let loaded = recording::load(export.to_str().unwrap())
        .expect("the exported recording loads through the app's loader");
    let requests: Vec<&_> = loaded
        .events
        .iter()
        .filter(|event| event.kind == "request")
        .collect();
    assert!(
        requests.iter().all(|event| !event.candidates.is_empty()),
        "the engine must never send an empty menu"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|event| event
                .candidates
                .iter()
                .any(|candidate| candidate.target.is_some()))
            .count(),
        1,
        "exactly one request may offer the reachable waypoint, and it must not be offered after it arrived"
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
        EndReason::Died,
        EndReason::PrerequisiteFailed,
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

#[test]
fn prestart_requires_current_dimension_and_empty_known_inventory() {
    let mut observation = jev_game_engine::model::Observation {
        connected: true,
        sequence: 2,
        dimension: Some("minecraft:hub".into()),
        crafting_inventory: Some(empty_player_slots()),
        ..Default::default()
    };
    assert_eq!(
        harness::prestart_issue(&observation, "minecraft:overworld", 2, Some(Duration::ZERO))
            .as_deref(),
        Some("waiting for a fresh observation")
    );
    assert!(
        harness::prestart_issue(&observation, "minecraft:overworld", 1, Some(Duration::ZERO))
            .unwrap()
            .contains("minecraft:hub")
    );
    observation.dimension = Some("minecraft:overworld".into());
    observation.inventory.push("slot 0: Dirt x1".into());
    assert!(
        harness::prestart_issue(&observation, "minecraft:overworld", 1, Some(Duration::ZERO))
            .unwrap()
            .contains("inventory")
    );
    observation.inventory.clear();
    observation.items.insert("minecraft:dirt".into(), 1);
    assert!(
        harness::prestart_issue(&observation, "minecraft:overworld", 1, Some(Duration::ZERO))
            .unwrap()
            .contains("inventory")
    );
    observation.items.clear();
    observation.held_item = Some("minecraft:stick".into());
    assert_eq!(
        harness::prestart_issue(&observation, "minecraft:overworld", 1, Some(Duration::ZERO))
            .as_deref(),
        Some("inventory is not empty")
    );
    observation.held_item = None;
    assert_eq!(
        harness::prestart_issue(&observation, "minecraft:overworld", 1, Some(Duration::ZERO)),
        None
    );
}

fn empty_player_slots() -> Vec<CraftingSlot> {
    (9..45)
        .map(|slot| CraftingSlot {
            slot,
            item: "minecraft:air".into(),
            count: 0,
        })
        .collect()
}

#[test]
fn prestart_requires_complete_empty_packet_confirmed_inventory() {
    let mut observation = jev_game_engine::model::Observation {
        connected: true,
        sequence: 3,
        dimension: Some("minecraft:overworld".into()),
        ..Default::default()
    };
    let issue = |observation: &Observation| {
        harness::prestart_issue(observation, "minecraft:overworld", 2, Some(Duration::ZERO))
    };
    assert_eq!(
        issue(&observation).as_deref(),
        Some("inventory unavailable")
    );
    observation.crafting_inventory = Some(empty_player_slots());
    observation.crafting_inventory.as_mut().unwrap()[0].count = 1;
    assert_eq!(
        issue(&observation).as_deref(),
        Some("inventory is not empty")
    );
    observation.crafting_inventory.as_mut().unwrap()[0].count = 0;
    observation.crafting_inventory.as_mut().unwrap().pop();
    assert_eq!(
        issue(&observation).as_deref(),
        Some("inventory unavailable")
    );
    observation.crafting_inventory = Some(empty_player_slots());
    assert_eq!(issue(&observation), None);
}

#[test]
fn prestart_requires_a_recent_sequence_advance() {
    let observation = jev_game_engine::model::Observation {
        connected: true,
        sequence: 3,
        dimension: Some("minecraft:overworld".into()),
        crafting_inventory: Some(empty_player_slots()),
        ..Default::default()
    };
    let issue =
        |initial, age| harness::prestart_issue(&observation, "minecraft:overworld", initial, age);
    assert_eq!(
        issue(3, Some(Duration::ZERO)).as_deref(),
        Some("waiting for a fresh observation")
    );
    assert_eq!(
        issue(2, None).as_deref(),
        Some("waiting for a fresh observation")
    );
    assert_eq!(
        issue(2, Some(Duration::from_millis(501))).as_deref(),
        Some("waiting for a fresh observation")
    );
    assert_eq!(issue(2, Some(Duration::from_millis(500))), None);
}

#[test]
fn fixture_cannot_request_a_live_wood_prestart_gate() {
    let mut options = fixture_options("", None);
    options.prestart_timeout = Duration::from_secs(3);
    options.settings.wood_skills = true;
    assert!(
        options
            .validate()
            .unwrap_err()
            .to_string()
            .contains("live wood")
    );
}

/// Regression for the first live night run, which ended as `provider_failed` 21 s in when
/// the bot was moved from the hub into the test world: a world change is a local stop the
/// harness resumes from, not the end of the session. Damage the bot survives is resumed too,
/// so a survival objective goes on; no health left is its own end reason.
#[test]
fn a_world_change_while_connected_resumes_instead_of_ending_the_session() {
    use harness::{ErrorOutcome, classify_error};
    use jev_game_engine::engine::{
        BOT_DIED, DIMENSION_CHANGED, HEALTH_DECREASED, WORLD_LIFECYCLE_CHANGED,
    };

    let alive = Some(20.0);
    for error in [DIMENSION_CHANGED, WORLD_LIFECYCLE_CHANGED, HEALTH_DECREASED] {
        assert_eq!(
            classify_error(error, true, alive),
            ErrorOutcome::Resume,
            "{error}"
        );
        assert_eq!(
            classify_error(error, false, alive),
            ErrorOutcome::End(EndReason::Disconnected),
            "a world change without a connection is a disconnect: {error}"
        );
    }
    let cases = [
        ("Request budget reached", true, EndReason::Budget),
        ("Session time budget reached", true, EndReason::Budget),
        (
            "Session time budget reached",
            false,
            EndReason::Disconnected,
        ),
        (
            "Adapter returned invalid observation values",
            true,
            EndReason::Disconnected,
        ),
        ("TypeSafe request failed", true, EndReason::ProviderFailed),
    ];
    for (error, connected, reason) in cases {
        assert_eq!(
            classify_error(error, connected, alive),
            ErrorOutcome::End(reason),
            "{error} (connected {connected})"
        );
    }
    // The adapter's death count ends the session even when the respawned bot is healthy
    // and connected in another world, which is what the live run hid as a world change.
    assert_eq!(
        classify_error(BOT_DIED, true, alive),
        ErrorOutcome::End(EndReason::Died)
    );
    for (connected, health) in [(true, Some(0.0)), (false, Some(0.0)), (true, None)] {
        assert_eq!(
            classify_error(HEALTH_DECREASED, connected, health),
            ErrorOutcome::End(EndReason::Died),
            "connected {connected}, health {health:?}"
        );
    }
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

/// A refusal never ends the run, so the recording's own classification is how a session
/// says how much of what the model chose the engine could not carry out. A refused action
/// carries the candidate it refused; a dropped model answer carries none and stays its own
/// class.
#[test]
fn counts_separate_a_refused_action_from_a_rejected_answer() {
    let mut refused = event(
        1,
        "rejected",
        "Local guard rejected action 'goal_soil': no local executor in this build runs it (no skill, no target, and it is not a bounded wait/stop action)",
        None,
    );
    refused.candidates = vec![Candidate {
        skill: None,
        id: "goal_soil".into(),
        description: "Goal 5/9 (bounded catalogue, ladder order)".into(),
        target: None,
        duration_ms: 60_000,
    }];
    let events = vec![
        refused,
        event(
            2,
            "rejected",
            "TypeSafe answer rejected: probabilities do not sum to one",
            None,
        ),
    ];
    let counts = harness::counts(&events);
    assert_eq!(counts.refused_actions, 1, "the refused goal is counted");
    assert_eq!(
        counts.rejected_answers, 1,
        "a dropped answer is classified apart from a refused action"
    );
    assert_eq!(counts.accepted_actions, 0, "a refusal executes nothing");
}

#[test]
fn routed_attempts_count_each_provider_start_in_summary_and_replay() {
    let session_id = "routed-session";
    let events = vec![
        event(1, "request", "one routed goal", None),
        event(
            2,
            "model_stage",
            &format!(r#"{{"session_id":"{session_id}","stage":{{"status":"started"}}}}"#),
            None,
        ),
        event(
            3,
            "model_stage",
            &format!(r#"{{"session_id":"{session_id}","stage":{{"status":"completed"}}}}"#),
            None,
        ),
        event(
            4,
            "model_stage",
            &format!(r#"{{"session_id":"{session_id}","stage":{{"status":"started"}}}}"#),
            None,
        ),
        event(
            5,
            "model_stage",
            r#"{"session_id":"previous-session","stage":{"status":"started"}}"#,
            None,
        ),
    ];
    let recording = jev_game_engine::model::Recording {
        schema_version: 1,
        id: session_id.into(),
        settings: Settings::default(),
        events,
    };
    assert_eq!(
        harness::counts_for_session(&recording.events, &recording.id).requests,
        2,
        "summary counts Jev and Astra starts, not the one routed request event"
    );
    assert_eq!(recording::request_count(&recording), 2, "replay agrees");

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("routed.json");
    std::fs::write(&path, serde_json::to_vec(&recording).unwrap()).unwrap();
    let engine = EngineHandle::new();
    engine.send(Command::Replay(path.to_string_lossy().into_owned()));
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let replay = engine.snapshot();
        if replay.replay {
            assert_eq!(replay.requests, 2, "replay view uses the same count");
            break;
        }
        assert!(std::time::Instant::now() < deadline, "replay did not load");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn unrelated_stage_telemetry_keeps_legacy_request_counting() {
    let events = vec![
        event(1, "request", "legacy request", None),
        event(
            2,
            "model_stage",
            r#"{"session_id":"previous-session","stage":{"status":"started"}}"#,
            None,
        ),
    ];
    let recording = jev_game_engine::model::Recording {
        schema_version: 1,
        id: "legacy-session".into(),
        settings: Settings::default(),
        events,
    };
    assert_eq!(
        harness::counts_for_session(&recording.events, &recording.id).requests,
        1
    );
    assert_eq!(recording::request_count(&recording), 1);
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
            "prestart over limit",
            vec!["--fixture", "--prestart-seconds", "301"],
        ),
        (
            "prestart requires live wood",
            vec!["--fixture", "--prestart-seconds", "1"],
        ),
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
fn pacing_does_not_turn_internal_observation_into_a_second_request() {
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
    let requests: Vec<_> = recorded
        .events
        .iter()
        .filter(|event| event.kind == "request")
        .collect();
    assert_eq!(requests.len(), 1, "no model request without an action");
    assert!(recorded.events.iter().any(|event| {
        event.kind == "idle" && event.message.contains("No actionable candidate")
    }));
    assert!(
        requests[0]
            .candidates
            .iter()
            .all(|candidate| candidate.id != "wait")
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
        "--prestart-seconds",
    ] {
        assert!(text.contains(flag), "--help must list {flag}");
    }
}
