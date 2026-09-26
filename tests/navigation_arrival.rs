//! Navigation arrival-verdict criteria for the bounded local executor.
//!
//! Every bounded goal that ends records an `executor` event carrying a locally measured
//! verdict; a recording that carries an arrived verdict must be rejected when its measured
//! distance is missing and must survive serde, `recording::load` and `Command::Replay`
//! unchanged. Everything here is deterministic and offline: the synthetic fixture and the
//! offline decision path only, never Minecraft, never Jev.

use std::time::{Duration, Instant};

use jev_game_engine::{
    adapter::{ActionRequest, AdapterCommand, AdapterHandle},
    engine::EngineHandle,
    fixture,
    model::{
        ARRIVAL_TOLERANCE_M, ArrivalVerdict, Candidate, Command, Event, Observation, Position,
        Recording, Settings, View,
    },
    recording,
};

/// `src/fixture.rs` moves the bot 0.1 blocks per 50 ms tick, so telemetry closes at most
/// two blocks per second of bounded goal duration.
const FIXTURE_SPEED_M_PER_S: f64 = 2.0;

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

fn connect() -> EngineHandle {
    let engine = EngineHandle::new();
    engine.send(Command::Connect(Settings::default()));
    wait_for(&engine, "connected fixture", |view| {
        view.observation
            .as_ref()
            .is_some_and(|observation| observation.connected)
    });
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

/// Blocks a bounded goal of `duration_ms` can close at the fixture's pace.
fn reachable_blocks(duration_ms: u64) -> f64 {
    duration_ms as f64 / 1_000.0 * FIXTURE_SPEED_M_PER_S
}

/// The verdict must agree with the observation recorded on the very same event.
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

/// Distance the first model-selected navigate goal had to close, taken from the request
/// the model was shown. A bounded goal that cannot close it cannot arrive.
fn first_navigate_distance(view: &View) -> f64 {
    let request = view
        .events
        .iter()
        .find(|event| {
            event.kind == "request" && event.candidates.iter().any(|c| c.target.is_some())
        })
        .expect("recorded navigate request");
    let candidate = request
        .candidates
        .iter()
        .find(|candidate| candidate.target.is_some())
        .expect("navigate candidate");
    let target = candidate.target.as_ref().expect("bounded target");
    let observation = request
        .observation
        .as_ref()
        .expect("recorded request observation");
    distance(&observation.position, target)
}

fn fixture_waypoint(observation: &Observation) -> Position {
    observation
        .blocks
        .iter()
        .find(|landmark| landmark.name == "waypoint")
        .expect("the fixture publishes a waypoint landmark")
        .position
        .clone()
}

fn observation() -> Observation {
    Observation {
        world_epoch: 1,
        dimension: Some("fixture:overworld".into()),
        sequence: 7,
        connected: true,
        position: Position {
            x: 0.0,
            y: 64.0,
            z: 0.0,
        },
        health: 20.0,
        food: 20.0,
        inventory: vec![],
        blocks: vec![],
        entities: vec![],
        note: "arrival verdict test recording".into(),
    }
}

fn arrived_verdict(measured_distance_m: Option<f64>) -> ArrivalVerdict {
    ArrivalVerdict {
        target: Some(Position {
            x: 4.0,
            y: 64.0,
            z: 4.0,
        }),
        measured_distance_m,
        tolerance_m: ARRIVAL_TOLERANCE_M,
        duration_ms: 2_000,
        elapsed_ms: 1_500,
        arrived: true,
    }
}

/// One otherwise valid recording whose only event is the executor event of a goal.
fn arrived_recording(verdict: ArrivalVerdict) -> Recording {
    Recording {
        schema_version: 1,
        id: "arrival-verdict".into(),
        settings: Settings::default(),
        events: vec![Event {
            sequence: 1,
            elapsed_ms: 1_800,
            kind: "executor".into(),
            message: "Local executor: target reached".into(),
            observation: Some(observation()),
            candidates: vec![],
            decision: None,
            arrival: Some(verdict),
        }],
    }
}

#[test]
fn bounded_navigation_expiry_records_an_honest_verdict_and_no_active_goal() {
    let engine = connect();
    engine.send(Command::Step);
    let view = wait_for(&engine, "expired bounded goal", |view| {
        count(view, "executor") == 1
    });

    let executors = executor_events(&view);
    let executor = executors[0];
    assert_eq!(
        executor.message.as_str(),
        "Local executor: goal duration expired without arrival"
    );
    let verdict = arrival(executor);
    assert_eq!(verdict.tolerance_m, ARRIVAL_TOLERANCE_M);
    // The fixture waypoint is further away than one bounded goal can close, so this goal
    // must expire instead of arriving.
    let start = first_navigate_distance(&view);
    assert!(
        reachable_blocks(verdict.duration_ms) < start - ARRIVAL_TOLERANCE_M,
        "one bounded goal of {} ms closes at most {:.3} of the {start:.3} blocks to the waypoint",
        verdict.duration_ms,
        reachable_blocks(verdict.duration_ms)
    );
    assert!(
        !verdict.arrived,
        "a goal that cannot close the distance cannot report arrival"
    );
    let measured = verdict
        .measured_distance_m
        .expect("a connected fixture always measures the distance");
    assert!(
        measured > ARRIVAL_TOLERANCE_M,
        "an expiry verdict must not be recorded below the tolerance: {measured} blocks"
    );
    assert_verdict_matches_its_observation(executor, verdict);

    // The verdict describes the candidate the adapter accepted, not a fresh choice.
    let accepted = accepted_candidates(&view);
    assert_eq!(accepted.len(), 1);
    assert_eq!(verdict.target, accepted[0].target);
    assert_eq!(verdict.duration_ms, accepted[0].duration_ms);
    let recorded = executor.observation.as_ref().expect("executor observation");
    assert_eq!(verdict.target.as_ref(), Some(&fixture_waypoint(recorded)));

    // A one-step run ends there: no further request, action or verdict is issued.
    assert!(view.active_goal.is_none());
    assert_eq!(view.requests, 1);
    assert_eq!(count(&view, "action"), 1);
    let timeline = serde_json::to_value(&view.events).unwrap();
    remains(&engine, |view| {
        view.requests == 1
            && count(view, "action") == 1
            && count(view, "executor") == 1
            && view.active_goal.is_none()
            && serde_json::to_value(&view.events).unwrap() == timeline
    });
}

#[test]
fn arrival_flips_only_below_the_tolerance_and_the_verdict_arithmetic_is_consistent() {
    let engine = connect();
    engine.send(Command::Step);
    let expired = wait_for(&engine, "expired first bounded goal", |view| {
        count(view, "executor") == 1
    });
    let expired_events = executor_events(&expired);
    let first = expired_events[0];
    let first_verdict = arrival(first).clone();

    assert_eq!(first_verdict.tolerance_m, ARRIVAL_TOLERANCE_M);
    let start = first_navigate_distance(&expired);
    assert!(
        reachable_blocks(first_verdict.duration_ms) < start - ARRIVAL_TOLERANCE_M,
        "one bounded goal of {} ms closes at most {:.3} of the {start:.3} blocks to the waypoint",
        first_verdict.duration_ms,
        reachable_blocks(first_verdict.duration_ms)
    );
    assert!(!first_verdict.arrived);
    assert!(
        first_verdict.elapsed_ms >= first_verdict.duration_ms,
        "an expiry is only reported at or after its bound: {} of {} ms",
        first_verdict.elapsed_ms,
        first_verdict.duration_ms
    );
    let first_measured = first_verdict
        .measured_distance_m
        .expect("a connected fixture always measures the distance");
    assert!(
        first_measured > ARRIVAL_TOLERANCE_M,
        "a non-arrival verdict must sit above the tolerance: {first_measured} blocks"
    );
    assert_verdict_matches_its_observation(first, &first_verdict);

    // The first goal left the bot close enough for the next bounded goal to close the gap.
    assert!(
        first_measured - ARRIVAL_TOLERANCE_M <= reachable_blocks(first_verdict.duration_ms),
        "the first goal stopped {first_measured} blocks out, beyond the {:.3} blocks a bounded goal closes",
        reachable_blocks(first_verdict.duration_ms)
    );

    engine.send(Command::Step);
    let reached = wait_for(&engine, "arrived second bounded goal", |view| {
        count(view, "executor") == 2
    });
    let reached_events = executor_events(&reached);
    let second = reached_events[1];
    assert_eq!(second.message.as_str(), "Local executor: target reached");
    let second_verdict = arrival(second);
    let second_measured = second_verdict
        .measured_distance_m
        .expect("a reached target is measured from an observation");
    assert!(
        second_verdict.arrived,
        "the second bounded goal must enter the tolerance; after {} of {} ms it left \
         {second_measured} blocks",
        second_verdict.elapsed_ms, second_verdict.duration_ms
    );
    assert!(
        second_measured < ARRIVAL_TOLERANCE_M,
        "arrival is recorded only strictly below the tolerance, not at {second_measured} blocks"
    );
    assert_eq!(second_verdict.tolerance_m, ARRIVAL_TOLERANCE_M);
    assert!(
        second_verdict.elapsed_ms < second_verdict.duration_ms,
        "an arrival is reported before its bound expires: {} of {} ms",
        second_verdict.elapsed_ms,
        second_verdict.duration_ms
    );
    assert_verdict_matches_its_observation(second, second_verdict);

    // Both goals were bound to the same waypoint, so only the observed distance flipped
    // arrived, and every verdict field matches the candidate the adapter accepted.
    assert_eq!(second_verdict.target, first_verdict.target);
    let accepted = accepted_candidates(&reached);
    assert_eq!(accepted.len(), 2);
    assert_eq!(accepted[0].target, first_verdict.target);
    assert_eq!(accepted[0].duration_ms, first_verdict.duration_ms);
    assert_eq!(accepted[1].target, second_verdict.target);
    assert_eq!(accepted[1].duration_ms, second_verdict.duration_ms);
    assert!(first_measured > ARRIVAL_TOLERANCE_M && second_measured < ARRIVAL_TOLERANCE_M);

    assert!(reached.active_goal.is_none());
    assert_eq!(reached.requests, 2);
    remains(&engine, |view| {
        count(view, "executor") == 2 && view.requests == 2 && view.active_goal.is_none()
    });
}

#[test]
fn validate_rejects_an_arrived_verdict_without_a_measured_distance() {
    let rejected = recording::validate(&arrived_recording(arrived_verdict(None)))
        .expect_err("an arrived verdict without a measured distance must be rejected");
    assert!(
        rejected.to_string().contains("without a measured distance"),
        "unexpected validation error: {rejected}"
    );

    let accepted = recording::validate(&arrived_recording(arrived_verdict(Some(0.42))));
    assert!(
        accepted.is_ok(),
        "the same verdict with a finite measured distance is valid: {accepted:?}"
    );
}

#[test]
fn an_arrived_verdict_survives_serde_load_and_replay_without_new_requests() {
    let verdict = arrived_verdict(Some(0.42));
    let original = arrived_recording(verdict.clone());
    let serialized = serde_json::to_vec_pretty(&original).unwrap();

    let round_tripped: Recording = serde_json::from_slice(&serialized).unwrap();
    assert_eq!(
        round_tripped.events[0].arrival.as_ref(),
        Some(&verdict),
        "serde must preserve every verdict field exactly"
    );
    assert_eq!(
        serde_json::to_value(&round_tripped).unwrap(),
        serde_json::to_value(&original).unwrap()
    );

    // Recordings written before the verdict existed stay loadable, without one.
    let mut legacy = serde_json::to_value(&original).unwrap();
    assert!(
        legacy["events"][0]
            .as_object_mut()
            .expect("recorded event object")
            .remove("arrival")
            .is_some()
    );
    let legacy: Recording = serde_json::from_value(legacy).unwrap();
    assert!(legacy.events[0].arrival.is_none());
    assert!(recording::validate(&legacy).is_ok());

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("arrival.json");
    std::fs::write(&path, &serialized).unwrap();
    let loaded = recording::load(path.to_str().unwrap()).unwrap();
    let loaded_verdict = loaded.events[0]
        .arrival
        .as_ref()
        .expect("loading preserves the verdict");
    assert_eq!(loaded_verdict, &verdict);
    assert_eq!(loaded_verdict.measured_distance_m, Some(0.42));
    assert_eq!(loaded_verdict.tolerance_m, ARRIVAL_TOLERANCE_M);

    let engine = connect();
    engine.send(Command::Replay(path.to_string_lossy().into_owned()));
    let view = wait_for(&engine, "loaded arrival replay", |view| view.replay);
    assert_eq!(view.requests, 0, "a replay must never issue a request");
    assert_eq!(count(&view, "request"), 0);
    assert_eq!(count(&view, "action"), 0);
    assert_eq!(count(&view, "executor"), 1);
    let executors = executor_events(&view);
    assert_eq!(arrival(executors[0]), &verdict);
    let timeline = serde_json::to_value(&view.events).unwrap();
    remains(&engine, |view| {
        view.requests == 0
            && count(view, "request") == 0
            && count(view, "action") == 0
            && serde_json::to_value(&view.events).unwrap() == timeline
    });
}

#[test]
fn invalidating_a_pending_decision_never_dispatches_or_records_a_verdict() {
    for invalidation in ["pause", "stop"] {
        let engine = connect();
        engine.send(Command::Step);
        let pending = wait_for(&engine, "pending fixture decision", |view| {
            count(view, "request") == 1
        });
        assert_eq!(
            count(&pending, "action"),
            0,
            "the fixture response must still be pending for this invalidation test"
        );
        match invalidation {
            "pause" => {
                engine.send(Command::Pause);
                wait_for(&engine, "paused session", |view| {
                    view.status.starts_with("Paused")
                });
            }
            _ => {
                engine.send(Command::Stop);
                wait_for(&engine, "stopped session", |view| view.status == "Stopped");
            }
        }
        remains(&engine, |view| {
            count(view, "dispatched") == 0
                && count(view, "action") == 0
                && count(view, "executor") == 0
                && view.requests == 1
                && view.active_goal.is_none()
                && !view.events.iter().any(|event| event.arrival.is_some())
        });
    }
}

async fn next_observation(adapter: &mut AdapterHandle) -> Observation {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            adapter
                .observations
                .changed()
                .await
                .expect("fixture observation channel closed unexpectedly");
            if let Some(observation) = adapter.observations.borrow_and_update().clone() {
                return observation;
            }
        }
    })
    .await
    .expect("fixture did not publish telemetry within two seconds")
}

async fn execute(
    adapter: &AdapterHandle,
    candidate: Candidate,
    observation: &Observation,
) -> Result<Instant, &'static str> {
    let (request, acknowledgement) = ActionRequest::new(candidate, observation);
    adapter
        .commands
        .send(AdapterCommand::Execute(request))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), acknowledgement)
        .await
        .expect("adapter acknowledgement timed out")
        .expect("adapter acknowledgement channel closed")
}

async fn disconnect(mut adapter: AdapterHandle) {
    adapter
        .commands
        .send(AdapterCommand::Disconnect)
        .expect("fixture command channel");
    tokio::time::timeout(Duration::from_secs(2), &mut adapter.task)
        .await
        .expect("fixture task did not terminate")
        .expect("fixture task panicked");
}

#[tokio::test]
async fn fixture_navigation_enters_the_tolerance_within_the_bounded_duration() {
    let mut adapter = fixture::spawn();
    let initial = next_observation(&mut adapter).await;
    let waypoint = fixture_waypoint(&initial);
    let start = distance(&initial.position, &waypoint);
    assert!(
        start > ARRIVAL_TOLERANCE_M,
        "the fixture must start outside the tolerance: {start:.3} blocks"
    );
    let duration_ms = 6_000;
    let candidate = Candidate {
        id: "navigate".into(),
        description: "Bounded navigation to the observed fixture waypoint".into(),
        target: Some(waypoint.clone()),
        duration_ms,
    };
    let accepted_at = execute(&adapter, candidate, &initial)
        .await
        .expect("the fixture accepts a reachable bounded navigate candidate");

    let mut previous_distance = start;
    let mut previous_sequence = initial.sequence;
    let reached = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let observation = next_observation(&mut adapter).await;
            assert!(
                observation.sequence > previous_sequence,
                "fixture observations must arrive in order"
            );
            previous_sequence = observation.sequence;
            let measured = distance(&observation.position, &waypoint);
            assert!(
                measured <= previous_distance + 1e-9,
                "navigation moved away from its bounded target: {measured} > {previous_distance}"
            );
            previous_distance = measured;
            if measured < ARRIVAL_TOLERANCE_M {
                return observation;
            }
        }
    })
    .await
    .expect("fixture navigation did not enter the tolerance inside its bounded duration");

    assert!(reached.connected);
    let elapsed = accepted_at.elapsed();
    assert!(
        elapsed < Duration::from_millis(duration_ms),
        "the tolerance was entered after the {duration_ms} ms bound expired: {elapsed:?}"
    );
    disconnect(adapter).await;
}
