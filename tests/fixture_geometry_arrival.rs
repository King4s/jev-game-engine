//! Offline fixture geometry: one bounded Demo goal must arrive, and say so.
//!
//! The acceptance criterion these tests close asks for a deterministic offline run in which a
//! fixture-provider navigate decision reaches its target inside a single bounded goal, the
//! recorded `ArrivalVerdict` reports `arrived` with a measured distance inside the tolerance, and
//! the session then stops locally. An engine-level Demo run can only construct that run if it can
//! select the fixture geometry: `Settings::fixture_waypoint` is what selects it, and without it
//! the offline fixture is always built with the distant waypoint, which one bounded goal can only
//! expire against.
//!
//! Scope: `tests/navigation_arrival.rs` already covers the expiry path of the default distant
//! geometry, verdict validation, serde/load/replay and the adapter-level approach. This file adds
//! only the reachable-geometry outcomes visible through the engine: the arrived verdict and its
//! measured distance, the honest absence of a navigate candidate once the bot sits inside the
//! tolerance, and the ordering continuous mode must keep between a verdict and its next request.
//!
//! Everything here is deterministic and offline: the synthetic fixture in `Mode::Demo` and the
//! offline decision path. No Minecraft connection, no TypeSafe request, no API key. The only
//! time-shaped claims are the bounded polling the sibling tests also use and the recorder's own
//! measured milliseconds, which the criterion names.

use std::time::{Duration, Instant};

use jev_game_engine::{
    engine::EngineHandle,
    fixture,
    model::{
        ARRIVAL_TOLERANCE_M, ArrivalVerdict, Candidate, Command, Event, FixtureWaypoint, Mode,
        Position, Settings, View,
    },
};

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
            view.events
                .iter()
                .map(|event| &event.kind)
                .collect::<Vec<_>>()
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

/// Connects the engine to the offline fixture whose waypoint lies inside one bounded goal's reach.
/// `fixture::REACHABLE_WAYPOINT` sits 1.5 blocks from the origin and the fixture closes 0.1 blocks
/// per 50 ms, so the same 2000 ms demo bound that expires against the distant waypoint arrives
/// here. `Mode::Demo` keeps every decision offline.
fn connect_reachable() -> EngineHandle {
    let engine = EngineHandle::new();
    engine.send(Command::Connect(Settings {
        fixture_waypoint: FixtureWaypoint::Reachable,
        mode: Mode::Demo,
        ..Settings::default()
    }));
    let view = wait_for(&engine, "connected offline fixture", |view| {
        view.observation
            .as_ref()
            .is_some_and(|observation| observation.connected)
    });
    assert_eq!(view.mode, Mode::Demo);
    engine
}

fn count(view: &View, kind: &str) -> usize {
    view.events
        .iter()
        .filter(|event| event.kind == kind)
        .count()
}

fn executor_events(view: &View) -> Vec<&Event> {
    view.events
        .iter()
        .filter(|event| event.kind == "executor")
        .collect()
}

/// Candidates the adapter accepted, in recorded order.
fn accepted_candidates(view: &View) -> Vec<&Candidate> {
    view.events
        .iter()
        .filter(|event| event.kind == "action")
        .flat_map(|event| event.candidates.iter())
        .collect()
}

fn arrival(event: &Event) -> &ArrivalVerdict {
    event
        .arrival
        .as_ref()
        .expect("an executor event records an arrival verdict")
}

fn distance(a: &Position, b: &Position) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2) + (a.z - b.z).powi(2)).sqrt()
}

/// The verdict must agree with the observation recorded on the very same event, so a distance that
/// was not measured from that telemetry cannot pass.
fn assert_verdict_matches_its_observation(event: &Event, verdict: &ArrivalVerdict) {
    let measured = verdict
        .measured_distance_m
        .expect("a connected fixture always measures the distance");
    let target = verdict
        .target
        .as_ref()
        .expect("the goal was bound to a target");
    let observation = event
        .observation
        .as_ref()
        .expect("the event records the observation it measured");
    let recomputed = distance(&observation.position, target);
    assert!(
        (measured - recomputed).abs() < 1e-9,
        "recorded measured distance {measured} disagrees with its own observation ({recomputed})"
    );
}

/// Waits until the fixture stops moving, without trusting a fixed delay: two successive
/// observations must report the same position.
///
/// The fixture republishes every 50 ms and only moves toward its bound target, so a repeated
/// position means either the engine's local stop reached the adapter or the bot arrived within
/// 0.05 blocks of the target. Nothing in a one-step run starts it moving again, so this is a stop
/// and not a pause.
fn wait_until_the_bot_stops(engine: &EngineHandle) -> View {
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut previous: Option<(u64, Position)> = None;
    loop {
        let view = engine.snapshot();
        let current = view
            .observation
            .as_ref()
            .filter(|observation| observation.connected)
            .map(|observation| (observation.sequence, observation.position.clone()));
        let settled = match (&previous, &current) {
            (Some((previous_sequence, previous_position)), Some((sequence, position))) => {
                sequence > previous_sequence && position == previous_position
            }
            _ => false,
        };
        if settled {
            return view;
        }
        assert!(
            Instant::now() < deadline,
            "Timed out waiting for the fixture to stop moving; last observation={previous:?}"
        );
        previous = current;
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn one_reachable_bounded_goal_records_an_arrived_verdict_and_a_local_stop() {
    let engine = connect_reachable();
    engine.send(Command::Step);
    let view = wait_for(&engine, "arrived bounded goal", |view| {
        count(view, "executor") == 1
    });

    // One bounded goal: one request, one adapter-accepted action, one verdict.
    assert_eq!(view.requests, 1);
    assert_eq!(count(&view, "request"), 1);
    assert_eq!(count(&view, "action"), 1);
    assert_eq!(count(&view, "executor"), 1);

    // The reachable waypoint is offered as a navigate candidate: it sits outside the tolerance and
    // inside the 12-block radius.
    let request = view
        .events
        .iter()
        .find(|event| event.kind == "request")
        .expect("the goal starts with a recorded request");
    let mut navigable = request
        .candidates
        .iter()
        .filter(|candidate| candidate.target.is_some());
    let offered = navigable
        .next()
        .expect("the reachable waypoint must be offered as a navigate candidate");
    assert!(
        navigable.next().is_none(),
        "exactly one navigate candidate is reachable here: {:?}",
        request.candidates
    );
    let target = offered
        .target
        .clone()
        .expect("a navigate candidate carries its bound target");
    assert_eq!(target, fixture::REACHABLE_WAYPOINT);
    let request_observation = request
        .observation
        .as_ref()
        .expect("a request records the observation it was shown");
    let start = distance(&request_observation.position, &target);
    assert!(
        start > ARRIVAL_TOLERANCE_M,
        "a waypoint inside the tolerance must not be offered as navigation: {start} blocks"
    );

    // The Demo decision answered offline (no provider call) and chose that candidate.
    let decision = view
        .events
        .iter()
        .find_map(|event| event.decision.as_ref())
        .expect("the request is answered with a decision");
    assert_eq!(
        decision.model, "offline-fixture",
        "demo mode must not call a provider"
    );
    assert_eq!(decision.choice, offered.id);

    // The adapter accepted the chosen candidate, and the verdict describes that same goal.
    let accepted = accepted_candidates(&view);
    assert_eq!(accepted.len(), 1);
    assert_eq!(accepted[0].id, decision.choice);
    assert_eq!(accepted[0].target.as_ref(), Some(&target));
    let action = view
        .events
        .iter()
        .find(|event| event.kind == "action")
        .expect("the accepted action is recorded");
    assert!(
        action.message.contains("adapter accepted bounded action"),
        "unexpected action record: {}",
        action.message
    );

    // The goal ended by arriving inside its bound, not by expiring. No latency sample exists
    // before the first request, so the bound is the default goal_ms of 2000 ms, while the fixture
    // needs about ten of its 50 ms ticks to close the 1.5 blocks to this waypoint.
    let executors = executor_events(&view);
    let executor = executors[0];
    assert_eq!(executor.message.as_str(), "Local executor: target reached");
    let verdict = arrival(executor);
    assert_eq!(verdict.tolerance_m, ARRIVAL_TOLERANCE_M);
    assert_eq!(
        verdict.target.as_ref(),
        Some(&target),
        "the verdict describes the target the adapter accepted"
    );
    assert_eq!(verdict.duration_ms, accepted[0].duration_ms);
    assert_eq!(verdict.duration_ms, 2_000);
    assert!(verdict.arrived, "the goal must report arrival");
    let measured = verdict
        .measured_distance_m
        .expect("a connected fixture always measures the distance");
    assert!(
        measured <= verdict.tolerance_m,
        "an arrived verdict must be inside the tolerance: {measured} of {} blocks",
        verdict.tolerance_m
    );
    assert!(
        measured < ARRIVAL_TOLERANCE_M,
        "arrival is recorded only strictly inside the tolerance, not at {measured} blocks"
    );
    assert!(
        verdict.elapsed_ms < verdict.duration_ms,
        "an arrival is reported before its bound expires: {} of {} ms",
        verdict.elapsed_ms,
        verdict.duration_ms
    );
    assert_verdict_matches_its_observation(executor, verdict);

    // Both the observation the verdict measured from and the latest telemetry are inside the
    // tolerance, because the fixture only ever moves toward its bound target.
    let measured_from = executor
        .observation
        .as_ref()
        .expect("the event records the observation it measured");
    assert!(distance(&measured_from.position, &target) < ARRIVAL_TOLERANCE_M);
    let latest = view
        .observation
        .as_ref()
        .expect("the fixture keeps publishing telemetry");
    assert!(latest.connected);
    assert!(
        distance(&latest.position, &target) < ARRIVAL_TOLERANCE_M,
        "the bot must be observed inside the {ARRIVAL_TOLERANCE_M} block tolerance"
    );

    // The goal's local stop reaches the adapter: the bot stops moving and stays inside the
    // tolerance.
    let stopped_moving = wait_until_the_bot_stops(&engine);
    let settled = stopped_moving
        .observation
        .as_ref()
        .expect("telemetry continues after the goal")
        .position
        .clone();
    assert!(distance(&settled, &target) < ARRIVAL_TOLERANCE_M);

    // A one-step run ends there: no second request, action or verdict, and no active goal.
    assert!(view.active_goal.is_none());
    let timeline = serde_json::to_value(&view.events).unwrap();
    remains(&engine, |view| {
        view.requests == 1
            && count(view, "action") == 1
            && count(view, "executor") == 1
            && view.active_goal.is_none()
            && serde_json::to_value(&view.events).unwrap() == timeline
    });

    // The session's own local stop: recorded, disconnected, verdict unchanged.
    engine.send(Command::Stop);
    let stopped = wait_for(&engine, "local stop", |view| view.status == "Stopped");
    assert!(
        stopped
            .events
            .iter()
            .any(|event| event.kind == "control" && event.message == "Local stop and disconnect"),
        "the local stop must be recorded: {:?}",
        stopped
            .events
            .iter()
            .map(|event| event.message.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(stopped.requests, 1);
    assert_eq!(count(&stopped, "executor"), 1);
    assert!(stopped.active_goal.is_none());
    assert!(
        stopped
            .observation
            .as_ref()
            .is_some_and(|observation| !observation.connected),
        "a local stop closes the fixture connection"
    );
    let stopped_executors = executor_events(&stopped);
    assert_eq!(
        arrival(stopped_executors[0]),
        verdict,
        "the arrived verdict must survive the local stop unchanged"
    );
    remains(&engine, |view| {
        view.status == "Stopped" && view.active_goal.is_none() && count(view, "executor") == 1
    });
}

#[test]
fn a_goal_after_the_arrival_offers_no_navigate_candidate_and_states_why() {
    let engine = connect_reachable();
    engine.send(Command::Step);
    let arrived = wait_for(&engine, "first bounded goal arrives", |view| {
        count(view, "executor") == 1
    });
    let first_executors = executor_events(&arrived);
    let first_executor = first_executors[0];
    let verdict = arrival(first_executor);
    assert!(verdict.arrived);
    let target = verdict
        .target
        .clone()
        .expect("the arrived goal was bound to a target");
    assert!(
        verdict
            .measured_distance_m
            .expect("a connected fixture measures the distance")
            < ARRIVAL_TOLERANCE_M
    );

    // The bot now sits inside the tolerance of the waypoint, which is the geometry in which the
    // engine must stop offering it.
    engine.send(Command::Step);
    let view = wait_for(&engine, "second bounded goal request", |view| {
        count(view, "request") == 2
    });
    let requests: Vec<&Event> = view
        .events
        .iter()
        .filter(|event| event.kind == "request")
        .collect();
    assert_eq!(requests.len(), 2, "a one-step run has issued two requests");
    let first_request = requests[0];
    let second_request = requests[1];
    assert!(second_request.sequence > first_executor.sequence);
    assert!(
        first_request
            .candidates
            .iter()
            .any(|candidate| candidate.target.is_some()),
        "the first request must have offered the reachable waypoint: {:?}",
        first_request.candidates
    );

    let observation = second_request
        .observation
        .as_ref()
        .expect("a request records the observation it was shown");
    assert!(observation.connected);
    let remaining = distance(&observation.position, &target);
    assert!(
        remaining <= ARRIVAL_TOLERANCE_M,
        "the second decision must be asked from inside the tolerance: {remaining} blocks"
    );
    assert!(
        second_request
            .candidates
            .iter()
            .all(|candidate| candidate.target.is_none()),
        "no navigate candidate may be offered from inside the tolerance: {:?}",
        second_request.candidates
    );

    // The reason is on the recorded request, not implicit in the empty candidate list.
    let reason = format!(
        "no navigate candidate: {} observed waypoints, none between {ARRIVAL_TOLERANCE_M} and 12 blocks away",
        observation.blocks.len()
    );
    assert!(
        second_request.message.contains(reason.as_str()),
        "the recorded request must state why no navigation was offered: {}",
        second_request.message
    );
    // The target-less alternative remains, so the request is still answerable offline.
    assert!(
        second_request
            .candidates
            .iter()
            .any(|candidate| candidate.id == "wait"),
        "a request must keep a legal bounded choice: {:?}",
        second_request.candidates
    );

    engine.send(Command::Stop);
    let stopped = wait_for(&engine, "local stop", |view| view.status == "Stopped");
    assert!(stopped.active_goal.is_none());
    remains(&engine, |view| {
        view.status == "Stopped" && count(view, "request") == 2 && view.active_goal.is_none()
    });
}

#[test]
fn continuous_mode_records_the_arrival_verdict_before_the_next_request() {
    let engine = connect_reachable();
    engine.send(Command::Start);
    let view = wait_for(&engine, "second continuous request", |view| {
        count(view, "request") == 2
    });

    let events = &view.events;
    let action_index = events
        .iter()
        .position(|event| event.kind == "action")
        .expect("continuous mode dispatches the selected goal");
    let verdict_index = events
        .iter()
        .position(|event| event.arrival.is_some())
        .expect("the first bounded goal records an arrival verdict");
    // The first request that follows the accepted action. If continuous mode had moved on before
    // the goal ended, this search would find that premature request instead.
    let next_request_index = events
        .iter()
        .enumerate()
        .find(|(index, event)| *index > action_index && event.kind == "request")
        .map(|(index, _)| index)
        .expect("continuous mode requests again after the goal ends");

    assert!(
        action_index < verdict_index,
        "the verdict must follow the action the adapter accepted"
    );
    assert!(
        verdict_index < next_request_index,
        "the next request must wait for the arrival verdict: verdict at {verdict_index}, next request at {next_request_index}"
    );
    assert!(
        !events[action_index + 1..verdict_index]
            .iter()
            .any(|event| event.kind == "request"),
        "no request may be recorded between the accepted action and its verdict"
    );
    assert!(
        events[verdict_index].elapsed_ms <= events[next_request_index].elapsed_ms,
        "recorded elapsed time must not go backwards: verdict at {} ms, next request at {} ms",
        events[verdict_index].elapsed_ms,
        events[next_request_index].elapsed_ms
    );

    let executor = &events[verdict_index];
    assert_eq!(executor.kind, "executor");
    assert_eq!(executor.message.as_str(), "Local executor: target reached");
    let verdict = arrival(executor);
    assert!(verdict.arrived);
    assert_eq!(verdict.tolerance_m, ARRIVAL_TOLERANCE_M);
    let measured = verdict
        .measured_distance_m
        .expect("a connected fixture always measures the distance");
    assert!(
        measured < ARRIVAL_TOLERANCE_M,
        "the arrived verdict must be inside the tolerance: {measured} blocks"
    );
    assert!(
        verdict.elapsed_ms < verdict.duration_ms,
        "an arrival is reported before its bound expires: {} of {} ms",
        verdict.elapsed_ms,
        verdict.duration_ms
    );
    assert_verdict_matches_its_observation(executor, verdict);
    let accepted = accepted_candidates(&view);
    assert!(
        !accepted.is_empty(),
        "the navigate goal is accepted before its verdict"
    );
    assert_eq!(accepted[0].target, verdict.target);

    // Continuous mode moved on from a goal that really arrived: the next request is asked from
    // inside the tolerance, so the reached waypoint is gone from its candidate list.
    let next_request = &events[next_request_index];
    let observation = next_request
        .observation
        .as_ref()
        .expect("a request records the observation it was shown");
    let target = verdict
        .target
        .as_ref()
        .expect("the arrived goal was bound to a target");
    assert!(distance(&observation.position, target) < ARRIVAL_TOLERANCE_M);
    assert!(
        next_request
            .candidates
            .iter()
            .all(|candidate| candidate.target.is_none()),
        "a reached waypoint must not be offered again: {:?}",
        next_request.candidates
    );

    // A local stop ends continuous mode without issuing a further request.
    engine.send(Command::Stop);
    let stopped = wait_for(&engine, "stopped continuous session", |view| {
        view.status == "Stopped"
    });
    assert!(stopped.active_goal.is_none());
    remains(&engine, |view| {
        view.status == "Stopped" && count(view, "request") == 2
    });
}
