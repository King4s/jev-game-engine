use jev_game_engine::{
    adapter::{ActionRequest, AdapterCommand, AdapterHandle},
    fixture,
    latency::{LatencyPolicy, answer_is_current},
    model::{Candidate, Observation, Position},
};
use std::time::Duration;

#[test]
fn low_and_high_latency_adapt_without_removing_hard_bounds() {
    let mut low = LatencyPolicy::default();
    let initial = low.timing();
    assert_eq!(initial.p50_ms, None);
    assert_eq!(initial.p95_ms, None);
    for value in [40, 80, 100, 120, 150] {
        low.record(value);
    }
    let fast = low.timing();
    assert_eq!(fast.p50_ms, Some(100));
    assert_eq!(fast.p95_ms, Some(150));
    assert_eq!(fast.goal_ms, 2_000);
    assert_eq!(fast.max_answer_age_ms, 1_500);

    let mut high = LatencyPolicy::default();
    for value in [1_500, 2_000, 2_500, 3_000, 4_000] {
        high.record(value);
    }
    let slow = high.timing();
    assert_eq!(slow.p50_ms, Some(2_500));
    assert_eq!(slow.p95_ms, Some(4_000));
    assert_eq!(slow.goal_ms, 10_000);
    assert_eq!(slow.max_answer_age_ms, 5_000);
    assert!(slow.goal_ms > fast.goal_ms);
}

#[test]
fn rolling_window_forgets_old_slow_requests() {
    let mut policy = LatencyPolicy::default();
    for _ in 0..64 {
        policy.record(4_000);
    }
    for _ in 0..64 {
        policy.record(80);
    }
    let timing = policy.timing();
    assert_eq!(timing.p50_ms, Some(80));
    assert_eq!(timing.p95_ms, Some(80));
    assert_eq!(timing.goal_ms, 2_000);
    assert_eq!(timing.max_answer_age_ms, 1_500);
}

#[test]
fn extreme_latency_saturates_and_never_bypasses_generation_or_age() {
    for sample in [0, u64::MAX] {
        let mut policy = LatencyPolicy::default();
        policy.record(sample);
        let timing = policy.timing();
        assert!((2_000..=10_000).contains(&timing.goal_ms));
        assert!((1_500..=5_000).contains(&timing.max_answer_age_ms));
        assert!(answer_is_current(9, 9, timing.max_answer_age_ms, timing));
        assert!(!answer_is_current(
            9,
            9,
            timing.max_answer_age_ms + 1,
            timing
        ));
        assert!(!answer_is_current(9, 10, 0, timing));
        assert!(!answer_is_current(10, 9, 0, timing));
        assert!(!answer_is_current(9, 9, u64::MAX, timing));
    }
}

#[test]
fn new_samples_do_not_extend_a_previously_issued_request() {
    let mut policy = LatencyPolicy::default();
    policy.record(100);
    let issued = policy.timing();
    policy.record(4_000);
    assert!(!answer_is_current(2, 2, 2_000, issued));
    assert!(answer_is_current(2, 2, 2_000, policy.timing()));
}

fn movement(target: Position, duration_ms: u64) -> Candidate {
    Candidate {
        id: "navigate".into(),
        description: "Test navigation".into(),
        target: Some(target),
        duration_ms,
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

async fn wait_for_movement(adapter: &mut AdapterHandle, initial: &Position) -> Observation {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let observation = next_observation(adapter).await;
            if &observation.position != initial {
                return observation;
            }
        }
    })
    .await
    .expect("valid fixture action never moved")
}

async fn disconnect(mut adapter: AdapterHandle) {
    adapter.commands.send(AdapterCommand::Disconnect).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let observation = next_observation(&mut adapter).await;
            if !observation.connected {
                break;
            }
        }
    })
    .await
    .expect("disconnect was not published");
    tokio::time::timeout(Duration::from_secs(2), &mut adapter.task)
        .await
        .expect("fixture task did not terminate")
        .expect("fixture task panicked");
    assert!(adapter.commands.send(AdapterCommand::Stop).is_err());
}

#[tokio::test]
async fn valid_movement_stops_and_disconnect_closes_the_adapter() {
    let mut adapter = fixture::spawn();
    let initial = next_observation(&mut adapter).await;
    assert!(initial.connected);
    assert!(
        execute(
            &adapter,
            movement(
                Position {
                    x: 4.0,
                    y: 64.0,
                    z: 0.0,
                },
                10_000,
            ),
            &initial
        )
        .await
        .is_ok()
    );
    let moving = wait_for_movement(&mut adapter, &initial.position).await;
    assert!(moving.position.x > initial.position.x);
    adapter.commands.send(AdapterCommand::Stop).unwrap();
    // Allow a telemetry tick already racing with the command to settle.
    let _ = next_observation(&mut adapter).await;
    let settled = next_observation(&mut adapter).await;
    for _ in 0..3 {
        assert_eq!(
            next_observation(&mut adapter).await.position,
            settled.position
        );
    }
    disconnect(adapter).await;
}

#[tokio::test]
async fn actions_expire_without_another_model_or_stop_command() {
    let mut adapter = fixture::spawn();
    let initial = next_observation(&mut adapter).await;
    assert!(
        execute(
            &adapter,
            movement(
                Position {
                    x: 4.0,
                    y: 64.0,
                    z: 0.0,
                },
                250,
            ),
            &initial
        )
        .await
        .is_ok()
    );
    wait_for_movement(&mut adapter, &initial.position).await;
    tokio::time::sleep(Duration::from_millis(350)).await;
    let settled = next_observation(&mut adapter).await;
    for _ in 0..3 {
        assert_eq!(
            next_observation(&mut adapter).await.position,
            settled.position
        );
    }
    disconnect(adapter).await;
}

#[tokio::test]
async fn invalid_durations_do_not_move_the_fixture() {
    for duration in [0, 10_001, u64::MAX] {
        let mut adapter = fixture::spawn();
        let initial = next_observation(&mut adapter).await;
        assert!(
            execute(
                &adapter,
                movement(
                    Position {
                        x: 4.0,
                        y: 64.0,
                        z: 0.0,
                    },
                    duration,
                ),
                &initial
            )
            .await
            .is_err()
        );
        for _ in 0..3 {
            assert_eq!(
                next_observation(&mut adapter).await.position,
                initial.position,
                "accepted invalid duration {duration}"
            );
        }
        disconnect(adapter).await;
    }
}

#[tokio::test]
async fn fixture_rejects_targets_beyond_the_live_adapters_twelve_block_radius() {
    // Includes vertical distance: the real adapter validates a 3D radius.
    for target in [
        Position {
            x: 12.01,
            y: 64.0,
            z: 0.0,
        },
        Position {
            x: 1.0,
            y: 77.0,
            z: 0.0,
        },
    ] {
        let mut adapter = fixture::spawn();
        let initial = next_observation(&mut adapter).await;
        assert!(
            execute(&adapter, movement(target.clone(), 10_000), &initial)
                .await
                .is_err()
        );
        for _ in 0..3 {
            assert_eq!(
                next_observation(&mut adapter).await.position,
                initial.position,
                "out-of-radius target was executed: {target:?}"
            );
        }
        disconnect(adapter).await;
    }
}

#[tokio::test]
async fn fixture_rejects_nonfinite_target_coordinates() {
    for target in [
        Position {
            x: f64::INFINITY,
            y: 64.0,
            z: 0.0,
        },
        Position {
            x: 1.0,
            y: f64::NAN,
            z: 0.0,
        },
        Position {
            x: 1.0,
            y: 64.0,
            z: f64::NEG_INFINITY,
        },
    ] {
        let mut adapter = fixture::spawn();
        let initial = next_observation(&mut adapter).await;
        assert!(
            execute(&adapter, movement(target.clone(), 10_000), &initial)
                .await
                .is_err()
        );
        for _ in 0..3 {
            let observation = next_observation(&mut adapter).await;
            assert!(
                [
                    observation.position.x,
                    observation.position.y,
                    observation.position.z
                ]
                .iter()
                .all(|value| value.is_finite()),
                "invalid target poisoned fixture telemetry: {target:?}"
            );
            assert_eq!(
                observation.position, initial.position,
                "nonfinite target was executed: {target:?}"
            );
        }
        disconnect(adapter).await;
    }
}

#[tokio::test]
async fn unsupported_targetless_command_rejects_and_cancels_existing_movement() {
    let mut adapter = fixture::spawn();
    let initial = next_observation(&mut adapter).await;
    assert!(
        execute(
            &adapter,
            movement(
                Position {
                    x: 4.0,
                    y: 64.0,
                    z: 0.0,
                },
                10_000,
            ),
            &initial
        )
        .await
        .is_ok()
    );
    wait_for_movement(&mut adapter, &initial.position).await;
    assert!(
        execute(
            &adapter,
            Candidate {
                id: "teleport".into(),
                description: "Unsupported targetless action".into(),
                target: None,
                duration_ms: 1_000,
            },
            &initial
        )
        .await
        .is_err()
    );

    // Wait for observable rejection instead of assuming command receipt ordering.
    let rejected = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let observation = next_observation(&mut adapter).await;
            if observation.note.contains("rejected") {
                break observation;
            }
        }
    })
    .await
    .expect("unsupported command was not reported as rejected");
    assert!(rejected.connected, "rejection should retain the connection");
    for _ in 0..3 {
        assert_eq!(
            next_observation(&mut adapter).await.position,
            rejected.position,
            "rejected command left the previous action moving"
        );
    }
    disconnect(adapter).await;
}

async fn execute(
    adapter: &AdapterHandle,
    candidate: Candidate,
    observation: &Observation,
) -> Result<std::time::Instant, &'static str> {
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

async fn assert_rejection_stops_motion(adapter: &mut AdapterHandle) {
    let rejected = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let observation = next_observation(adapter).await;
            if observation.note.contains("rejected") {
                break observation;
            }
        }
    })
    .await
    .expect("fixture did not publish rejection");
    assert!(rejected.connected);
    for _ in 0..3 {
        assert_eq!(
            next_observation(adapter).await.position,
            rejected.position,
            "rejected request allowed continued movement"
        );
    }
}

#[tokio::test]
async fn mismatched_world_identity_rejects_with_ack_and_cancels_active_motion() {
    // Test each identity component separately, both before and during movement.
    for change_epoch in [true, false] {
        for already_moving in [false, true] {
            let mut adapter = fixture::spawn();
            let initial = next_observation(&mut adapter).await;
            let candidate = movement(
                Position {
                    x: 4.0,
                    y: 64.0,
                    z: 0.0,
                },
                10_000,
            );
            if already_moving {
                assert!(execute(&adapter, candidate.clone(), &initial).await.is_ok());
                wait_for_movement(&mut adapter, &initial.position).await;
            }
            let (mut request, reply) = ActionRequest::new(candidate, &initial);
            if change_epoch {
                request.world_epoch += 1;
            } else {
                request.dimension = Some("fixture:other_world".into());
            }
            adapter
                .commands
                .send(AdapterCommand::Execute(request))
                .unwrap();
            let result = tokio::time::timeout(Duration::from_secs(2), reply)
                .await
                .expect("missing rejection acknowledgement")
                .expect("rejection must be an explicit Result, not a dropped sender");
            assert!(result.is_err(), "world identity mismatch was accepted");
            assert_rejection_stops_motion(&mut adapter).await;
            if !already_moving {
                assert_eq!(
                    adapter.observations.borrow().as_ref().unwrap().position,
                    initial.position
                );
            }
            disconnect(adapter).await;
        }
    }
}

#[tokio::test]
async fn dropped_ack_receiver_prevents_execution_and_cancels_active_motion() {
    for already_moving in [false, true] {
        let mut adapter = fixture::spawn();
        let initial = next_observation(&mut adapter).await;
        let candidate = movement(
            Position {
                x: 4.0,
                y: 64.0,
                z: 0.0,
            },
            10_000,
        );
        if already_moving {
            assert!(execute(&adapter, candidate.clone(), &initial).await.is_ok());
            wait_for_movement(&mut adapter, &initial.position).await;
        }
        let (request, reply) = ActionRequest::new(candidate, &initial);
        // Drop before enqueueing: this deterministically models engine cancellation.
        drop(reply);
        adapter
            .commands
            .send(AdapterCommand::Execute(request))
            .unwrap();
        assert_rejection_stops_motion(&mut adapter).await;
        if !already_moving {
            assert_eq!(
                adapter.observations.borrow().as_ref().unwrap().position,
                initial.position
            );
        }
        disconnect(adapter).await;
    }
}

#[tokio::test]
async fn expired_acceptance_never_executes_and_stops_prior_motion() {
    for expires_while_queued in [false, true] {
        for already_moving in [false, true] {
            let mut adapter = fixture::spawn();
            let initial = next_observation(&mut adapter).await;
            let candidate = movement(
                Position {
                    x: 4.0,
                    y: 64.0,
                    z: 0.0,
                },
                10_000,
            );
            if already_moving {
                assert!(execute(&adapter, candidate.clone(), &initial).await.is_ok());
                wait_for_movement(&mut adapter, &initial.position).await;
            }
            let (request, reply) = ActionRequest::new(candidate, &initial);
            let accepted_before = if expires_while_queued {
                std::time::Instant::now() + Duration::from_millis(5)
            } else {
                std::time::Instant::now() - Duration::from_millis(1)
            };
            adapter
                .commands
                .send(AdapterCommand::Execute(
                    request.with_deadline(accepted_before),
                ))
                .unwrap();
            if expires_while_queued {
                // This current-thread test does not yield: the command expires in
                // the queue before the adapter can inspect it.
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(
                tokio::time::timeout(Duration::from_secs(2), reply)
                    .await
                    .unwrap()
                    .unwrap()
                    .is_err()
            );
            assert_rejection_stops_motion(&mut adapter).await;
            if !already_moving {
                assert_eq!(
                    adapter.observations.borrow().as_ref().unwrap().position,
                    initial.position
                );
            }
            disconnect(adapter).await;
        }
    }
}
