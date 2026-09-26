//! Minecraft 26.2 adapter for the pinned Azalea revision. Local server only.
use std::{
    net::{Ipv4Addr, SocketAddr},
    time::{Duration, Instant},
};

use azalea::{
    BlockPos, Client, Event, StartClientOpts, Vec3, WalkDirection,
    account::Account,
    pathfinder::{PathfinderClientExt, PathfinderOpts, goals::BlockPosGoal},
    physics::collision::BlockWithShape,
    protocol::address::ResolvedAddr,
};
use tokio::sync::{mpsc, watch};

use crate::{
    adapter::{ActionRequest, AdapterCommand, AdapterHandle, execute_and_acknowledge},
    model::{Candidate, Landmark, Observation, Position, Settings},
};

pub fn spawn(settings: Settings) -> AdapterHandle {
    let (commands, receiver) = mpsc::unbounded_channel();
    let (sender, observations) = watch::channel(None);
    let (error_sender, errors) = watch::channel(None);
    // Azalea's ECS runner uses spawn_local. Keep its runtime and LocalSet alive
    // on one dedicated blocking-pool thread until Disconnect closes the run.
    // A running spawn_blocking task cannot be aborted; AdapterHandle::drop
    // sends Disconnect, and dropped channels also cause run() to terminate.
    let task = tokio::task::spawn_blocking(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(_) => {
                error_sender
                    .send_replace(Some("Could not initialize Minecraft local runtime.".into()));
                return;
            }
        };
        tokio::task::LocalSet::new()
            .block_on(&runtime, run(settings, receiver, sender, error_sender));
    });
    AdapterHandle {
        commands,
        observations,
        errors,
        task,
    }
}

// Exiting the adapter also terminates Azalea's internal ECS/network tasks.
struct Connection(Client);
impl Drop for Connection {
    fn drop(&mut self) {
        stop(&self.0);
        self.0.disconnect();
        self.0.exit();
    }
}

struct PendingConnection(Option<Box<dyn FnOnce() + Send>>);
impl Drop for PendingConnection {
    fn drop(&mut self) {
        if let Some(cleanup) = self.0.take() {
            cleanup();
        }
    }
}

async fn run(
    settings: Settings,
    mut commands: mpsc::UnboundedReceiver<AdapterCommand>,
    observations: watch::Sender<Option<Observation>>,
    errors: watch::Sender<Option<String>>,
) {
    if settings.port == 0
        || settings.bot_name.is_empty()
        || settings.bot_name.len() > 16
        || !settings
            .bot_name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        errors.send_replace(Some(
            "Invalid local Minecraft port or bot name (1–16 ASCII letters, digits, underscore)."
                .into(),
        ));
        return;
    }
    let socket = SocketAddr::from((Ipv4Addr::LOCALHOST, settings.port));
    let account = Account::offline(&settings.bot_name);
    let mut address = ResolvedAddr {
        server: socket.into(),
        socket,
    };
    if settings.legacy_forwarding {
        // Explicit support for the owner's legacy-forwarding backend through
        // loopback/SSH. Forward only this account's own derived offline UUID.
        // The TCP destination remains loopback; no arbitrary identity override.
        address.server.host = format!(
            "127.0.0.1\0{}\0{}",
            Ipv4Addr::LOCALHOST,
            account.uuid().simple()
        );
    }
    let (event_sender, mut events) = mpsc::unbounded_channel();
    let (opts, _exit_receiver) =
        StartClientOpts::new_with_appexit_rx(account, address, Some(event_sender));
    let ecs = opts.ecs_lock.clone();
    let mut pending = PendingConnection(Some(Box::new(move || {
        ecs.write().write_message(azalea::app::AppExit::Success);
    })));
    let joining = Client::start_client(opts);
    tokio::pin!(joining);
    let join_timeout = tokio::time::sleep(Duration::from_secs(20));
    tokio::pin!(join_timeout);
    let bot = loop {
        tokio::select! {
            biased;
            command = commands.recv() => match command {
                None | Some(AdapterCommand::Disconnect) => return,
                Some(AdapterCommand::Stop) => {},
                Some(AdapterCommand::Execute(request)) => { let _ = request.reply.send(Err("Minecraft is still connecting")); },
            },
            _ = &mut join_timeout => {
                errors.send_replace(Some("Local Minecraft connection initialization timed out.".into()));
                return;
            },
            bot = &mut joining => break bot,
        }
    };
    let connection = Connection(bot);
    pending.0 = None;
    let bot = &connection.0;
    let mut ready = false;
    let mut sequence = 0;
    let mut deadline: Option<Instant> = None;
    let mut last_snapshot = Instant::now();
    let mut last_tick = Instant::now();
    let mut connected_at = Instant::now();
    let mut world_epoch = 0_u64;
    let mut deferred: Option<ActionRequest> = None;
    let mut deferred_ready = false;
    let mut last_health: Option<f32> = None;
    // Monotonic per connection; stamped on every observation so the engine sees a death
    // even when the respawn observation replaces the dying one.
    let mut deaths = 0_u64;
    // A flight goal keeps running when the bot is hit: stopping under fire is what let
    // skeleton arrows land every shot in the first live night run.
    let mut fleeing = false;
    let mut note =
        String::from("Live server observation; local physics and pathfinding; no mining.");
    let mut timer = tokio::time::interval(Duration::from_millis(25));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        // Arm only after all events queued before stop have been consumed. A
        // subsequent Tick then proves another synchronous ECS update completed.
        if deferred.is_some() && events.is_empty() {
            deferred_ready = true;
        }
        tokio::select! {
            biased;
            _ = timer.tick() => {
                if observations.is_closed() { break; }
                if !ready && connected_at.elapsed() > Duration::from_secs(20) {
                    errors.send_replace(Some("Minecraft did not spawn within 20 seconds. Check local server authentication and version.".into()));
                    break;
                }
                let stale = last_tick.elapsed() > Duration::from_millis(500);
                if stale { reject_deferred(&mut deferred, "Action rejected because local game ticks became stale"); }
                if deadline.is_some_and(|d| Instant::now() >= d) || (deadline.is_some() && stale) {
                    stop(bot);
                    deadline = None;
                    note = "Local watchdog stopped movement: action expired or game ticks became stale.".into();
                }
                if ready && last_snapshot.elapsed() >= Duration::from_millis(250) {
                    last_snapshot = Instant::now();
                    if let Some(mut observation) = observe(bot, sequence + 1, world_epoch, &note) {
                        sequence += 1;
                        observation.deaths = deaths;
                        let hurt = last_health.is_some_and(|old| observation.health < old);
                        if observation.health > 0.0 && hurt && fleeing && deadline.is_some() {
                            note = "Damage during a flight goal; movement continues.".into();
                        } else if observation.health <= 0.0 || hurt {
                            reject_deferred(&mut deferred, "Health decreased before action acceptance");
                            stop(bot);
                            deadline = None;
                            fleeing = false;
                            note = "Local health guard stopped movement after damage.".into();
                        }
                        last_health = Some(observation.health);
                        if stale { observation.note.push_str(" Game ticks stale; local movement stopped."); }
                        observations.send_replace(Some(observation));
                    } else {
                        reject_deferred(&mut deferred, "World observation unavailable before action acceptance");
                        stop(bot);
                        deadline = None;
                    }
                }
            },
            command = commands.recv() => match command {
                None | Some(AdapterCommand::Disconnect) => break,
                Some(AdapterCommand::Stop) => {
                    reject_deferred(&mut deferred, "Action cancelled by local stop");
                    stop(bot);
                    deadline = None;
                    note = "Local operator stop; pathfinding and movement cancelled.".into();
                }
                Some(AdapterCommand::Execute(request)) => {
                    stop(bot);
                    deadline = None;
                    reject_deferred(&mut deferred, "Action superseded by a newer command");
                    // goto_listener precedes stop processing in Azalea Update. Starting
                    // goto now would have its ComputePath removed by this same stop.
                    deferred = Some(request);
                    deferred_ready = false;
                    note = "Action dispatched; waiting for local stop update before validation.".into();
                }
            },
            event = events.recv() => match event {
                Some(Event::ConnectionFailed(_)) => {
                    errors.send_replace(Some("Local Minecraft connection failed. Check server port and Minecraft 26.2 compatibility.".into()));
                    break;
                }
                None | Some(Event::Disconnect(_)) => {
                    errors.send_replace(Some("Local Minecraft server disconnected.".into()));
                    break;
                }
                Some(Event::Spawn) => {
                    reject_deferred(&mut deferred, "World lifecycle changed before action acceptance");
                    stop(bot);
                    deadline = None;
                    world_epoch = world_epoch.wrapping_add(1);
                    ready = true;
                    last_health = None;
                    last_tick = Instant::now();
                    fleeing = false;
                    note = "World spawned; prior movement cancelled.".into();
                    if let Some(mut observation) = observe(bot, sequence + 1, world_epoch, &note) {
                        sequence += 1;
                        observation.deaths = deaths;
                        observations.send_replace(Some(observation));
                    }
                }
                Some(Event::Death(_)) => {
                    deaths = deaths.wrapping_add(1);
                    reject_deferred(&mut deferred, "Bot died before action acceptance");
                    stop(bot);
                    deadline = None;
                    fleeing = false;
                    note = "Bot died; movement stopped.".into();
                }
                Some(Event::Login) => { reject_deferred(&mut deferred, "Login interrupted pending action"); ready = false; connected_at = Instant::now(); stop(bot); deadline = None; }
                Some(Event::Tick) => {
                    last_tick = Instant::now();
                    // The prior ECS update consumed stop before a new goto is enqueued.
                    if deferred_ready && events.is_empty()
                        && let Some(request) = deferred.take() {
                        let candidate = request.candidate;
                    if ready && !request.reply.is_closed()
                        && request.world_epoch == world_epoch
                        && bot.world_name().ok().map(|name| name.0.to_string()) == request.dimension {
                        if execute_and_acknowledge(request.reply, request.accepted_before,
                            || execute(bot, &candidate, request.accepted_before).map_err(|()| "Local guard rejected invalid, expired or unsafe action"),
                            || stop(bot)) {
                            deadline = Some(Instant::now() + Duration::from_millis(candidate.duration_ms));
                            fleeing = candidate.id.starts_with("flee_");
                            note = "Adapter accepted bounded action; local Azalea pathfinding, not proof of movement.".into();
                        } else {
                            deadline = None;
                            note = "Local guard rejected or cancelled action before acceptance.".into();
                        }
                    } else {
                        let _ = request.reply.send(Err("Local guard rejected action: stale world lifecycle or observation"));
                        note = "Local guard rejected action: no fresh matching spawned world observation.".into();
                    }
                    }
                }
                _ => {},
            }
        }
    }
    observations.send_modify(|value| {
        if let Some(observation) = value {
            observation.sequence += 1;
            observation.connected = false;
            observation.note = "Disconnected; values are the last observed server state.".into();
        }
    });
}

fn reject_deferred(deferred: &mut Option<ActionRequest>, reason: &'static str) {
    if let Some(request) = deferred.take() {
        let _ = request.reply.send(Err(reason));
    }
}

fn stop(bot: &Client) {
    bot.force_stop_pathfinding();
    bot.walk(WalkDirection::None);
    let _ = bot.set_jumping(false);
    let _ = bot.set_crouching(false);
}

fn execute(bot: &Client, candidate: &Candidate, accepted_before: Instant) -> Result<(), ()> {
    if Instant::now() >= accepted_before {
        return Err(());
    }
    if !(1..=10_000).contains(&candidate.duration_ms) {
        return Err(());
    }
    if bot.health().map_err(|_| ())? <= 0.0 {
        return Err(());
    }
    let Some(target) = &candidate.target else {
        return if matches!(candidate.id.as_str(), "wait" | "stop") {
            Ok(())
        } else {
            Err(())
        };
    };
    if ![target.x, target.y, target.z]
        .iter()
        .all(|v| v.is_finite() && v.abs() < 30_000_000.0)
    {
        return Err(());
    }
    let position = bot.position().map_err(|_| ())?;
    let target_vec = Vec3::new(target.x, target.y, target.z);
    if position.distance_to(target_vec) > 12.0 {
        return Err(());
    }
    let goal = BlockPos::from(target_vec);
    // Reject unloaded endpoints. Azalea handles collisions against loaded blocks.
    let world = bot.world().map_err(|_| ())?;
    if !standable(&world.read(), goal) {
        return Err(());
    }
    if Instant::now() >= accepted_before {
        return Err(());
    }
    bot.start_goto_with_opts(
        BlockPosGoal(goal),
        PathfinderOpts::new()
            .allow_mining(false)
            .retry_on_no_path(false)
            .max_timeout(Duration::from_millis(100)),
    );
    Ok(())
}

// Require a full collision cube and Azalea's support hazard checks. This
// validates the loaded endpoint, not every cell along a path.
fn standable(world: &azalea::world::World, feet: BlockPos) -> bool {
    let Some(foot_block) = world.get_block_state(feet) else {
        return false;
    };
    let Some(head_block) = world.get_block_state(BlockPos::new(feet.x, feet.y + 1, feet.z)) else {
        return false;
    };
    let Some(support) = world.get_block_state(BlockPos::new(feet.x, feet.y - 1, feet.z)) else {
        return false;
    };
    foot_block.is_air() && head_block.is_air() && supports_endpoint(support)
}

fn supports_endpoint(support: azalea::block::BlockState) -> bool {
    support.is_collision_shape_full() && azalea::pathfinder::world::is_block_state_solid(support)
}

fn observe(bot: &Client, sequence: u64, world_epoch: u64, note: &str) -> Option<Observation> {
    let dimension = bot.world_name().ok()?.0.to_string();
    let position = bot.position().ok()?;
    let health = bot.health().ok()?;
    let food = bot.hunger().ok()?.food as f32;
    if ![position.x, position.y, position.z]
        .iter()
        .all(|v| v.is_finite())
        || !health.is_finite()
    {
        return None;
    }
    let mut missing = Vec::new();
    let inventory = match bot
        .get_inventory()
        .ok()
        .and_then(|inventory| inventory.slots())
    {
        Some(slots) => slots
            .iter()
            .enumerate()
            .filter(|(_, item)| !item.is_empty())
            .map(|(slot, item)| format!("slot {slot}: {:?} x{}", item.kind(), item.count()))
            .collect(),
        None => {
            missing.push("inventory unavailable");
            Vec::new()
        }
    };
    let mut blocks = Vec::new();
    if let Ok(world) = bot.world() {
        let world = world.read();
        let center = BlockPos::from(position);
        // A bounded sample, not a complete world map; only loaded non-air cells.
        for x in -3..=3 {
            for z in -3..=3 {
                for y in -1..=2 {
                    let point = BlockPos::new(center.x + x, center.y + y, center.z + z);
                    if let Some(block) = world.get_block_state(point)
                        && !block.is_air()
                    {
                        blocks.push(Landmark {
                            name: block.to_trait().id().into(),
                            position: Position {
                                x: point.x as f64,
                                y: point.y as f64,
                                z: point.z as f64,
                            },
                        });
                    }
                }
            }
        }
        for x in -3..=3 {
            for z in -3..=3 {
                for y in -1..=1 {
                    let feet = BlockPos::new(center.x + x, center.y + y, center.z + z);
                    if (x != 0 || z != 0) && standable(&world, feet) {
                        blocks.push(Landmark {
                            name: format!("waypoint:{}:{}:{}", feet.x, feet.y, feet.z),
                            position: Position {
                                x: feet.x as f64 + 0.5,
                                y: feet.y as f64,
                                z: feet.z as f64 + 0.5,
                            },
                        });
                    }
                }
            }
        }
    } else {
        missing.push("blocks unavailable");
    }
    let mut entities = Vec::new();
    if let Ok(nearby) = bot.nearest_entities::<()>() {
        for entity in nearby.iter().take(32) {
            if entity.id() == bot.entity {
                continue;
            }
            if let (Ok(point), Ok(kind)) = (entity.position(), entity.kind())
                && point.distance_to(position) <= 16.0
            {
                entities.push(Landmark {
                    name: format!("{kind:?}"),
                    position: Position {
                        x: point.x,
                        y: point.y,
                        z: point.z,
                    },
                });
            }
        }
    } else {
        missing.push("entities unavailable");
    }
    Some(Observation {
        world_epoch,
        dimension: Some(dimension),
        // The caller stamps the adapter's death count; `observe` only reads the client.
        deaths: 0,
        sequence,
        connected: true,
        position: Position {
            x: position.x,
            y: position.y,
            z: position.z,
        },
        health,
        food,
        inventory,
        blocks,
        entities,
        note: format!(
            "{note} Blocks sampled within 3 cells; entities capped at 32 within 16 blocks. {}",
            missing.join("; ")
        ),
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn full_hub_blocks_support_endpoints_but_partial_blocks_and_magma_do_not() {
        use azalea::registry::builtin::BlockKind;

        for kind in [BlockKind::PackedMud, BlockKind::BrownMushroomBlock] {
            assert!(super::supports_endpoint(kind.into()));
        }
        for kind in [
            BlockKind::JungleStairs,
            BlockKind::MagmaBlock,
            BlockKind::Air,
        ] {
            assert!(!super::supports_endpoint(kind.into()));
        }
    }

    use super::*;
    use std::net::TcpListener;

    #[tokio::test]
    async fn closed_port_finishes_without_localset_panic() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let mut adapter = spawn(Settings {
            port,
            ..Settings::default()
        });
        tokio::time::timeout(Duration::from_secs(5), &mut adapter.task)
            .await
            .expect("closed port must finish promptly")
            .expect("Azalea must run within its LocalSet without panicking");
        assert!(adapter.errors.borrow().is_some());
        assert!(adapter.observations.borrow().is_none());
    }

    #[tokio::test]
    async fn disconnect_interrupts_server_that_never_logs_in() {
        // The bound listener permits TCP establishment but never replies to
        // Minecraft packets, exercising cancellation during login.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut adapter = spawn(Settings {
            port: listener.local_addr().unwrap().port(),
            ..Settings::default()
        });
        let accepted = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            !adapter.task.is_finished(),
                            "adapter exited before connecting"
                        );
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    Err(error) => panic!("test listener failed: {error}"),
                }
            }
        })
        .await
        .expect("adapter must establish TCP connection");
        adapter.commands.send(AdapterCommand::Disconnect).unwrap();
        tokio::time::timeout(Duration::from_secs(2), &mut adapter.task)
            .await
            .expect("Disconnect must interrupt pending login")
            .expect("adapter must exit cleanly");
        drop(accepted);
    }
}
