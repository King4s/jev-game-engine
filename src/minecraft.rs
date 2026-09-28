//! Minecraft 26.2 adapter for the pinned Azalea revision. Local server only.
use std::{
    collections::BTreeMap,
    net::{Ipv4Addr, SocketAddr},
    time::{Duration, Instant},
};

use azalea::{
    BlockPos, Client, EntityRef, Event, StartClientOpts, Vec3, WalkDirection,
    account::Account,
    core::position::ChunkPos,
    core::{data_registry::DataRegistryWithKey, registry_holder::RegistryHolder},
    entity::inventory::Inventory,
    pathfinder::{PathfinderClientExt, PathfinderOpts, goals::BlockPosGoal},
    protocol::{
        address::ResolvedAddr,
        packets::game::{ClientboundGamePacket, ClientboundSetTime},
    },
    registry::{builtin::BlockKind, data::WorldClockKey},
};
use tokio::sync::{mpsc, watch};

use crate::{
    adapter::{ActionRequest, AdapterCommand, AdapterHandle, execute_and_acknowledge},
    engine::REJECTED_AFTER_HIT,
    model::{
        Candidate, Landmark, Observation, Position, RecoverableDrop, Settings, SkillOutcome,
        WoodSkill,
    },
    refusal::{self, LocalExecutor, Refusal},
    resources::{animal_in_range, classify_block, classify_entity, nearest_resources},
};

/// Ticks in one Minecraft day cycle.
const DAY_TICKS: u64 = 24_000;

#[path = "minecraft_crafting.rs"]
mod crafting;
use crafting::{Craft, ServerInventory};
#[path = "minecraft_placement.rs"]
mod placement;
use placement::Place;

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
                Some(AdapterCommand::Execute(request)) => { let _ = request.reply.send(Err(Refusal::StillConnecting)); },
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
    let mut gather: Option<Gather> = None;
    let mut orphaned: Option<OrphanDrop> = None;
    let mut craft: Option<Craft> = None;
    let mut place: Option<Place> = None;
    let mut server_inventory: Option<ServerInventory> = None;
    let mut skill_outcome: Option<SkillOutcome> = None;
    let mut last_snapshot = Instant::now();
    let mut last_tick = Instant::now();
    let mut connected_at = Instant::now();
    let mut world_epoch = 0_u64;
    // Tree history can veto a fresh route, but never supplies action geometry.
    let mut tree_memory = NativeTreeMemory::default();
    let mut deferred: Option<Box<ActionRequest>> = None;
    let mut deferred_ready = false;
    // Whether this connection has published an observation of a world it spawned into. A
    // later login is then a transfer inside an observable world, not a fresh connection.
    let mut observed = false;
    let mut last_health: Option<f32> = None;
    // Monotonic per connection; stamped on every observation so the engine sees a death
    // even when the respawn observation replaces the dying one.
    let mut deaths = 0_u64;
    // A flight goal keeps running when the bot is hit: stopping under fire is what let
    // skeleton arrows land every shot in the first live night run.
    let mut fleeing = false;
    // Azalea's own SetTime handler discards the packet, so the adapter keeps the last
    // day time it saw; `None` until the server has sent one.
    let mut time_of_day: Option<u64> = None;
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
                if stale { reject_deferred(&mut deferred, Refusal::StaleGameTicks); }
                if deadline.is_some_and(|d| Instant::now() >= d) || (deadline.is_some() && stale) {
                    cancel_skills(bot, &mut gather, &mut craft, &mut place, &mut skill_outcome, &mut orphaned, "Gather cancelled by local guard or operator");
                    deadline = None;
                    note = "Local watchdog stopped movement: action expired or game ticks became stale.".into();
                }
                if ready && last_snapshot.elapsed() >= Duration::from_millis(250) {
                    last_snapshot = Instant::now();
                    if let Some(mut observation) = observe(bot, sequence + 1, world_epoch, time_of_day, &note, &mut tree_memory) {
                        sequence += 1;
                        observation.deaths = deaths;
                        observation.crafting_inventory = server_inventory.as_ref().and_then(|s| s.idle_view(bot)).filter(|_| crafting::craft_stance(bot));
                        observation.skill_outcome = skill_outcome.clone();
                        let hurt = last_health.is_some_and(|old| observation.health < old);
                        if observation.health > 0.0 && hurt && fleeing && deadline.is_some() {
                            note = "Damage during a flight goal; movement continues.".into();
                        } else if observation.health <= 0.0 || hurt {
                            reject_deferred(&mut deferred, REJECTED_AFTER_HIT);
                            cancel_skills(bot, &mut gather, &mut craft, &mut place, &mut skill_outcome, &mut orphaned, "Gather cancelled by local guard or operator");
                            deadline = None;
                            fleeing = false;
                            note = "Local health guard stopped movement after damage.".into();
                        }
                        last_health = Some(observation.health);
                        if stale { observation.note.push_str(" Game ticks stale; local movement stopped."); }
                        observation.skill_outcome = skill_outcome.clone();
                        observation.recoverable_drop = orphaned.as_ref().and_then(|o| recovery_opportunity(bot, o, world_epoch));
                        observed = true;
                        observations.send_replace(Some(observation));
                    } else {
                        reject_deferred(&mut deferred, Refusal::ObservationUnavailable);
                        cancel_skills(bot, &mut gather, &mut craft, &mut place, &mut skill_outcome, &mut orphaned, "Gather cancelled by local guard or operator");
                        deadline = None;
                    }
                }
            },
            command = commands.recv() => match command {
                None | Some(AdapterCommand::Disconnect) => break,
                Some(AdapterCommand::Stop) => {
                    reject_deferred(&mut deferred, Refusal::OperatorStop);
                    cancel_skills(bot, &mut gather, &mut craft, &mut place, &mut skill_outcome, &mut orphaned, "Gather cancelled by local guard or operator");
                    deadline = None;
                    note = "Local operator stop; pathfinding and movement cancelled.".into();
                }
                Some(AdapterCommand::Execute(request)) => {
                    cancel_skills(bot, &mut gather, &mut craft, &mut place, &mut skill_outcome, &mut orphaned, "Gather cancelled by local guard or operator");
                    deadline = None;
                    reject_deferred(&mut deferred, Refusal::SupersededByNewerCommand);
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
                    server_inventory = None;
                    reject_deferred(&mut deferred, Refusal::LifecycleChangedBeforeAcceptance);
                    cancel_skills(bot, &mut gather, &mut craft, &mut place, &mut skill_outcome, &mut orphaned, "Gather cancelled by local guard or operator");
                    deadline = None;
                    world_epoch = world_epoch.wrapping_add(1);
                    orphaned = None;
                    ready = true;
                    last_health = None;
                    last_tick = Instant::now();
                    fleeing = false;
                    note = "World spawned; prior movement cancelled.".into();
                    if let Some(mut observation) = observe(bot, sequence + 1, world_epoch, time_of_day, &note, &mut tree_memory) {
                        sequence += 1;
                        observation.deaths = deaths;
                        observation.crafting_inventory = server_inventory.as_ref().and_then(|s| s.idle_view(bot)).filter(|_| crafting::craft_stance(bot));
                        observation.skill_outcome = skill_outcome.clone();
                        observed = true;
                        observations.send_replace(Some(observation));
                    }
                }
                Some(Event::Death(_)) => {
                    deaths = deaths.wrapping_add(1);
                    reject_deferred(&mut deferred, Refusal::DeathBeforeAcceptance);
                    cancel_skills(bot, &mut gather, &mut craft, &mut place, &mut skill_outcome, &mut orphaned, "Gather cancelled by local guard or operator");
                    orphaned = None;
                    deadline = None;
                    fleeing = false;
                    note = "Bot died; movement stopped.".into();
                }
                Some(Event::Packet(packet)) => {
                    ServerInventory::receive(&mut server_inventory, &packet, bot);
                    if let Some(active) = place.as_mut() { active.receive(&packet); }
                    if let ClientboundGamePacket::SetPlayerInventory(packet) = &*packet {
                        apply_player_inventory(bot, packet);
                    }
                    if let Some(active) = gather.as_mut() {
                        match &*packet {
                            ClientboundGamePacket::BlockUpdate(update) if update.pos == active.target => {
                                active.server_removed = crate::perception::is_air(update.block_state);
                            }
                            ClientboundGamePacket::SectionBlocksUpdate(update) => {
                                for change in &update.states {
                                    let pos = BlockPos::new(update.section_pos.x * 16 + i32::from(change.pos.x), update.section_pos.y * 16 + i32::from(change.pos.y), update.section_pos.z * 16 + i32::from(change.pos.z));
                                    if pos == active.target { active.server_removed = crate::perception::is_air(change.state); }
                                }
                            }
                            _ => {}
                        }
                    }
                    if let ClientboundGamePacket::SetTime(packet) = &*packet
                        && let Ok(world) = bot.world()
                        && let Some(ticks) = day_time(packet, &world.read().registries)
                    {
                        time_of_day = Some(ticks);
                    }
                }
                Some(Event::Login) => {
                    server_inventory = None;
                    reject_deferred(&mut deferred, Refusal::LoginInterrupted);
                    cancel_skills(bot, &mut gather, &mut craft, &mut place, &mut skill_outcome, &mut orphaned, "Gather cancelled by local guard or operator");
                    orphaned = None;
                    deadline = None;
                    let effect = login_effect(observed);
                    if effect.awaits_spawn {
                        // The first login of a connection: no world is observable yet, so
                        // publication waits for the spawn instead of reporting a position
                        // that is not in a loaded world.
                        ready = false;
                        connected_at = Instant::now();
                    } else {
                        // A transfer inside an observable world: the epoch advances, so every
                        // action bound to the world the bot left is refused, and publication
                        // continues. See `login_effect`.
                        world_epoch = world_epoch.wrapping_add(1);
                        last_health = None;
                        note = "World switched; prior movement cancelled. Observations continue in the new world.".into();
                    }
                }
                Some(Event::Tick) => {
                    last_tick = Instant::now();
                    // Recheck every active phase before it may send another input.
                    // This uses current permitted observations, never unseen entities,
                    // and is independent of the optional engine flight reflex.
                    if gather.is_some() || craft.is_some() || place.is_some() {
                        // A nearby threat, thin food and a truncated entity scan are recorded
                        // as facts about this turn, never as an engine veto: whether to keep
                        // working while a mob is close is the model's decision, and each
                        // executor still cancels itself when health or the world changes.
                        if let Some(risk) = observe(bot, sequence + 1, world_epoch, time_of_day, &note, &mut tree_memory)
                            .and_then(|current| crate::survival::work_risk(&current))
                        {
                            note = format!("Skill in flight while risk observed: {risk}.");
                        }
                    }
                    if deadline.is_some_and(|d| Instant::now() < d) && let Some(active) = gather.as_mut() {
                        match active.advance(bot, world_epoch) {
                            Ok(false) => {},
                            result => {
                                if result.is_err() && active.server_removed && !active.recovery {
                                    orphaned = Some(OrphanDrop::from_gather(active));
                                }
                                skill_outcome = Some(active.outcome(result.is_ok(), result.err().unwrap_or("Observed server removal and gathered log inventory increase")));
                                gather = None;
                                stop(bot);
                                deadline = None;
                            }
                        }
                    }
                    if events.is_empty() && deadline.is_some_and(|d| Instant::now() < d)
                        && let Some(active) = craft.as_mut() {
                        match active.advance(bot, world_epoch, server_inventory.as_ref()) {
                            Ok(false) => {},
                            result => {
                                skill_outcome = Some(active.outcome(result.is_ok(), result.err().unwrap_or("Server confirmed recipe consumption and deposited output")));
                                craft = None;
                                stop(bot);
                                deadline = None;
                            }
                        }
                    }
                    if events.is_empty() && deadline.is_some_and(|d| Instant::now() < d)
                        && let Some(active) = place.as_mut() {
                        match active.advance(bot, world_epoch, server_inventory.as_ref()) {
                            Ok(false) => {},
                            result => {
                                skill_outcome = Some(active.outcome(result.is_ok(), result.err().unwrap_or("Server confirmed exact table placement and inventory consumption")));
                                place = None;
                                stop(bot);
                                deadline = None;
                            }
                        }
                    }
                    // The prior ECS update consumed stop before a new goto is enqueued.
                    if deferred_ready && events.is_empty()
                        && let Some(request) = deferred.take() {
                        let candidate = request.candidate.clone();
                        // The action is bound to the world it was observed in, and each check
                        // that can refuse it here is named: the alternative was one sentence
                        // that fit a world change, a missing spawn and a cancellation alike.
                        if let Some(refusal) = identity_refusal(
                            &request,
                            ready,
                            world_epoch,
                            bot.world_name().ok().map(|name| name.0.to_string()).as_deref(),
                        ) {
                            note = format!("Local guard rejected action: {refusal}");
                            let _ = request.reply.send(Err(refusal));
                        } else {
                        // No engine-side veto on the model's work choice: a nearby threat or
                        // low food is stated in the candidate the model answered, and each
                        // executor's own prepare/advance guards still refuse anything unsafe
                        // it can verify itself (health, stance, world identity, inventory).
                        let prepared = if matches!(candidate.skill, Some(WoodSkill::Gather { .. })) {
                            Gather::prepare(bot, &candidate, &settings, world_epoch, request.attempt_id, server_inventory.as_ref(), &mut tree_memory).map(Some)
                        } else if matches!(candidate.skill, Some(WoodSkill::RecoverDrop { .. })) {
                            Gather::prepare_recover(bot, &candidate, &settings, world_epoch, request.attempt_id, RecoveryInputs { server: server_inventory.as_ref(), orphan: orphaned.as_ref() }, &mut tree_memory).map(Some)
                        } else { Ok(None) };
                        let mut prepared = match prepared {
                            Ok(value) => value,
                            Err(()) => { let _ = request.reply.send(Err(Refusal::WoodSkillUnprepared)); continue; }
                        };
                        let mut prepared_craft = if matches!(candidate.skill, Some(WoodSkill::CraftPlanks { .. } | WoodSkill::CraftTable { .. })) {
                            match server_inventory.as_ref().ok_or(()).and_then(|server|
                                Craft::prepare(bot, &candidate, &settings, world_epoch, request.attempt_id, server)) {
                                Ok(value) => Some(value),
                                Err(()) => { let _ = request.reply.send(Err(Refusal::CraftingUnprepared)); continue; }
                            }
                        } else { None };
                        let mut prepared_place = if matches!(candidate.skill, Some(WoodSkill::PlaceTable { .. })) {
                            match server_inventory.as_ref().ok_or(()).and_then(|server|
                                Place::prepare(bot, &candidate, &settings, world_epoch, request.attempt_id, server)) {
                                Ok(value) => Some(value),
                                Err(()) => { let _ = request.reply.send(Err(Refusal::PlacementUnprepared)); continue; }
                            }
                        } else { None };
                        match execute_and_acknowledge(request.reply, request.accepted_before,
                            || {
                                if prepared.is_some() || prepared_craft.is_some() || prepared_place.is_some() {
                                    Ok(())
                                } else {
                                    execute(bot, &candidate, request.accepted_before)
                                }
                            },
                            || stop(bot)) {
                            Ok(_) => {
                                gather = prepared.take();
                                if matches!(candidate.skill, Some(WoodSkill::RecoverDrop { .. })) { orphaned = None; }
                                craft = prepared_craft.take();
                                place = prepared_place.take();
                                skill_outcome = None;
                                deadline = Some(Instant::now() + Duration::from_millis(candidate.duration_ms));
                                fleeing = candidate.id.starts_with("flee_");
                                note = "Adapter accepted bounded action; local Azalea pathfinding, not proof of movement.".into();
                            }
                            Err(refusal) => {
                                deadline = None;
                                note = format!("Local guard rejected action: {refusal}");
                            }
                        }
                        }
                    }
                }
                _ => {},
            }
        }
    }
    cancel_skills(
        bot,
        &mut gather,
        &mut craft,
        &mut place,
        &mut skill_outcome,
        &mut orphaned,
        "Gather cancelled by disconnect",
    );
    observations.send_modify(|value| {
        if let Some(observation) = value {
            observation.sequence += 1;
            observation.connected = false;
            observation.skill_outcome = skill_outcome.clone();
            observation.note = "Disconnected; values are the last observed server state.".into();
        }
    });
}

/// What a login packet does to the live adapter's world lifecycle.
///
/// azalea sends `Login` for every login packet the server sends, and switching worlds is one
/// of them, while `Spawn` is re-sent only once the new world's chunks have been received. The
/// first login of a connection is therefore the only one with nothing to observe yet, and it
/// waits for that spawn. Every later login arrives while the adapter is already publishing,
/// which makes it a transfer: the world lifecycle epoch advances so every action bound to the
/// world the bot left is refused, and publication continues instead of waiting for a spawn a
/// world the bot was dropped into may never send. Blanking publication for a transfer is what
/// left the engine without fresh observations after the live run's dimension change, and the
/// engine then ended the session as provider_failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LoginEffect {
    awaits_spawn: bool,
    transfers_world: bool,
}

fn login_effect(observed: bool) -> LoginEffect {
    LoginEffect {
        awaits_spawn: !observed,
        transfers_world: observed,
    }
}

fn reject_deferred(deferred: &mut Option<Box<ActionRequest>>, reason: Refusal) {
    if let Some(request) = deferred.take() {
        let _ = request.reply.send(Err(reason));
    }
}

fn cancel_skills(
    bot: &Client,
    active: &mut Option<Gather>,
    craft: &mut Option<Craft>,
    place: &mut Option<Place>,
    outcome: &mut Option<SkillOutcome>,
    orphaned: &mut Option<OrphanDrop>,
    reason: &str,
) {
    if let Some(active) = active.take() {
        if active.server_removed && !active.recovery {
            *orphaned = Some(OrphanDrop::from_gather(&active));
        }
        *outcome = Some(active.outcome(false, reason));
    }
    if let Some(active) = craft.take() {
        *outcome = Some(active.outcome(
            false,
            "Craft cancelled; partial ingredients may remain pending server reconciliation",
        ));
    }
    if let Some(active) = place.take() {
        *outcome = Some(active.outcome(false, reason));
    }
    stop(bot);
}

// The pinned 26.2 client leaves SetPlayerInventory unimplemented. Main inventory
// slot numbers are 0..8 hotbar, 9..35 storage; menu slots put the hotbar last.
// Copy only the server packet's owned slot, never infer item gains from actions.
fn apply_player_inventory(
    bot: &Client,
    packet: &azalea::protocol::packets::game::ClientboundSetPlayerInventory,
) {
    let index = match packet.slot {
        0..=8 => packet.slot as usize + 27,
        9..=35 => packet.slot as usize - 9,
        _ => return,
    };
    let mut ecs = bot.ecs.write();
    if let Some(mut inventory) = ecs.get_mut::<Inventory>(bot.entity) {
        inventory.inventory_menu.as_player_mut().inventory[index] = packet.contents.clone();
        if let Some(menu) = inventory.container_menu.as_mut() {
            let start = *menu.player_slots_range().start();
            if let Some(slot) = menu.slot_mut(start + index) {
                *slot = packet.contents.clone();
            }
        }
    }
}

/// Queue one break of `target` in the pinned client.
///
/// Direct component insertion permits synchronous cancellation before the next game tick,
/// and only a whitelisted target is ever queued: `LeftClickMine` is never used because it can
/// retarget arbitrary blocks. Never call this after [`stop`], which removes the queue.
fn queue_mining(bot: &Client, target: BlockPos) {
    bot.ecs
        .write()
        .entity_mut(bot.entity)
        .insert(azalea::mining::MiningQueued {
            position: target,
            direction: azalea::core::direction::Direction::Down,
            force: true,
        });
}

/// Whether the local client is mining `target` itself.
///
/// This is Azalea's own state, not an inference from the world: in creative the pinned client
/// breaks through a local prediction and never inserts `Mining`, in survival it inserts this
/// component and waits for the server. Either way a request that left the block unchanged and
/// left nothing in flight was consumed without starting a break.
fn mining_in_flight(bot: &Client, target: BlockPos) -> bool {
    bot.ecs
        .read()
        .get::<azalea::mining::Mining>(bot.entity)
        .is_some_and(|mining| mining.pos == target)
}

fn stop(bot: &Client) {
    // Remove queued mining synchronously: StopMiningBlockEvent alone does not remove
    // MiningQueued in this pinned client. No worker survives a local cancellation.
    let mut ecs = bot.ecs.write();
    let mining = ecs.get::<azalea::mining::Mining>(bot.entity).is_some();
    if mining
        && ecs
            .get::<azalea::mining::MineBlockPos>(bot.entity)
            .is_some_and(|pos| pos.0.is_some())
    {
        ecs.write_message(azalea::mining::StopMiningBlockEvent { entity: bot.entity });
    }
    if let Ok(mut entity) = ecs.get_entity_mut(bot.entity) {
        entity.remove::<azalea::mining::MiningQueued>();
        entity.remove::<azalea::mining::Mining>();
        entity.remove::<azalea::mining::LeftClickMine>();
    }
    drop(ecs);
    bot.force_stop_pathfinding();
    bot.walk(WalkDirection::None);
    let _ = bot.set_jumping(false);
    let _ = bot.set_crouching(false);
}

enum GatherPhase {
    Approach,
    Mining,
    Collect,
    Return,
}

/// Old mining coordinates only limit the search area. Each recovery action is
/// constructed from a new visible item and a new reversible route.
struct OrphanDrop {
    log: String,
    source: Vec3,
    epoch: u64,
    expires: Instant,
}

impl OrphanDrop {
    fn from_gather(gather: &Gather) -> Self {
        Self {
            log: crate::wood::drop_for_block(&gather.log).unwrap_or_else(|| gather.log.clone()),
            source: Vec3::new(
                gather.target.x as f64 + 0.5,
                gather.target.y as f64 + 0.5,
                gather.target.z as f64 + 0.5,
            ),
            epoch: gather.epoch,
            expires: Instant::now() + Duration::from_secs(15),
        }
    }
}

fn recovery_opportunity(bot: &Client, orphan: &OrphanDrop, epoch: u64) -> Option<RecoverableDrop> {
    if epoch != orphan.epoch || Instant::now() >= orphan.expires {
        return None;
    }
    let position = bot.position().ok()?;
    let eye = bot.eye_position().ok()?;
    let look = *bot.component::<azalea::entity::LookDirection>().ok()?;
    let (nearby, incomplete) = bounded_nearby_entities(bot, position, 256)?;
    if incomplete {
        return None;
    }
    let matching_drops = nearby
        .iter()
        .take(128)
        .filter_map(|entity| {
            let item = entity
                .component::<azalea::entity::metadata::ItemItem>()
                .ok()?;
            if item.0.kind().to_str() != orphan.log {
                return None;
            }
            drop(item);
            entity.position().ok()
        })
        .collect::<Vec<_>>();
    // Planning frames use the same bounded look-around as the observation: a drop and
    // its route can lie outside the direction the bot happened to face.
    let views = crate::perception::Views::new(eye, look.y_rot(), look.x_rot()).with_budget(4096);
    let drop_views = crate::perception::Views::new(eye, look.y_rot(), look.x_rot())
        .with_budget(crate::perception::ENTITY_CELL_BUDGET);
    let world = bot.world().ok()?;
    let world = world.read();
    let occupied = BlockPos::from(position);
    let (visible, _) = views.scan(occupied, |p| world.get_block_state(p));
    matching_drops
        .into_iter()
        .filter_map(|point| {
            if point.distance_to(orphan.source) > 3.0
                || point.distance_to(position) > 12.0
                || !drop_views.visible(point, false, |cell| world.get_block_state(cell))
            {
                return None;
            }
            let route = crate::wood_tree::pickup_route(
                &Position {
                    x: position.x,
                    y: position.y,
                    z: position.z,
                },
                &Position {
                    x: point.x,
                    y: point.y,
                    z: point.z,
                },
                |(x, y, z)| {
                    let cell = BlockPos::new(x, y, z);
                    let contact = x == occupied.x
                        && z == occupied.z
                        && (occupied.y - 1..=occupied.y + 1).contains(&y);
                    let block = if contact {
                        world.get_block_state(cell)
                    } else {
                        visible.get(cell)
                    }?;
                    Some(crate::wood_tree::Terrain {
                        air: crate::perception::is_air(block),
                        support: supports_endpoint(block),
                        log: None,
                    })
                },
            )?;
            Some(RecoverableDrop {
                log: orphan.log.clone(),
                position: Position {
                    x: point.x,
                    y: point.y,
                    z: point.z,
                },
                route,
            })
        })
        .next()
}

#[derive(Debug, PartialEq, Eq)]
enum MiningDecision {
    /// The local client is mining this target; the action deadline bounds the wait.
    Continue,
    /// A local prediction is in flight; wait for the authoritative block update.
    WaitForServer,
    /// Server removal is visible; collect the drop.
    Collect,
    /// The block is unchanged and nothing is mining it: the queued request was consumed
    /// without starting a break, so ask again instead of idling out the deadline.
    Reask,
    /// The block is unchanged and nothing is mining it, but this tick's stance or sightline
    /// is not verified. Turn toward the target and re-read: turning is an observation, not
    /// permission to ask, and a transient wobble is not a changed target.
    TurnToTarget,
}

/// How many times one action may ask the local client to break its target, and how many
/// client ticks must pass between asks.
///
/// One adapter tick is one client game tick, whose length the protocol fixes at 50 ms, so this
/// budget is measured in ticks and never in wall-clock time or in one machine's packets per
/// second. A client that accepted the request answers within a tick or two: the pinned client
/// either predicts the block away or inserts its own mining component in the same tick it
/// consumes `MiningQueued` (see `azalea-client/src/plugins/mining.rs` `handle_mining_queued`).
/// A whole second of client ticks with no answer is evidence the client is not going to mine
/// this block, and three such asks is the bounded retry budget of one action.
const MINING_ANSWER_TICKS: u64 = 20;
const MINING_ASK_BUDGET: u32 = 3;

/// The executor's own ask schedule, in client ticks. Pure, so the deadline behaviour is
/// pinned without wall-clock time.
#[derive(Clone, Copy, Debug, Default)]
struct MiningAsks {
    asked: u32,
    ticks: u64,
}

impl MiningAsks {
    fn tick(&mut self) {
        self.ticks = self.ticks.saturating_add(1);
    }
    /// Whether the client has had long enough to answer the previous ask.
    fn due(&self) -> bool {
        self.asked == 0 || self.ticks >= MINING_ANSWER_TICKS
    }
    fn budget_spent(&self) -> bool {
        self.asked >= MINING_ASK_BUDGET
    }
    fn ask(&mut self) {
        self.asked += 1;
        self.ticks = 0;
    }
}

fn mining_decision(
    air: bool,
    expected_log: bool,
    server_removed: bool,
    mining_in_flight: bool,
    guard: bool,
) -> Result<MiningDecision, &'static str> {
    if air {
        // Azalea may predict air before the authoritative block update arrives. Sight cannot
        // find a log in air, so wait within the action deadline. A stance or look that drifts
        // after the request is in flight is not a reason to cancel: the break is already out.
        if server_removed {
            Ok(MiningDecision::Collect)
        } else {
            Ok(MiningDecision::WaitForServer)
        }
    } else if !expected_log {
        Err("Gather target replaced during mining")
    } else if mining_in_flight {
        Ok(MiningDecision::Continue)
    } else if guard {
        Ok(MiningDecision::Reask)
    } else {
        Ok(MiningDecision::TurnToTarget)
    }
}

struct Gather {
    recovery: bool,
    last_health: f32,
    attempt_id: u64,
    id: String,
    epoch: u64,
    dimension: String,
    /// The harvestable block being mined, whatever it is.
    log: String,
    /// The item that block really drops; every inventory confirmation compares this,
    /// never the block name (stone confirms cobblestone, grass block confirms dirt).
    drop: String,
    target: BlockPos,
    route: Vec<Vec3>,
    route_index: usize,
    baseline: u32,
    phase: GatherPhase,
    /// The break requests this action has made, and the client ticks since the last one.
    asks: MiningAsks,
    server_removed: bool,
    pickup: Vec<Vec3>,
    pickup_index: usize,
    retreat: Vec<Vec3>,
    retreat_index: usize,
    collection_started: Option<Instant>,
}

struct RecoveryInputs<'a> {
    server: Option<&'a ServerInventory>,
    orphan: Option<&'a OrphanDrop>,
}

impl Gather {
    fn prepare_recover(
        bot: &Client,
        candidate: &Candidate,
        settings: &Settings,
        epoch: u64,
        attempt_id: u64,
        inputs: RecoveryInputs<'_>,
        tree_memory: &mut NativeTreeMemory,
    ) -> Result<Self, ()> {
        let Some(WoodSkill::RecoverDrop { log, route }) = &candidate.skill else {
            return Err(());
        };
        let orphan = inputs.orphan.ok_or(())?;
        if orphan.log != *log
            || orphan.epoch != epoch
            || Instant::now() >= orphan.expires
            || bot.world_name().map_err(|_| ())?.0.to_string() != settings.allowed_dimension
        {
            return Err(());
        }
        let health = bot.health().map_err(|_| ())?;
        if !health.is_finite() || health <= 0.0 {
            return Err(());
        }
        let mut snapshot = observe(bot, 0, epoch, None, "", tree_memory).ok_or(())?;
        snapshot.crafting_inventory = inputs.server.and_then(|s| s.idle_view(bot));
        snapshot.recoverable_drop = recovery_opportunity(bot, orphan, epoch);
        if !crate::wood::recover_is_current(candidate, &snapshot, settings) {
            return Err(());
        }
        let drop = snapshot.recoverable_drop.ok_or(())?;
        if drop.route != *route || Some(&drop.position) != candidate.target.as_ref() {
            return Err(());
        }
        let inventory = bot.component::<Inventory>().map_err(|_| ())?;
        if inventory.container_menu.is_some() {
            return Err(());
        }
        let baseline = inventory_totals(&inventory).get(log).copied().unwrap_or(0);
        Ok(Self {
            recovery: true,
            last_health: health,
            attempt_id,
            id: candidate.id.clone(),
            epoch,
            dimension: settings.allowed_dimension.clone(),
            log: log.clone(),
            drop: log.clone(),
            target: BlockPos::from(Vec3::new(drop.position.x, drop.position.y, drop.position.z)),
            route: Vec::new(),
            route_index: 0,
            baseline,
            phase: GatherPhase::Collect,
            asks: MiningAsks::default(),
            server_removed: true,
            pickup: route.iter().map(|p| Vec3::new(p.x, p.y, p.z)).collect(),
            pickup_index: 0,
            retreat: Vec::new(),
            retreat_index: 0,
            collection_started: Some(Instant::now()),
        })
    }

    fn prepare(
        bot: &Client,
        candidate: &Candidate,
        settings: &Settings,
        epoch: u64,
        attempt_id: u64,
        server: Option<&ServerInventory>,
        tree_memory: &mut NativeTreeMemory,
    ) -> Result<Self, ()> {
        let Some(WoodSkill::Gather {
            log,
            approach,
            route,
        }) = &candidate.skill
        else {
            return Err(());
        };
        let health = bot.health().map_err(|_| ())?;
        let target = candidate.target.as_ref().ok_or(())?;
        if !settings.wood_skills
            || !crate::wood::is_harvestable(log)
            || bot.world_name().map_err(|_| ())?.0.to_string() != settings.allowed_dimension
            || !(1..=10_000).contains(&candidate.duration_ms)
            || !health.is_finite()
            || health <= 0.0
            || ![
                target.x, target.y, target.z, approach.x, approach.y, approach.z,
            ]
            .iter()
            .all(|n| n.is_finite() && n.abs() < 30_000_000.0)
        {
            return Err(());
        }
        let target_vec = Vec3::new(target.x, target.y, target.z);
        let target = BlockPos::from(target_vec);
        let approach = Vec3::new(approach.x, approach.y, approach.z);
        if bot.position().map_err(|_| ())?.distance_to(target_vec) > 12.0
            || Vec3::new(approach.x, approach.y + 1.62, approach.z).distance_to(target_vec) > 3.0
        {
            return Err(());
        }
        let world = bot.world().map_err(|_| ())?;
        let world = world.read();
        if BlockKind::from(world.get_block_state(target).ok_or(())?).to_str() != log
            || !standable(&world, BlockPos::from(approach))
        {
            return Err(());
        }
        drop(world);
        let mut snapshot = observe(bot, 0, epoch, None, "", tree_memory).ok_or(())?;
        snapshot.crafting_inventory = server.and_then(|s| s.idle_view(bot));
        if !crate::wood::gather_is_current(candidate, &snapshot, settings) {
            return Err(());
        }
        let inventory = bot.component::<Inventory>().map_err(|_| ())?;
        if inventory.container_menu.is_some() {
            return Err(());
        }
        let drop = crate::wood::drop_for_block(log).ok_or(())?;
        let baseline = inventory_totals(&inventory)
            .get(&drop)
            .copied()
            .unwrap_or(0);
        Ok(Self {
            recovery: false,
            last_health: health,
            attempt_id,
            id: candidate.id.clone(),
            epoch,
            dimension: settings.allowed_dimension.clone(),
            log: log.clone(),
            drop,
            target,
            route: route.iter().map(|p| Vec3::new(p.x, p.y, p.z)).collect(),
            route_index: 0,
            baseline,
            phase: GatherPhase::Approach,
            asks: MiningAsks::default(),
            server_removed: false,
            pickup: Vec::new(),
            pickup_index: 0,
            retreat: Vec::new(),
            retreat_index: 0,
            collection_started: None,
        })
    }

    fn outcome(&self, success: bool, message: &str) -> SkillOutcome {
        SkillOutcome {
            attempt_id: self.attempt_id,
            candidate_id: self.id.clone(),
            world_epoch: self.epoch,
            success,
            confirmed_by_server: success,
            message: message.into(),
        }
    }

    fn advance(&mut self, bot: &Client, epoch: u64) -> Result<bool, &'static str> {
        let health = bot.health().map_err(|_| "Gather health unavailable")?;
        if epoch != self.epoch
            || bot.world_name().ok().map(|n| n.0.to_string()).as_deref()
                != Some(self.dimension.as_str())
            || !health.is_finite()
            || health <= 0.0
            || health < self.last_health
        {
            return Err("Gather cancelled: world or health changed");
        }
        self.last_health = health;
        let inventory = bot
            .component::<Inventory>()
            .map_err(|_| "Gather inventory unavailable")?;
        if inventory.container_menu.is_some() {
            return Err("Gather cancelled: inventory menu changed");
        }
        let count = inventory_totals(&inventory)
            .get(&self.drop)
            .copied()
            .unwrap_or(0);
        let has_space = inventory.menu().player_slots_range().any(|slot| {
            inventory
                .menu()
                .slot(slot)
                .is_some_and(|item| item.is_empty())
        });
        drop(inventory);
        let position = bot.position().map_err(|_| "Gather position unavailable")?;
        let grounded = bot
            .component::<azalea::entity::Physics>()
            .map_err(|_| "Gather physics unavailable")?
            .on_ground();
        let eye = bot.eye_position().map_err(|_| "Gather eye unavailable")?;
        let look = *bot
            .component::<azalea::entity::LookDirection>()
            .map_err(|_| "Gather look unavailable")?;
        let sight = crate::perception::Sight::new(eye, look.y_rot(), look.x_rot());
        let occupied = BlockPos::from(position);
        let world = bot.world().map_err(|_| "Gather world unavailable")?;
        let world = world.read();
        let block = world
            .get_block_state(self.target)
            .ok_or("Gather target unloaded")?;
        if self
            .route
            .iter()
            .any(|p| !standable(&world, BlockPos::from(*p)))
        {
            return Err("Gather cancelled: standing or return route changed");
        }
        for pair in self.route.windows(2) {
            if pair[0].y != pair[1].y {
                let top = pair[0].y.max(pair[1].y) as i32 + 2;
                if pair.iter().any(|p| {
                    !world
                        .get_block_state(BlockPos::new(p.x.floor() as i32, top, p.z.floor() as i32))
                        .is_some_and(crate::perception::is_air)
                }) {
                    return Err("Gather cancelled: jump clearance changed");
                }
            }
        }
        // `can_mine` checks cells before the target; require separate current
        // visibility of the exact log before a forced mining packet.
        let target_visible = sight
            .block(self.target, |p| world.get_block_state(p))
            .is_some_and(|seen| BlockKind::from(seen).to_str() == self.log);
        let stable_mining = crate::wood_tree::can_mine(
            &Position {
                x: position.x,
                y: position.y,
                z: position.z,
            },
            (self.target.x, self.target.y, self.target.z),
            grounded,
            |(x, y, z)| {
                let point = BlockPos::new(x, y, z);
                let contact = x == occupied.x
                    && z == occupied.z
                    && (occupied.y - 1..=occupied.y + 1).contains(&y);
                let block = if contact {
                    world.get_block_state(point)
                } else {
                    sight.block(point, |p| world.get_block_state(p))
                }?;
                Some(crate::wood_tree::Terrain {
                    air: crate::perception::is_air(block),
                    support: supports_endpoint(block),
                    log: None,
                })
            },
        );
        drop(world);
        let target_center = Vec3::new(
            self.target.x as f64 + 0.5,
            self.target.y as f64 + 0.5,
            self.target.z as f64 + 0.5,
        );
        if count <= self.baseline && !has_space {
            return Err("Gather blocked: no empty inventory space for drop");
        }
        match self.phase {
            GatherPhase::Approach => {
                if BlockKind::from(block).to_str() != self.log {
                    return Err("Gather target changed before mining");
                }
                if follow_wood_route(bot, &self.route, &mut self.route_index)? {
                    if !grounded {
                        return Ok(false);
                    }
                    if !stable_mining || !target_visible {
                        // Turning is a new observation, not permission to mine a
                        // target that was outside the previous view. Wait for the
                        // next tick's actual look direction and sightline.
                        bot.look_at(target_center);
                        return Ok(false);
                    }
                    stop(bot);
                    bot.look_at(target_center);
                    // Queue the exact whitelisted block, never LeftClickMine (which can
                    // retarget arbitrary blocks). Direct component insertion permits
                    // synchronous cancellation before the next game tick.
                    queue_mining(bot, self.target);
                    self.asks.ask();
                    self.phase = GatherPhase::Mining;
                }
            }
            GatherPhase::Mining => {
                self.asks.tick();
                let guard = stable_mining && target_visible;
                match mining_decision(
                    crate::perception::is_air(block),
                    BlockKind::from(block).to_str() == self.log,
                    self.server_removed,
                    mining_in_flight(bot, self.target),
                    guard,
                )? {
                    MiningDecision::Continue => {}
                    MiningDecision::WaitForServer => stop(bot),
                    MiningDecision::Collect => {
                        stop(bot);
                        self.phase = GatherPhase::Collect;
                        self.collection_started = Some(Instant::now());
                    }
                    MiningDecision::Reask => {
                        // The request left the block unchanged. Turn toward the target so the
                        // next tick's sightline is real, then ask again: the pinned client
                        // consumes a queued break silently when its own game mode or held item
                        // forbids the block, and a server that answers with a block-changed
                        // acknowledgement rolls a refused prediction back the same way. Neither
                        // is a reason to sit out the action deadline.
                        bot.look_at(target_center);
                        if self.asks.budget_spent() {
                            return Err(
                                "Gather break not started by the local client: the target is unchanged and no mining is in flight after the bounded retry budget",
                            );
                        }
                        if self.asks.due() {
                            queue_mining(bot, self.target);
                            self.asks.ask();
                        }
                    }
                    MiningDecision::TurnToTarget => {
                        // Turning is a new observation, not permission to mine a target that
                        // was outside the previous view. Wait for the next tick's actual look
                        // direction and sightline instead of ending a good attempt.
                        bot.look_at(target_center);
                    }
                }
            }
            GatherPhase::Collect => {
                if !crate::perception::is_air(block) {
                    return Err("Gather collection target changed");
                }
                if !self.pickup.is_empty()
                    && !follow_wood_route(bot, &self.pickup, &mut self.pickup_index)?
                {
                    return Ok(false);
                }
                if count > self.baseline {
                    self.retreat = self
                        .pickup
                        .iter()
                        .rev()
                        .chain(self.route.iter().rev())
                        .copied()
                        .collect();
                    self.retreat.dedup();
                    self.retreat_index = 0;
                    self.phase = GatherPhase::Return;
                    stop(bot);
                    return Ok(false);
                }
                let elapsed = self
                    .collection_started
                    .map_or(Duration::ZERO, |t| t.elapsed());
                if elapsed < Duration::from_millis(350) {
                    return Ok(false);
                }
                let (drops, incomplete) = bounded_nearby_entities(bot, position, 256)
                    .ok_or("Gather drop entities unavailable")?;
                if incomplete {
                    return Err("Gather drop scan incomplete; unseen drops unknown");
                }
                let eye = bot.eye_position().map_err(|_| "Gather sight unavailable")?;
                let look = *bot
                    .component::<azalea::entity::LookDirection>()
                    .map_err(|_| "Gather look unavailable")?;
                let drop_views = crate::perception::Views::new(eye, look.y_rot(), look.x_rot())
                    .with_budget(crate::perception::ENTITY_CELL_BUDGET);
                let nearby_drop = drops
                    .iter()
                    .take(128)
                    .filter_map(|entity| {
                        let item = entity
                            .component::<azalea::entity::metadata::ItemItem>()
                            .ok()?;
                        let matching = item.0.kind().to_str() == self.drop;
                        drop(item);
                        let p = entity.position().ok()?;
                        if !matching || p.distance_to(target_center) > 3.0 {
                            return None;
                        }
                        let world = bot.world().ok()?;
                        let world = world.read();
                        drop_views
                            .visible(p, false, |cell| world.get_block_state(cell))
                            .then_some(p)
                    })
                    .next();
                if let Some(drop_position) = nearby_drop {
                    let world = bot
                        .world()
                        .map_err(|_| "Gather collection world unavailable")?;
                    let world = world.read();
                    let occupied = BlockPos::from(position);
                    let views = crate::perception::Views::new(eye, look.y_rot(), look.x_rot());
                    let (visible, _) = views.scan(occupied, |p| world.get_block_state(p));
                    let route = crate::wood_tree::pickup_route(
                        &Position {
                            x: position.x,
                            y: position.y,
                            z: position.z,
                        },
                        &Position {
                            x: drop_position.x,
                            y: drop_position.y,
                            z: drop_position.z,
                        },
                        |(x, y, z)| {
                            let point = BlockPos::new(x, y, z);
                            // Current body/support contact is sensed locally. Every
                            // other route cell requires this frame's sight evidence.
                            let contact = x == occupied.x
                                && z == occupied.z
                                && (occupied.y - 1..=occupied.y + 1).contains(&y);
                            let b = if contact {
                                world.get_block_state(point)
                            } else {
                                visible.get(point)
                            }?;
                            Some(crate::wood_tree::Terrain {
                                air: crate::perception::is_air(b),
                                support: supports_endpoint(b),
                                log: None,
                            })
                        },
                    );
                    drop(world);
                    if let Some(route) = route {
                        if self.pickup.is_empty() {
                            self.pickup = route.iter().map(|p| Vec3::new(p.x, p.y, p.z)).collect();
                            self.pickup_index = 0;
                        } else {
                            self.pickup_index = self.pickup.len() - 1;
                            self.pickup
                                .extend(route.iter().skip(1).map(|p| Vec3::new(p.x, p.y, p.z)));
                        }
                        if elapsed > Duration::from_secs(3) {
                            return Err("Gather drop not collected within bounded pursuit");
                        }
                    } else if elapsed > Duration::from_secs(2) {
                        return Err("Gather drop blocked: no verified collection and return route");
                    }
                } else if elapsed > Duration::from_secs(2) {
                    return Err("Gather drop lost or not observed; inventory gain not confirmed");
                }
            }
            GatherPhase::Return => {
                if count <= self.baseline {
                    return Err("Gather collected item no longer owned during return");
                }
                if follow_wood_route(bot, &self.retreat, &mut self.retreat_index)? {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

// One mover. The walk/jump/reorient loop, the arrival test and the tolerance live in
// `motion`; this module supplies only the world-facing half (loaded-route revalidation
// and fresh bounded sight) and the world's own movement verbs. Waypoint navigation,
// harvest approach, collection, flight and placement therefore share one loop.
struct BotNav<'a> {
    bot: &'a Client,
}

fn as_position(p: Vec3) -> Position {
    Position {
        x: p.x,
        y: p.y,
        z: p.z,
    }
}

impl crate::motion::Nav for BotNav<'_> {
    fn position(&self) -> Result<Position, &'static str> {
        self.bot
            .position()
            .map(as_position)
            .map_err(|_| "Motion position unavailable")
    }

    fn grounded(&self) -> Result<bool, &'static str> {
        self.bot
            .component::<azalea::entity::Physics>()
            .map(|physics| physics.on_ground())
            .map_err(|_| "Motion physics unavailable")
    }

    fn verify_route(&mut self, route: &[Position]) -> Result<(), &'static str> {
        let world = self
            .bot
            .world()
            .map_err(|_| "Motion route world unavailable")?;
        let world = world.read();
        for p in route {
            if !standable(&world, BlockPos::from(Vec3::new(p.x, p.y, p.z))) {
                return Err("Motion route support/headroom changed");
            }
        }
        for pair in route.windows(2) {
            if pair[0].y != pair[1].y {
                let top = pair[0].y.max(pair[1].y) as i32 + 2;
                if pair.iter().any(|p| {
                    !world
                        .get_block_state(BlockPos::new(p.x.floor() as i32, top, p.z.floor() as i32))
                        .is_some_and(crate::perception::is_air)
                }) {
                    return Err("Motion jump/return clearance changed");
                }
            }
        }
        Ok(())
    }

    fn observed_step(
        &mut self,
        position: &Position,
        from: &Position,
        to: &Position,
        grounded: bool,
    ) -> Result<bool, &'static str> {
        observed_wood_step(
            self.bot,
            Vec3::new(position.x, position.y, position.z),
            Vec3::new(from.x, from.y, from.z),
            Vec3::new(to.x, to.y, to.z),
            grounded,
        )
    }

    fn step(&mut self, forward: bool, jump: bool) {
        self.bot.walk(if forward {
            WalkDirection::Forward
        } else {
            WalkDirection::None
        });
        let _ = self.bot.set_jumping(jump);
    }

    fn start_goal(&mut self, target: &Position) -> Result<(), &'static str> {
        let goal = BlockPos::from(Vec3::new(target.x, target.y, target.z));
        // Reject unloaded endpoints. Azalea handles collisions against loaded blocks.
        let world = self
            .bot
            .world()
            .map_err(|_| "Motion goal world unavailable")?;
        if !standable(&world.read(), goal) {
            return Err("Motion goal is not a standable observed cell");
        }
        self.bot.start_goto_with_opts(
            BlockPosGoal(goal),
            PathfinderOpts::new()
                .allow_mining(false)
                .retry_on_no_path(false)
                .max_timeout(Duration::from_millis(100)),
        );
        Ok(())
    }
}

/// Follow one verified route through the single mover. Returns `true` on arrival.
fn follow_wood_route(
    bot: &Client,
    route: &[Vec3],
    index: &mut usize,
) -> Result<bool, &'static str> {
    let route: Vec<Position> = route.iter().map(|p| as_position(*p)).collect();
    let mut nav = BotNav { bot };
    match crate::motion::follow(
        &mut nav,
        crate::motion::Plan::Route {
            route: &route,
            index,
        },
    )? {
        crate::motion::Progress::Arrived => Ok(true),
        crate::motion::Progress::Advancing => Ok(false),
    }
}

/// Cells touched by the player's 0.6-wide body on the current cardinal edge.
/// Jumping needs the extra ceiling above both columns, including an airborne
/// cancellation tick. This intentionally over-approximates the swept volume.
fn wood_swept_cells(position: Vec3, from: Vec3, target: Vec3) -> Vec<BlockPos> {
    let mut cells = Vec::new();
    let min_x = (position.x.min(from.x).min(target.x) - 0.3).floor() as i32;
    let max_x = (position.x.max(from.x).max(target.x) + 0.3).floor() as i32;
    let min_z = (position.z.min(from.z).min(target.z) - 0.3).floor() as i32;
    let max_z = (position.z.max(from.z).max(target.z) + 0.3).floor() as i32;
    let min_y = position.y.min(from.y).min(target.y).floor() as i32;
    let from_cell = BlockPos::from(from);
    let target_cell = BlockPos::from(target);
    let jumping = from.y != target.y || position.y > from.y.min(target.y) + 0.1;
    let top = position.y.max(from.y).max(target.y) + 1.8 + if jumping { 0.5 } else { 0.0 };
    let max_y = (top - 0.001).floor() as i32;
    for x in min_x..=max_x {
        for z in min_z..=max_z {
            // A raised landing block is support under the arrival feet, not
            // empty space through which the player must pass while ascending.
            let bottom = if x == target_cell.x && z == target_cell.z {
                min_y.max(target_cell.y)
            } else if x == from_cell.x && z == from_cell.z {
                min_y.max(from_cell.y)
            } else {
                min_y
            };
            for y in bottom..=max_y {
                cells.push(BlockPos::new(x, y, z));
            }
        }
    }
    cells
}

fn wood_transition_clear(
    position: Vec3,
    from: Vec3,
    target: Vec3,
    grounded: bool,
    mut read: impl FnMut(BlockPos) -> Option<azalea::block::BlockState>,
) -> bool {
    let feet = BlockPos::from(target);
    let support = BlockPos::new(feet.x, feet.y - 1, feet.z);
    let occupied = BlockPos::from(position);
    let current_support = BlockPos::new(occupied.x, occupied.y - 1, occupied.z);
    wood_swept_cells(position, from, target)
        .into_iter()
        .all(|cell| read(cell).is_some_and(crate::perception::clear_stance))
        && read(support).is_some_and(supports_endpoint)
        && (!grounded || read(current_support).is_some_and(supports_endpoint))
}

/// Fresh bounded sight plus local body contact. Unknown or occluded cells
/// pause a step; no distant chunk-cache state authorizes walking.
fn observed_wood_step(
    bot: &Client,
    position: Vec3,
    from: Vec3,
    target: Vec3,
    grounded: bool,
) -> Result<bool, &'static str> {
    let eye = bot
        .eye_position()
        .map_err(|_| "Wood step eye unavailable")?;
    let occupied = BlockPos::from(position);
    let world = bot.world().map_err(|_| "Wood step world unavailable")?;
    let mut read = |point: BlockPos| {
        let contact = point.x == occupied.x
            && point.z == occupied.z
            && (occupied.y - 1..=occupied.y + 1).contains(&point.y);
        if contact {
            world.read().get_block_state(point)
        } else {
            // Turn to each cell before sampling it. A single forward cone
            // cannot include both landing support and ascent headroom.
            let center = point.center();
            let direction = azalea::bot::direction_looking_at(eye, center);
            if bot
                .set_direction(direction.y_rot(), direction.x_rot())
                .is_err()
            {
                return None;
            }
            let sight = crate::perception::Sight::new(eye, direction.y_rot(), direction.x_rot());
            // set_direction enters Azalea's ECS; never hold a world read guard
            // across it. Keep this guard only while sampling the new sight line.
            let world = world.read();
            sight.block(point, |p| world.get_block_state(p))
        }
    };
    let clear = wood_transition_clear(position, from, target, grounded, &mut read);
    let direction =
        azalea::bot::direction_looking_at(eye, Vec3::new(target.x, target.y + 0.75, target.z));
    bot.set_direction(direction.y_rot(), direction.x_rot())
        .map_err(|_| "Wood step look unavailable")?;
    Ok(clear)
}

/// The bounded local executor: a stay-put action, or a move to an observed target inside the
/// mover's verified range.
///
/// Every check that can refuse the action has its own reason ([`crate::refusal`]), and the
/// order is the order the guard applies: the body first, then whether any executor in this
/// build runs the candidate at all, then that executor's bounds. The concrete reason travels
/// back to the engine in the reply and into the recording, so a refusal is never again a
/// sentence that fits an expired deadline, a dead bot, a distant target and a candidate nothing
/// can run.
fn execute(bot: &Client, candidate: &Candidate, accepted_before: Instant) -> Result<(), Refusal> {
    if Instant::now() >= accepted_before {
        return Err(Refusal::AcceptanceExpiredBeforeExecution);
    }
    // The two facts only a running client can supply. Both are read once, so the decision
    // itself is a pure function a test can pin without a Minecraft connection.
    let health = bot.health().ok();
    let target_distance_m = candidate.target.as_ref().and_then(|target| {
        bot.position()
            .ok()
            .map(|position| position.distance_to(Vec3::new(target.x, target.y, target.z)))
    });
    match refusal::admit(candidate, health, target_distance_m)? {
        // Nothing moves: the candidate's duration is the whole action.
        LocalExecutor::StayPut => return Ok(()),
        LocalExecutor::Move => {}
        // `admit` refuses these two with their own reason, so reaching here means the guard
        // and its own decision disagree: refuse rather than move something nothing prepared.
        LocalExecutor::Skill => {
            return Err(Refusal::SkillNotPrepared {
                candidate: candidate.id.clone(),
            });
        }
        LocalExecutor::Missing => {
            return Err(Refusal::NoLocalExecutor {
                candidate: candidate.id.clone(),
            });
        }
    }
    let Some(target) = &candidate.target else {
        return Err(Refusal::NoLocalExecutor {
            candidate: candidate.id.clone(),
        });
    };
    let goal = Position {
        x: target.x,
        y: target.y,
        z: target.z,
    };
    if Instant::now() >= accepted_before {
        return Err(Refusal::AcceptanceExpiredDuringValidation);
    }
    // One mover: waypoint navigation, flight and any other long-range goal enter the
    // same plan as a harvest route. The standable-endpoint guard lives inside the mover.
    let mut nav = BotNav { bot };
    crate::motion::follow(&mut nav, crate::motion::Plan::Goal { target: &goal }).map_err(
        |reason| Refusal::MoveRefused {
            reason: reason.to_owned(),
        },
    )?;
    Ok(())
}

/// Why this adapter refuses a queued action against the world it observes right now, or `None`
/// when the action still belongs to it.
///
/// One check per reason, in the order they are applied, so the refusal names the check that
/// failed: a queued action could be void because no world had spawned yet, because the caller
/// cancelled it, because the world lifecycle it was bound to has ended, or because the bot is
/// in another world than the one the action was observed in. The old single sentence fitted all
/// four at once, which is why a recording could not say which one happened.
fn identity_refusal(
    request: &ActionRequest,
    ready: bool,
    world_epoch: u64,
    observed_dimension: Option<&str>,
) -> Option<Refusal> {
    if !ready {
        return Some(Refusal::WorldNotSpawned);
    }
    if request.reply.is_closed() {
        return Some(Refusal::CancelledBeforeGuard);
    }
    if request.world_epoch != world_epoch {
        return Some(Refusal::WorldLifecycleChanged {
            action_epoch: request.world_epoch,
            world_epoch,
        });
    }
    if request.dimension.as_deref() != observed_dimension {
        return Some(Refusal::WorldIdentityMismatch {
            expected: request.dimension.clone(),
            observed: observed_dimension.map(str::to_owned),
        });
    }
    None
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
    crate::perception::clear_stance(foot_block)
        && crate::perception::clear_stance(head_block)
        && supports_endpoint(support)
}

fn supports_endpoint(support: azalea::block::BlockState) -> bool {
    crate::perception::full_support(support)
}

/// Day time of the overworld clock in a SetTime packet, as a tick within the day cycle.
///
/// Minecraft 26.2 sends one clock state per world clock instead of a single day time
/// (`ClientboundSetTime::clock_updates`). A clock whose id the client's registries cannot
/// resolve is used only when it is the packet's single clock.
fn day_time(packet: &ClientboundSetTime, registries: &RegistryHolder) -> Option<u64> {
    let single = packet.clock_updates.len() == 1;
    let mut fallback = None;
    for (clock, state) in &packet.clock_updates {
        match clock.key(registries) {
            Some(WorldClockKey::Overworld) => return Some(normalise_day_time(state.total_ticks)),
            None if single => fallback = Some(normalise_day_time(state.total_ticks)),
            _ => {}
        }
    }
    fallback
}

fn normalise_day_time(ticks: u64) -> u64 {
    ticks % DAY_TICKS
}

/// Owned items, using the active menu's freshest player slots. Container contents
/// and crafting previews are not property; the cursor is counted exactly once.
fn inventory_totals(inventory: &Inventory) -> BTreeMap<String, u32> {
    let menu = inventory.menu();
    let player = inventory.inventory_menu.as_player();
    let mut totals = BTreeMap::new();
    let craft = if inventory.container_menu.is_none() {
        &player.craft[..]
    } else {
        &[]
    };
    for item in menu
        .player_slots_range()
        .filter_map(|slot| menu.slot(slot))
        .chain(player.armor.iter())
        .chain(std::iter::once(&player.offhand))
        .chain(craft.iter())
        .chain(std::iter::once(&inventory.carried))
    {
        if !item.is_empty() {
            let total: &mut u32 = totals.entry(item.kind().to_str().to_owned()).or_default();
            *total = total.saturating_add(u32::try_from(item.count()).unwrap_or(0));
        }
    }
    totals
}

/// Pinned Azalea indexes loaded entities by chunk. Enumerate at most `limit`
/// entries from the nine chunks around the player, then sort only that bounded
/// sample. Truncation is explicit; a partial list cannot prove work is safe.
fn bounded_nearby_entities(
    bot: &Client,
    position: Vec3,
    limit: usize,
) -> Option<(Vec<EntityRef>, bool)> {
    let world = bot.world().ok()?;
    let world = world.read();
    let center = ChunkPos::from(position);
    let mut ids = Vec::new();
    let mut incomplete = false;
    'chunks: for dx in -1..=1 {
        for dz in -1..=1 {
            let chunk = ChunkPos::new(center.x + dx, center.z + dz);
            if let Some(actors) = world.entities_by_chunk.get(&chunk) {
                for id in actors {
                    if *id == bot.entity {
                        continue;
                    }
                    if ids.len() == limit {
                        incomplete = true;
                        break 'chunks;
                    }
                    ids.push(*id);
                }
            }
        }
    }
    drop(world);
    let mut nearby = Vec::new();
    for id in ids {
        let actor = EntityRef::new(bot.clone(), id);
        if let Ok(point) = actor.position() {
            if point.distance_to(position) <= 16.0 {
                nearby.push((point.distance_to(position), actor));
            }
        } else {
            incomplete = true;
        }
    }
    nearby.sort_by(|a, b| a.0.total_cmp(&b.0));
    Some((
        nearby.into_iter().map(|(_, actor)| actor).collect(),
        incomplete,
    ))
}

#[derive(Default)]
struct NativeTreeMemory {
    dimension: Option<String>,
    trees: crate::wood_tree::TreeMemory,
}

fn observe(
    bot: &Client,
    sequence: u64,
    world_epoch: u64,
    time_of_day: Option<u64>,
    note: &str,
    tree_memory: &mut NativeTreeMemory,
) -> Option<Observation> {
    let dimension = bot.world_name().ok()?.0.to_string();
    if tree_memory.dimension.as_deref() != Some(dimension.as_str()) {
        tree_memory.dimension = Some(dimension.clone());
        tree_memory.trees = crate::wood_tree::TreeMemory::default();
    }
    let position = bot.position().ok()?;
    let eye = bot.eye_position().ok()?;
    let look = *bot.component::<azalea::entity::LookDirection>().ok()?;
    // One bounded look-around frame: several sectors of the same current world,
    // not a single wedge about the server-reported direction.
    let views = crate::perception::Views::new(eye, look.y_rot(), look.x_rot());
    let mut view_cost = crate::perception::ViewCost::default();
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
    if time_of_day.is_none() {
        missing.push("time unavailable");
    }
    let items = bot
        .component::<Inventory>()
        .ok()
        .map(|inventory| inventory_totals(&inventory))
        .unwrap_or_default();
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
    // `Client::get_held_item` reads the selected hotbar slot of the open menu.
    let held_item = match bot.get_held_item() {
        Ok(item) if item.is_empty() => None,
        Ok(item) => Some(item.kind().to_str().to_owned()),
        Err(_) => {
            missing.push("held item unavailable");
            None
        }
    };
    let origin = Position {
        x: position.x,
        y: position.y,
        z: position.z,
    };
    // Resource finds borrow `&'static` registry ids, so only the kept ones allocate.
    let mut finds: Vec<(&'static str, &'static str, Position)> = Vec::new();
    let mut blocks = Vec::new();
    let mut block_facts = Vec::new();
    let mut trees;
    let placement_stance = crafting::craft_stance(bot);
    if let Ok(world) = bot.world() {
        let world = world.read();
        let occupied = BlockPos::from(position);
        let (visible, frame_cost) = views.scan(occupied, |p| world.get_block_state(p));
        view_cost = frame_cost;
        if placement_stance {
            blocks.extend(placement::sites(position, &visible));
        }
        // Two different questions, two different sources. What the route search may
        // *report as work* comes from the frame: a cell may only be a harvest target when
        // this observation perceived it (`visible`), so the catalogue can never offer a
        // break of a block the engine has not seen. What the route search may *plan
        // through* comes from the loaded world, because that is exactly what the mover's
        // own route validator reads before every step (`BotNav::verify_route`: `standable`
        // plus the headroom cell at `max(y) + 2`), and a plan stricter than its own
        // validator makes lawful work unreachable. It did: a one-block step up needs the
        // headroom cell above the bot's own column, 1.88 blocks above the eye and so 75 to
        // 90 degrees of elevation, while every look sector is a 120-degree cone pointing
        // horizontally or tilted down and reaches at most 60 degrees above the horizon
        // (`crate::perception::LOOK_SECTORS`). Unless the bot happens to look up by more
        // than fifteen degrees, which it does not while it faces the block it will break,
        // that cell is never in the frame, so no route with a height change could be
        // verified. Live evidence: the operator-placed trunk in
        // `runs/level1-live/live-known-block.json` was perceived
        // (`resources`: minecraft:oak_log at (-1445.5, 72.5, -1548.5) and
        // (-1445.5, 73.5, -1548.5)) and the loaded world called the cell beside it
        // standable (`blocks`: waypoint:-1447:72:-1549), yet every tree view reported
        // `next: []` and `wood::gather_candidates` offered nothing, so no `gather_*`
        // candidate ever reached the model. Each step is still authorized separately by
        // fresh sight of its own swept volume (`observed_wood_step`), so planning through
        // the loaded world does not authorize walking through cells the bot has not
        // re-observed.
        let observed_trees = crate::wood_tree::inspect(&origin, |(x, y, z)| {
            let point = BlockPos::new(x, y, z);
            let contact = x == occupied.x
                && z == occupied.z
                && (occupied.y - 1..=occupied.y + 1).contains(&y);
            // A harvest target outside the bot's own contact column must be a perceived
            // block; the contact column is read from the world by the frame as well.
            let perceived = visible.get(point).is_some();
            let block = world.get_block_state(point)?;
            let name = BlockKind::from(block).to_str();
            Some(crate::wood_tree::Terrain {
                air: crate::perception::is_air(block),
                support: supports_endpoint(block),
                log: (crate::wood::is_harvestable(name) && (contact || perceived))
                    .then(|| name.to_owned()),
            })
        });
        // Reconcile only cells in this eye's visible frame. The loaded chunk
        // cache and contact geometry cannot prove that hidden wood is gone.
        trees = tree_memory
            .trees
            .update(world_epoch, observed_trees, |(x, y, z), name| {
                visible
                    .get(BlockPos::new(x, y, z))
                    .map(|block| BlockKind::from(block).to_str() == name)
            });
        // TreeView is public observation data. Keep historical coordinates in
        // TreeMemory only; the exposed log list contains current visible wood.
        for tree in &mut trees {
            let wood = tree.wood.as_str();
            tree.logs.retain(|log| {
                visible
                    .get(BlockPos::from(Vec3::new(log.x, log.y, log.z)))
                    .is_some_and(|block| BlockKind::from(block).to_str() == wood)
            });
        }
        let center = BlockPos::from(position);
        // Bounded resource scan of loaded cells; unloaded cells are skipped.
        for (point, block) in visible.iter() {
            if crate::perception::is_air(block) {
                continue;
            }
            let name = BlockKind::from(block).to_str();
            if let Some(kind) = classify_block(name) {
                finds.push((
                    kind,
                    name,
                    Position {
                        x: point.x as f64 + 0.5,
                        y: point.y as f64 + 0.5,
                        z: point.z as f64 + 0.5,
                    },
                ));
            }
        }
        // A bounded sample, not a complete world map; only loaded non-air cells.
        for x in -3..=3 {
            for z in -3..=3 {
                for y in -1..=2 {
                    let point = BlockPos::new(center.x + x, center.y + y, center.z + z);
                    if let Some(block) = visible.get(point)
                        && !crate::perception::is_air(block)
                    {
                        if block_facts.len() < 16 {
                            block_facts.push(crate::perception::facts(point, block));
                        }
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
                    if (x != 0 || z != 0)
                        && standable(&world, feet)
                        && [
                            feet,
                            BlockPos::new(feet.x, feet.y + 1, feet.z),
                            BlockPos::new(feet.x, feet.y - 1, feet.z),
                        ]
                        .iter()
                        .all(|p| visible.get(*p).is_some())
                    {
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
        trees = tree_memory
            .trees
            .update(world_epoch, Vec::new(), |_, _| None);
        for tree in &mut trees {
            tree.logs.clear();
        }
    }
    let entity_views = crate::perception::Views::new(eye, look.y_rot(), look.x_rot())
        .with_budget(crate::perception::ENTITY_CELL_BUDGET);
    let entity_visible = |point: Vec3| {
        bot.world().ok().is_some_and(|world| {
            let world = world.read();
            entity_views.visible(point, false, |p| world.get_block_state(p))
        })
    };
    let mut entities = Vec::new();
    let entity_sample = bounded_nearby_entities(bot, position, 256);
    let mut entity_scan_incomplete = entity_sample
        .as_ref()
        .is_none_or(|(_, incomplete)| *incomplete);
    if let Some((nearby, _)) = entity_sample {
        for entity in &nearby {
            if entity.id() == bot.entity {
                continue;
            }
            if let (Ok(point), Ok(kind)) = (entity.position(), entity.kind())
                && point.distance_to(position) <= 16.0
                && entity_visible(point)
            {
                // Keep the nearest 32 ordinary landmarks for display, but
                // inspect every bounded actor for hostiles needed by safety.
                let name = format!("{kind:?}");
                if entities.len() >= 32 && !crate::survival::is_hostile(&name) {
                    continue;
                }
                entities.push(Landmark {
                    name,
                    position: Position {
                        x: point.x,
                        y: point.y,
                        z: point.z,
                    },
                });
            }
        }
        // Animals are not capped by the 32 landmarks above: every known one in range counts.
        for entity in nearby.iter().take(128) {
            if entity.id() == bot.entity {
                continue;
            }
            if let (Ok(point), Ok(kind)) = (entity.position(), entity.kind())
                && animal_in_range(
                    &origin,
                    &Position {
                        x: point.x,
                        y: point.y,
                        z: point.z,
                    },
                )
                && entity_visible(point)
                && let Some(animal) = classify_entity(kind.to_str())
            {
                finds.push((
                    animal,
                    kind.to_str(),
                    Position {
                        x: point.x,
                        y: point.y,
                        z: point.z,
                    },
                ));
            }
        }
    } else {
        missing.push("entities unavailable");
    }
    if entity_views.cost().exhausted {
        entity_scan_incomplete = true;
    }
    if entity_scan_incomplete {
        missing.push("bounded entity scan or visibility incomplete; omitted actors unknown");
    }
    if view_cost.exhausted || entity_views.cost().exhausted {
        missing.push("visibility cell budget exhausted; omitted cells unknown");
    }
    Some(Observation {
        world_epoch,
        dimension: Some(dimension),
        // The caller stamps the adapter's death count; `observe` only reads the client.
        deaths: 0,
        sequence,
        connected: true,
        position: origin.clone(),
        health,
        food,
        inventory,
        blocks,
        block_facts,
        entities,
        entity_scan_incomplete,
        note: format!(
            "{note} Bounded look-around frame: {} of {} look directions ({} degrees per view, {}m maximum, {} cell reads per direction, ground sectors tilted {} degrees down); {} ray cell reads, {} unknown/unsupported occlusions. Lighting/fluid visibility unresolved; conservative geometric visibility only. {}",
            view_cost.directions,
            crate::perception::LOOK_SECTORS.len(),
            crate::perception::VIEW_CONE_DEGREES,
            crate::perception::VIEW_DISTANCE,
            crate::perception::CELL_BUDGET,
            crate::perception::LOOK_TILT_DEGREES,
            view_cost.reads,
            view_cost.unknown,
            missing.join("; ")
        ),
        time_of_day,
        held_item,
        items,
        resources: nearest_resources(&origin, finds),
        trees,
        crafting_inventory: None,
        skill_outcome: None,
        recoverable_drop: None,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use azalea::{
        BlockPos, Vec3,
        entity::inventory::Inventory,
        inventory::ItemStack,
        registry::builtin::{BlockKind, ItemKind},
    };

    #[test]
    fn wood_step_rechecks_landing_support_before_movement() {
        let from = Vec3::new(0.5, 0.0, 0.5);
        let target = Vec3::new(1.5, 0.0, 0.5);
        assert!(!super::wood_transition_clear(
            from,
            from,
            target,
            true,
            |_| { Some(BlockKind::Air.into()) }
        ));
        assert!(super::wood_transition_clear(
            from,
            from,
            target,
            true,
            |point| {
                Some(
                    if point == BlockPos::new(1, -1, 0) || point == BlockPos::new(0, -1, 0) {
                        BlockKind::Stone.into()
                    } else {
                        BlockKind::Air.into()
                    },
                )
            }
        ));
    }

    #[test]
    fn wood_jump_requires_visible_swept_headroom() {
        let from = Vec3::new(0.5, 0.0, 0.5);
        let target = Vec3::new(1.5, 1.0, 0.5);
        let extra = BlockPos::new(0, 3, 0);
        assert!(super::wood_swept_cells(from, from, target).contains(&extra));
        assert!(super::wood_transition_clear(
            from,
            from,
            target,
            true,
            |point| {
                Some(
                    if point == BlockPos::new(1, 0, 0) || point == BlockPos::new(0, -1, 0) {
                        BlockKind::Stone.into()
                    } else {
                        BlockKind::Air.into()
                    },
                )
            }
        ));
        assert!(!super::wood_transition_clear(
            from,
            from,
            target,
            true,
            |point| {
                if point == extra {
                    None // Occluded or unloaded is never an air claim.
                } else {
                    Some(
                        if point == BlockPos::new(1, 0, 0) || point == BlockPos::new(0, -1, 0) {
                            BlockKind::Stone.into()
                        } else {
                            BlockKind::Air.into()
                        },
                    )
                }
            }
        ));
    }

    #[test]
    fn wood_airborne_step_cancels_when_swept_clearance_changes() {
        let from = Vec3::new(0.5, 0.0, 0.5);
        let target = Vec3::new(1.5, 1.0, 0.5);
        let airborne = Vec3::new(1.1, 1.35, 0.5);
        let blocked = BlockPos::new(1, 3, 0);
        assert!(super::wood_swept_cells(airborne, from, target).contains(&blocked));
        assert!(super::wood_transition_clear(
            airborne,
            from,
            target,
            false,
            |point| {
                Some(if point == BlockPos::new(1, 0, 0) {
                    BlockKind::Stone.into()
                } else {
                    BlockKind::Air.into()
                })
            }
        ));
        assert!(!super::wood_transition_clear(
            airborne,
            from,
            target,
            false,
            |point| {
                Some(if point == blocked || point == BlockPos::new(1, 0, 0) {
                    BlockKind::Stone.into()
                } else {
                    BlockKind::Air.into()
                })
            }
        ));
    }

    #[test]
    fn predicted_air_waits_for_server_removal_without_mining_again() {
        use super::{MiningDecision, mining_decision};

        // The local mining simulation removes the block before its server packet.
        assert_eq!(
            mining_decision(true, false, false, false, true),
            Ok(MiningDecision::WaitForServer)
        );
        // A stance or look that drifted after the request went out does not cancel it: the
        // break is already in flight. This is the live failure at runs/level1-live/live-dt.json
        // event 20, where the bot coasted 0.2 blocks past its stance while the predicted air
        // was still unconfirmed and the attempt was thrown away.
        assert_eq!(
            mining_decision(true, false, false, false, false),
            Ok(MiningDecision::WaitForServer)
        );
        // Only the matching authoritative removal allows collection.
        assert_eq!(
            mining_decision(true, false, true, true, false),
            Ok(MiningDecision::Collect)
        );
        // A server update received before the local world applies it also waits.
        assert_eq!(
            mining_decision(false, true, true, true, true),
            Ok(MiningDecision::Continue)
        );
        assert_eq!(
            mining_decision(false, false, false, true, true),
            Err("Gather target replaced during mining")
        );
    }

    #[test]
    fn a_break_the_client_never_started_is_requested_again_instead_of_waiting_out_the_deadline() {
        use super::{MiningDecision, mining_decision};

        // The target is still the expected block and the local client is mining nothing: the
        // queued request was consumed without a break. Five live attempts died here -- 10 s
        // each, block never touched, inventory never changed (live-dt.json events 28-52).
        assert_eq!(
            mining_decision(false, true, false, false, true),
            Ok(MiningDecision::Reask)
        );
        // The same, while this tick's stance or sightline is not verified: turn toward the
        // target and re-read rather than ending the attempt.
        assert_eq!(
            mining_decision(false, true, false, false, false),
            Ok(MiningDecision::TurnToTarget)
        );
    }

    #[test]
    fn mining_requests_are_retried_on_a_client_tick_budget_not_a_wall_clock() {
        use super::{MINING_ANSWER_TICKS, MINING_ASK_BUDGET, MiningAsks};

        let mut asks = MiningAsks::default();
        // The first request is the action's own request, not a retry.
        assert!(asks.due());
        asks.ask();
        assert!(!asks.due());
        for _ in 1..MINING_ANSWER_TICKS {
            asks.tick();
            assert!(
                !asks.due(),
                "a client answers within a tick or two, not instantly"
            );
        }
        asks.tick();
        assert!(
            asks.due(),
            "one second of client ticks is long enough to answer"
        );

        // Replaying the executor's own cycle: ask, wait, ask, wait, ask, wait, then a verdict
        // that names the client instead of the deadline. Counted in client ticks throughout.
        let (mut asks, mut ticks, mut asked) = (MiningAsks::default(), 0_u64, 0_u32);
        loop {
            asks.tick();
            ticks += 1;
            if !asks.due() {
                continue;
            }
            if asks.budget_spent() {
                break;
            }
            asks.ask();
            asked += 1;
        }
        assert_eq!(asked, MINING_ASK_BUDGET);
        assert!(ticks >= u64::from(MINING_ASK_BUDGET) * MINING_ANSWER_TICKS);
        assert!(ticks <= u64::from(MINING_ASK_BUDGET) * (MINING_ANSWER_TICKS + 1));
        // The whole verdict lands well inside the shortest advertised action bound, so the
        // engine reports this reason instead of "action deadline expired".
        assert!(ticks * 50 < 10_000);
    }

    #[test]
    fn item_totals_aggregate_player_slots_only_and_use_canonical_ids() {
        let mut inventory = Inventory::default();
        let player = inventory.inventory_menu.as_player_mut();
        player.inventory[0] = ItemStack::new(ItemKind::OakLog, 5);
        player.inventory[1] = ItemStack::new(ItemKind::OakLog, 7);
        player.inventory[2] = ItemStack::new(ItemKind::Dirt, 2);

        // Exercise Azalea's real Inventory and ItemStack models. The helper is the seam
        // observe should use to avoid counting an open container's slots as bot property.
        assert_eq!(
            super::inventory_totals(&inventory),
            BTreeMap::from([
                ("minecraft:dirt".to_owned(), 2),
                ("minecraft:oak_log".to_owned(), 12),
            ])
        );
    }

    #[test]
    fn open_chest_uses_fresh_player_slots_without_counting_chest_or_stale_copy() {
        use azalea::{inventory::Menu, registry::builtin::MenuKind};
        let mut inventory = Inventory::default();
        let player = inventory.inventory_menu.as_player_mut();
        player.inventory[0] = ItemStack::new(ItemKind::OakLog, 64);
        player.armor[0] = ItemStack::new(ItemKind::IronHelmet, 1);
        player.offhand = ItemStack::new(ItemKind::Torch, 4);
        player.craft_result = ItemStack::new(ItemKind::OakPlanks, 4);
        player.craft[0] = ItemStack::new(ItemKind::OakLog, 32);
        let mut chest = Menu::from_kind(MenuKind::Generic9x3);
        *chest.slot_mut(0).unwrap() = ItemStack::new(ItemKind::Diamond, 64);
        let start = *chest.player_slots_range().start();
        *chest.slot_mut(start).unwrap() = ItemStack::new(ItemKind::OakLog, 5);
        *chest.slot_mut(start + 35).unwrap() = ItemStack::new(ItemKind::OakLog, 7);
        inventory.container_menu = Some(chest);
        inventory.id = 1;
        inventory.carried = ItemStack::new(ItemKind::OakLog, 2);
        assert_eq!(
            super::inventory_totals(&inventory),
            BTreeMap::from([
                ("minecraft:oak_log".into(), 14),
                ("minecraft:iron_helmet".into(), 1),
                ("minecraft:torch".into(), 4),
            ])
        );
    }

    #[test]
    fn closed_crafting_inputs_are_owned_but_output_is_only_a_preview() {
        let mut inventory = Inventory::default();
        let player = inventory.inventory_menu.as_player_mut();
        player.craft[0] = ItemStack::new(ItemKind::OakLog, 2);
        player.craft_result = ItemStack::new(ItemKind::OakPlanks, 4);
        assert_eq!(
            super::inventory_totals(&inventory),
            BTreeMap::from([("minecraft:oak_log".into(), 2),])
        );
    }

    #[test]
    fn time_packet_uses_overworld_clock_not_game_time_or_other_clocks() {
        use azalea::{
            core::registry_holder::RegistryHolder,
            protocol::packets::game::{ClientboundSetTime, c_set_time::ClockState},
            registry::{DataRegistry, data::WorldClock},
        };
        let mut registries = RegistryHolder::default();
        registries.append(
            "minecraft:world_clock".parse().unwrap(),
            vec![
                ("example:other".parse().unwrap(), Some(Default::default())),
                (
                    "minecraft:overworld".parse().unwrap(),
                    Some(Default::default()),
                ),
            ],
        );
        let mut packet = ClientboundSetTime {
            game_time: 777,
            clock_updates: Default::default(),
        };
        assert_eq!(super::day_time(&packet, &registries), None);
        packet.clock_updates.insert(
            WorldClock::new_raw(0),
            ClockState {
                total_ticks: 888,
                partial_tick: 0.0,
                rate: 1.0,
            },
        );
        packet.clock_updates.insert(
            WorldClock::new_raw(1),
            ClockState {
                total_ticks: 48_017,
                partial_tick: 0.0,
                rate: 0.0,
            },
        );
        assert_eq!(super::day_time(&packet, &registries), Some(17));
        assert_eq!(super::day_time(&packet, &RegistryHolder::default()), None);
        packet.clock_updates.shift_remove(&WorldClock::new_raw(0));
        assert_eq!(
            super::day_time(&packet, &RegistryHolder::default()),
            Some(17)
        );
    }

    #[test]
    fn raw_day_clock_ticks_are_normalized_to_one_day() {
        assert_eq!(super::normalise_day_time(24_017), 17);
        assert_eq!(super::normalise_day_time(48_000), 0);
        assert!(super::normalise_day_time(23_999) < 24_000);
    }

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

    /// Regression for the live run's silence: azalea re-sends `Login` for a world switch, but
    /// `Spawn` only follows once the new world's chunks arrive. Blanking publication on every
    /// login left the engine without fresh observations after a dimension transfer and ended
    /// the session as `provider_failed`. A login inside an observable world must keep
    /// publishing and start a new world lifecycle instead.
    #[test]
    fn a_login_inside_an_observable_world_keeps_publishing_and_starts_a_new_lifecycle() {
        let first = login_effect(false);
        assert!(
            first.awaits_spawn,
            "the first login has no observable world to publish"
        );
        assert!(!first.transfers_world);

        let transfer = login_effect(true);
        assert!(
            transfer.transfers_world,
            "a login after an observation is a world transfer"
        );
        assert!(
            !transfer.awaits_spawn,
            "a transfer must not wait for a spawn that a dropped-in world may never send"
        );
    }

    /// The queued action used by the world-identity checks: a bounded wait, so nothing else
    /// about the candidate can refuse it.
    fn queued_wait() -> (
        Box<ActionRequest>,
        tokio::sync::oneshot::Receiver<Result<Instant, Refusal>>,
    ) {
        let observation = Observation {
            world_epoch: 4,
            dimension: Some("minecraft:dt".into()),
            ..Observation::default()
        };
        let candidate = Candidate {
            skill: None,
            id: "wait".into(),
            description: "Stay where you are and keep observing.".into(),
            target: None,
            duration_ms: 2_000,
        };
        let (request, reply) = ActionRequest::new(candidate, &observation);
        (Box::new(request), reply)
    }

    /// One sentence covered a missing spawn, a cancelled caller, an ended world lifecycle and
    /// another world altogether. Each of the four now names itself, so a recording says which
    /// check refused the action the model chose.
    #[test]
    fn a_queued_action_names_the_world_check_that_refused_it() {
        let (request, reply) = queued_wait();
        assert_eq!(
            identity_refusal(&request, false, 4, Some("minecraft:dt")),
            Some(Refusal::WorldNotSpawned),
            "no spawned world is a reason of its own"
        );
        assert_eq!(
            identity_refusal(&request, true, 5, Some("minecraft:dt")),
            Some(Refusal::WorldLifecycleChanged {
                action_epoch: 4,
                world_epoch: 5,
            }),
            "the world lifecycle that the action was bound to has ended"
        );
        assert_eq!(
            identity_refusal(&request, true, 4, Some("minecraft:overworld")),
            Some(Refusal::WorldIdentityMismatch {
                expected: Some("minecraft:dt".into()),
                observed: Some("minecraft:overworld".into()),
            }),
            "the bot is in another world than the action was observed in"
        );
        assert_eq!(
            identity_refusal(&request, true, 4, Some("minecraft:dt")),
            None,
            "an action that still belongs to this world is not refused"
        );

        drop(reply);
        assert_eq!(
            identity_refusal(&request, true, 4, Some("minecraft:dt")),
            Some(Refusal::CancelledBeforeGuard),
            "a cancelled action is not an expired one"
        );
    }
}
