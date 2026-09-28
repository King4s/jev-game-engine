//! Explicit offline telemetry fixture; never a live game or Jev inference.
use crate::{
    adapter::{ActionRequest, AdapterCommand, AdapterHandle, execute_and_acknowledge},
    model::*,
    refusal::{self, Refusal},
    resources::nearest_resources,
};
use std::{collections::BTreeMap, time::Duration};
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

/// Fixed synthetic day time of the fixture: early morning, well before hostile spawning.
pub const FIXTURE_TIME_OF_DAY: u64 = 1000;

/// Fixed synthetic resources of the fixture as `(kind, name, position)`. Distances are
/// recomputed from the fixture bot's position on every observation.
pub const FIXTURE_RESOURCES: [(&str, &str, Position); 2] = [
    (
        "log",
        "minecraft:oak_log",
        Position {
            x: -3.5,
            y: 64.5,
            z: 2.5,
        },
    ),
    (
        "sheep",
        "minecraft:sheep",
        Position {
            x: -6.,
            y: 64.,
            z: -5.,
        },
    ),
];

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
            inventory: vec![
                "slot 36: WoodenPickaxe x1".into(),
                "slot 37: OakLog x3".into(),
            ],
            blocks: vec![Landmark {
                name: "waypoint".into(),
                position: waypoint,
            }],
            block_facts: vec![],
            entities: vec![],
            entity_scan_incomplete: false,
            note: "Synthetic offline fixture; no Minecraft connection".into(),
            time_of_day: Some(FIXTURE_TIME_OF_DAY),
            held_item: Some("minecraft:wooden_pickaxe".into()),
            items: BTreeMap::from([
                ("minecraft:oak_log".into(), 3),
                ("minecraft:wooden_pickaxe".into(), 1),
            ]),
            resources: vec![],
            skill_outcome: None,
            recoverable_drop: None,
            trees: Vec::new(),
            crafting_inventory: None,
        };
        // The fixture's own clock advances 50 ms per tick, so an action's bound, its speed and
        // therefore every test built on it are independent of how fast or how loaded the machine
        // is. A wall-clock bound here let one busy machine turn a passing run red.
        let mut active: Option<(Candidate, u64)> = None;
        let mut simulated_ms: u64 = 0;
        let mut tick = tokio::time::interval(Duration::from_millis(50));
        loop {
            tokio::select! {
                command = rx.recv() => match command {
                    Some(AdapterCommand::Execute(request)) => {
                        let candidate = request.candidate.clone();
                        // Match the live adapter: a new command cancels prior movement even when rejected.
                        active = None;
                        if let Some(refusal) = fixture_refusal(&request, &observation, &candidate) {
                            let _ = request.reply.send(Err(refusal.clone()));
                            observation.note = format!(
                                "Synthetic offline fixture; local guard rejected action: {refusal}"
                            );
                        } else {
                            match execute_and_acknowledge(request.reply, request.accepted_before, || Ok(()), || {}) {
                                Ok(_) => {
                                    observation.note = "Synthetic offline fixture; executing a bounded local action".into();
                                    active = Some((candidate, simulated_ms));
                                }
                                Err(refusal) => {
                                    observation.note = format!(
                                        "Synthetic offline fixture; local guard rejected action: {refusal}"
                                    );
                                }
                            }
                        }
                    }
                    Some(AdapterCommand::Stop) => active = None,
                    Some(AdapterCommand::Disconnect) | None => break,
                },
                _ = tick.tick() => {
                    simulated_ms = simulated_ms.saturating_add(50);
                    if let Some((candidate, started)) = &active {
                        if simulated_ms.saturating_sub(*started) >= candidate.duration_ms {
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
                    observation.resources = nearest_resources(&observation.position, FIXTURE_RESOURCES.iter().cloned());
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

/// Why the offline fixture refuses an action, or `None` when it carries it out.
///
/// The fixture is the synthetic twin of the live adapter, and it names its reasons the same
/// way: the world identity checks first, then the same candidate decision
/// ([`refusal::admit`]) the live guard applies. Its own executor is a synthetic walk to the
/// candidate's target and it holds the synthetic clock, so a skill candidate is admitted by
/// its target (the fixture has no harvester to prepare); nothing else differs, and no refusal
/// is a sentence that fits every failure.
fn fixture_refusal(
    request: &ActionRequest,
    observation: &Observation,
    candidate: &Candidate,
) -> Option<Refusal> {
    if request.reply.is_closed() {
        return Some(Refusal::CancelledBeforeGuard);
    }
    if request.world_epoch != observation.world_epoch {
        return Some(Refusal::WorldLifecycleChanged {
            action_epoch: request.world_epoch,
            world_epoch: observation.world_epoch,
        });
    }
    if request.dimension != observation.dimension {
        return Some(Refusal::WorldIdentityMismatch {
            expected: request.dimension.clone(),
            observed: observation.dimension.clone(),
        });
    }
    let walk = Candidate {
        skill: None,
        ..candidate.clone()
    };
    let distance = walk.target.as_ref().map(|target| {
        let dx = target.x - observation.position.x;
        let dy = target.y - observation.position.y;
        let dz = target.z - observation.position.z;
        dx.hypot(dy).hypot(dz)
    });
    refusal::admit(&walk, Some(observation.health), distance).err()
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn fixture_observation_exercises_survival_state() {
        let mut adapter = super::spawn();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            adapter.observations.changed(),
        )
        .await
        .unwrap()
        .unwrap();
        let observation = adapter.observations.borrow().clone().unwrap();
        assert!(observation.note.contains("fixture"));
        assert_eq!(observation.time_of_day, Some(super::FIXTURE_TIME_OF_DAY));
        assert_eq!(
            observation.held_item.as_deref(),
            Some("minecraft:wooden_pickaxe")
        );
        assert_eq!(observation.items["minecraft:oak_log"], 3);
        assert_eq!(observation.food, 20.0);
        for kind in ["log", "sheep"] {
            let resource = observation
                .resources
                .iter()
                .find(|r| r.kind == kind)
                .unwrap();
            assert!(resource.distance_m > 0.0 && resource.distance_m <= 16.0);
        }
        adapter
            .commands
            .send(crate::adapter::AdapterCommand::Disconnect)
            .unwrap();
        (&mut adapter.task).await.unwrap();
    }
}
