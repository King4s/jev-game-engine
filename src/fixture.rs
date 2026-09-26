//! Explicit offline telemetry fixture; never a live game or Jev inference.
use crate::{
    adapter::{AdapterCommand, AdapterHandle, execute_and_acknowledge},
    model::*,
};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};

/// Waypoint geometry of [`spawn`]: 5.657 blocks from the origin.
///
/// The fixture closes 0.1 blocks per 50 ms tick, so 2 blocks per second. At that rate this
/// waypoint is further away than one bounded demo goal (`goal_ms` 2000 closes at most 4.0
/// blocks) can reach, which is what lets [`spawn`] demonstrate the honest expiry path.
/// Adapter-level tests that need a goal to expire use it deliberately.
pub const DISTANT_WAYPOINT: Position = Position {
    x: 4.,
    y: 64.,
    z: 4.,
};

/// Waypoint geometry for tests and tools that need one bounded goal to arrive: 1.5 blocks
/// from the origin.
///
/// One bounded goal closes the remaining 0.9 blocks in roughly 0.45 s, so the same 2000 ms
/// bound that expires against [`DISTANT_WAYPOINT`] records a genuine `arrived` verdict here.
/// The waypoint stays outside the arrival tolerance, so it is offered as a navigate
/// candidate instead of being filtered out. The desktop app's offline demo uses
/// [`DISTANT_WAYPOINT`] instead, on purpose.
pub const REACHABLE_WAYPOINT: Position = Position {
    x: 1.5,
    y: 64.,
    z: 0.,
};

/// Synthetic fixture with the distant waypoint, unchanged for adapter-level tests.
pub fn spawn() -> AdapterHandle {
    spawn_with_waypoint(DISTANT_WAYPOINT)
}

/// Synthetic fixture with an explicit waypoint geometry, so a test or tool chooses whether a
/// bounded goal arrives or expires. The bot starts at the origin either way.
pub fn spawn_with_waypoint(waypoint: Position) -> AdapterHandle {
    let (commands, mut rx) = mpsc::unbounded_channel();
    let (tx, observations) = watch::channel(None);
    let (error_tx, errors) = watch::channel(None);
    let task = tokio::spawn(async move {
        let _error_tx = error_tx;
        let mut observation = Observation {
            world_epoch: 1,
            dimension: Some("fixture:overworld".into()),
            deaths: 0,
            sequence: 0,
            connected: true,
            position: Position {
                x: 0.,
                y: 64.,
                z: 0.,
            },
            health: 20.,
            food: 20.,
            inventory: vec![],
            blocks: vec![Landmark {
                name: "waypoint".into(),
                position: waypoint,
            }],
            entities: vec![],
            note: "Synthetic offline fixture; no Minecraft connection".into(),
        };
        let mut active: Option<(Candidate, Instant)> = None;
        let mut tick = tokio::time::interval(Duration::from_millis(50));
        loop {
            tokio::select! {
                command = rx.recv() => match command {
                    Some(AdapterCommand::Execute(request)) => {
                        let candidate = request.candidate;
                        // Match the live adapter: a new command cancels prior movement even when rejected.
                        active = None;
                        if !request.reply.is_closed() && request.world_epoch == observation.world_epoch
                            && request.dimension == observation.dimension
                            && valid_action(&candidate, &observation.position) {
                            if execute_and_acknowledge(request.reply, request.accepted_before, || Ok(()), || {}) {
                                observation.note = "Synthetic offline fixture; executing a bounded local action".into();
                                active = Some((candidate,Instant::now()));
                            } else {
                                observation.note = "Synthetic offline fixture; action rejected: expired or cancelled before acceptance".into();
                            }
                        } else {
                            let _ = request.reply.send(Err("Local fixture guard rejected invalid or stale action"));
                            observation.note = "Synthetic offline fixture; local guard rejected invalid action".into();
                        }
                    }
                    Some(AdapterCommand::Stop) => active = None,
                    Some(AdapterCommand::Disconnect) | None => break,
                },
                _ = tick.tick() => {
                    if let Some((candidate, started)) = &active {
                        if started.elapsed().as_millis() >= candidate.duration_ms as u128 {
                            active = None;
                        } else if let Some(target) = &candidate.target {
                            let dx=target.x-observation.position.x;
                            let dz=target.z-observation.position.z;
                            let distance=dx.hypot(dz);
                            if distance > 0.05 {
                                let step=0.1_f64.min(distance);
                                observation.position.x += dx/distance*step;
                                observation.position.z += dz/distance*step;
                            }
                        }
                    }
                    observation.sequence += 1;
                    if tx.send(Some(observation.clone())).is_err() { break; }
                }
            }
        }
        observation.connected = false;
        let _ = tx.send(Some(observation));
    });
    AdapterHandle {
        commands,
        observations,
        errors,
        task,
    }
}

fn valid_action(candidate: &Candidate, position: &Position) -> bool {
    if !(1..=10_000).contains(&candidate.duration_ms) {
        return false;
    }
    let Some(target) = &candidate.target else {
        return matches!(candidate.id.as_str(), "wait" | "stop");
    };
    if ![target.x, target.y, target.z]
        .iter()
        .all(|value| value.is_finite() && value.abs() < 30_000_000.0)
    {
        return false;
    }
    let dx = target.x - position.x;
    let dy = target.y - position.y;
    let dz = target.z - position.z;
    dx.hypot(dy).hypot(dz) <= 12.0
}
