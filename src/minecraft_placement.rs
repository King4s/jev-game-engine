//! Conservative table placement using pinned native hit results and inbound evidence.
use super::crafting::{ServerInventory, craft_stance};
use crate::model::{Candidate, Landmark, Mode, Position, Settings, SkillOutcome, WoodSkill};
use azalea::{
    BlockPos, Client, Vec3,
    core::{direction::Direction, hit_result::HitResult},
    interact::BlockStatePredictionHandler,
    inventory::{ItemStack, operations::ClickType},
    packet::game::SendGamePacketEvent,
    protocol::packets::game::{
        ClientboundGamePacket, ServerboundContainerClick, ServerboundUseItemOn,
        s_container_click::HashedStack, s_interact::InteractionHand,
    },
    registry::builtin::BlockKind,
};
use std::time::{Duration, Instant};

/// One conversion for the placement stance check, which goes through the mover's
/// `holds_stance` rather than a tolerance of its own.
fn as_position(p: Vec3) -> Position {
    Position {
        x: p.x,
        y: p.y,
        z: p.z,
    }
}

// Deliberately small verified placement surface policy. Other full shapes are not
// assumed non-interactive, harmless or stable merely because they collide.
fn support(state: azalea::block::BlockState) -> bool {
    matches!(
        BlockKind::from(state),
        BlockKind::Stone | BlockKind::Cobblestone | BlockKind::Dirt | BlockKind::GrassBlock
    ) && crate::perception::full_support(state)
}

pub(super) fn sites(position: Vec3, frame: &crate::perception::VisibleFrame) -> Vec<Landmark> {
    let occupied = BlockPos::from(position);
    let safe = |p: BlockPos| {
        frame.get(p).is_some_and(crate::perception::clear_stance)
            && frame
                .get(BlockPos::new(p.x, p.y + 1, p.z))
                .is_some_and(crate::perception::clear_stance)
            && frame
                .get(BlockPos::new(p.x, p.y - 1, p.z))
                .is_some_and(support)
    };
    let adjacent: Vec<_> = [(1, 0), (-1, 0), (0, 1), (0, -1)]
        .into_iter()
        .map(|(x, z)| BlockPos::new(occupied.x + x, occupied.y, occupied.z + z))
        .filter(|p| safe(*p))
        .collect();
    if adjacent.len() < 2 {
        return vec![];
    }
    adjacent
        .into_iter()
        .map(|p| {
            let position = Position {
                x: p.x as f64 + 0.5,
                y: p.y as f64,
                z: p.z as f64 + 0.5,
            };
            Landmark {
                name: crate::placement::site_name(&position),
                position,
            }
        })
        .collect()
}

fn current_site(bot: &Client, target: BlockPos) -> bool {
    if !craft_stance(bot) {
        return false;
    }
    let Ok(position) = bot.position() else {
        return false;
    };
    let Ok(eye) = bot.eye_position() else {
        return false;
    };
    let Ok(look) = bot.component::<azalea::entity::LookDirection>().map(|v| *v) else {
        return false;
    };
    let Ok(world) = bot.world() else {
        return false;
    };
    let world = world.read();
    let frame = crate::perception::Sight::new(eye, look.y_rot(), look.x_rot())
        .scan(BlockPos::from(position), |p| world.get_block_state(p));
    sites(position, &frame)
        .iter()
        .any(|s| BlockPos::from(Vec3::new(s.position.x, s.position.y, s.position.z)) == target)
}

enum Phase {
    Stage,
    AwaitSwap(u64, Instant),
    Select,
    Aim,
    Confirm(u64),
}
pub(super) struct Place {
    candidate: Candidate,
    epoch: u64,
    attempt: u64,
    dimension: String,
    target: BlockPos,
    support: BlockPos,
    source: usize,
    hotbar: u8,
    expected: Vec<ItemStack>,
    phase: Phase,
    position: Vec3,
    health: f32,
    deadline: Instant,
    aim_observed: Option<Instant>,
    sent: bool,
    placed: bool,
}
impl Place {
    pub(super) fn prepare(
        bot: &Client,
        candidate: &Candidate,
        settings: &Settings,
        epoch: u64,
        attempt: u64,
        server: &ServerInventory,
    ) -> Result<Self, ()> {
        let Some(WoodSkill::PlaceTable {
            support,
            source_slot,
            hotbar_slot,
        }) = &candidate.skill
        else {
            return Err(());
        };
        let target = candidate.target.as_ref().ok_or(())?;
        let target = BlockPos::from(Vec3::new(target.x, target.y, target.z));
        let bound_support = BlockPos::from(Vec3::new(support.x, support.y, support.z));
        if !settings.wood_skills
            || settings.mode != Mode::Live
            || !(1..=5000).contains(&candidate.duration_ms)
            || *hotbar_slot > 8
            || !(9..=44).contains(source_slot)
            || bound_support != BlockPos::new(target.x, target.y - 1, target.z)
            || bot.world_name().map_err(|_| ())?.0.to_string() != settings.allowed_dimension
            || !current_site(bot, target)
        {
            return Err(());
        }
        server.idle_view(bot).ok_or(())?;
        let source = usize::from(*source_slot);
        let hot = 36 + usize::from(*hotbar_slot);
        if server.slots[source].kind().to_str() != crate::placement::TABLE
            || server.slots[source].count() <= 0
            || (source != hot && (source >= 36 || !server.slots[hot].is_empty()))
        {
            return Err(());
        }
        let health = bot.health().map_err(|_| ())?;
        if !health.is_finite() || health <= 0.0 {
            return Err(());
        }
        Ok(Self {
            candidate: candidate.clone(),
            epoch,
            attempt,
            dimension: settings.allowed_dimension.clone(),
            target,
            support: bound_support,
            source,
            hotbar: *hotbar_slot,
            expected: server.slots.clone(),
            phase: Phase::Stage,
            position: bot.position().map_err(|_| ())?,
            health,
            deadline: Instant::now() + Duration::from_millis(candidate.duration_ms),
            aim_observed: None,
            sent: false,
            placed: false,
        })
    }
    pub(super) fn outcome(&self, success: bool, message: &str) -> SkillOutcome {
        SkillOutcome {
            attempt_id: self.attempt,
            candidate_id: self.candidate.id.clone(),
            world_epoch: self.epoch,
            success,
            confirmed_by_server: success,
            message: message.into(),
        }
    }
    pub(super) fn receive(&mut self, packet: &ClientboundGamePacket) {
        if !self.sent {
            // A brief look transition may reuse this observation only until any
            // server block delta arrives. Never retain a historical placement map.
            if matches!(
                packet,
                ClientboundGamePacket::BlockUpdate(_)
                    | ClientboundGamePacket::SectionBlocksUpdate(_)
                    | ClientboundGamePacket::LevelChunkWithLight(_)
                    | ClientboundGamePacket::ForgetLevelChunk(_)
            ) {
                self.aim_observed = None;
            }
            return;
        }
        match packet {
            ClientboundGamePacket::BlockUpdate(p) if p.pos == self.target => {
                self.placed = BlockKind::from(p.block_state) == BlockKind::CraftingTable;
            }
            ClientboundGamePacket::SectionBlocksUpdate(p) => {
                for change in &p.states {
                    let pos = BlockPos::new(
                        p.section_pos.x * 16 + i32::from(change.pos.x),
                        p.section_pos.y * 16 + i32::from(change.pos.y),
                        p.section_pos.z * 16 + i32::from(change.pos.z),
                    );
                    if pos == self.target {
                        self.placed = BlockKind::from(change.state) == BlockKind::CraftingTable;
                    }
                }
            }
            _ => {}
        }
    }
    pub(super) fn advance(
        &mut self,
        bot: &Client,
        epoch: u64,
        server: Option<&ServerInventory>,
    ) -> Result<bool, &'static str> {
        let health = bot.health().map_err(|_| "Placement health unavailable")?;
        let held = as_position(
            bot.position()
                .map_err(|_| "Placement position unavailable")?,
        );
        if epoch != self.epoch
            || Instant::now() >= self.deadline
            || !health.is_finite()
            || health <= 0.0
            || health < self.health
            || !craft_stance(bot)
            || !crate::motion::holds_stance(&held, &as_position(self.position))
            || bot
                .world_name()
                .map_err(|_| "Placement dimension unavailable")?
                .0
                .to_string()
                != self.dimension
        {
            return Err("Placement cancelled: stance, damage, world or deadline changed");
        }
        self.health = health;
        let server = server.ok_or("Placement server inventory unknown")?;
        let hot = 36 + usize::from(self.hotbar);
        if let Phase::Confirm(revision) = self.phase {
            server
                .idle_view(bot)
                .ok_or("Placement inventory busy or unreconciled")?;
            if self.placed && server.slot_revisions[hot] > revision && server.slots == self.expected
            {
                let world = bot.world().map_err(|_| "Placement world unavailable")?;
                if world
                    .read()
                    .get_block_state(self.target)
                    .is_some_and(|s| BlockKind::from(s) == BlockKind::CraftingTable)
                {
                    return Ok(true);
                }
            }
            return Ok(false);
        }
        if matches!(self.phase, Phase::Aim) {
            if self
                .aim_observed
                .is_none_or(|at| at.elapsed() > Duration::from_millis(250))
            {
                return Err("Placement observation expired or changed while aiming");
            }
        } else if !current_site(bot, self.target) {
            return Err("Placement site or preserved exit changed");
        }
        if let Phase::AwaitSwap(revision, sent_at) = self.phase {
            if server.slot_revisions[self.source] > revision
                && server.slot_revisions[hot] > revision
                && server.slots == self.expected
                && server.idle_view(bot).is_some()
            {
                self.phase = Phase::Select;
            } else if sent_at.elapsed() >= Duration::from_secs(2) {
                return Err("Placement hotbar swap unconfirmed");
            }
            return Ok(false);
        }
        server
            .idle_view(bot)
            .ok_or("Placement inventory busy or unreconciled")?;
        if server.slots != self.expected {
            return Err("Placement inventory changed");
        }
        match self.phase {
            Phase::Stage if self.source != hot => {
                let world = bot.world().map_err(|_| "Placement registry unavailable")?;
                let carried_item =
                    HashedStack::from_item_stack(&server.carried, &world.read().registries);
                bot.ecs.write().trigger(SendGamePacketEvent::new(
                    bot.entity,
                    ServerboundContainerClick {
                        container_id: 0,
                        state_id: server.state_id,
                        slot_num: self.source as i16,
                        button_num: self.hotbar,
                        click_type: ClickType::Swap,
                        changed_slots: Default::default(),
                        carried_item,
                    },
                ));
                self.expected.swap(self.source, hot);
                self.phase = Phase::AwaitSwap(server.revision, Instant::now());
            }
            Phase::Stage => self.phase = Phase::Select,
            Phase::Select => {
                // Full site and exit were freshly scanned above. The next ECS
                // tick updates the crosshair; any intervening block packet or
                // a 250ms expiry invalidates that evidence before interaction.
                self.aim_observed = Some(Instant::now());
                bot.set_selected_hotbar_slot(self.hotbar);
                bot.look_at(Vec3::new(
                    self.support.x as f64 + 0.5,
                    self.support.y as f64 + 1.0,
                    self.support.z as f64 + 0.5,
                ));
                self.phase = Phase::Aim;
            }
            Phase::Aim => {
                if bot
                    .selected_hotbar_slot()
                    .map_err(|_| "Placement selection unavailable")?
                    != self.hotbar
                    || bot
                        .get_held_item()
                        .map_err(|_| "Placement held item unavailable")?
                        .kind()
                        .to_str()
                        != crate::placement::TABLE
                {
                    return Ok(false);
                }
                let HitResult::Block(hit) = bot
                    .hit_result()
                    .map_err(|_| "Placement crosshair unavailable")?
                else {
                    return Ok(false);
                };
                if hit.miss
                    || hit.inside
                    || hit.world_border
                    || hit.block_pos != self.support
                    || hit.direction != Direction::Up
                    || bot
                        .eye_position()
                        .map_err(|_| "Placement eye unavailable")?
                        .distance_to(hit.location)
                        > 3.0
                {
                    return Ok(false);
                }
                let mut ecs = bot.ecs.write();
                let seq = ecs
                    .get_mut::<BlockStatePredictionHandler>(bot.entity)
                    .ok_or("Placement prediction sequence unavailable")?
                    .start_predicting();
                ecs.trigger(SendGamePacketEvent::new(
                    bot.entity,
                    ServerboundUseItemOn {
                        hand: InteractionHand::MainHand,
                        block_hit: (&hit).into(),
                        seq,
                    },
                ));
                drop(ecs);
                let count = self.expected[hot].count() - 1;
                if count == 0 {
                    self.expected[hot] = ItemStack::Empty;
                } else if let ItemStack::Present(data) = &mut self.expected[hot] {
                    data.count = count;
                }
                self.sent = true;
                self.phase = Phase::Confirm(server.revision);
            }
            Phase::AwaitSwap(..) | Phase::Confirm(..) => unreachable!(),
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn place() -> Place {
        Place {
            candidate: Candidate {
                id: "table_site:1:0:0".into(),
                description: "test".into(),
                target: None,
                duration_ms: 5000,
                skill: None,
            },
            epoch: 5,
            attempt: 7,
            dimension: "minecraft:overworld".into(),
            target: BlockPos::new(1, 0, 0),
            support: BlockPos::new(1, -1, 0),
            source: 36,
            hotbar: 0,
            expected: vec![],
            phase: Phase::Aim,
            position: Vec3::new(0.5, 0.0, 0.5),
            health: 20.0,
            deadline: Instant::now() + Duration::from_secs(5),
            aim_observed: Some(Instant::now()),
            sent: false,
            placed: false,
        }
    }
    fn update(pos: BlockPos, kind: BlockKind) -> ClientboundGamePacket {
        ClientboundGamePacket::BlockUpdate(
            azalea::protocol::packets::game::ClientboundBlockUpdate {
                pos,
                block_state: kind.into(),
            },
        )
    }
    #[test]
    fn native_flat_ground_sites_preserve_another_observed_exit() {
        let position = Vec3::new(0.5, 0.0, 0.5);
        let sight = crate::perception::Sight::new(Vec3::new(0.5, 1.62, 0.5), -45.0, 25.0);
        let scene = |p: BlockPos| {
            Some(
                if p.y < 0 {
                    BlockKind::Stone
                } else {
                    BlockKind::Air
                }
                .into(),
            )
        };
        let frame = sight.scan(BlockPos::from(position), scene);
        let available = sites(position, &frame);
        assert!(available.iter().any(|s| s.name == "table_site:1:0:0"));
        assert!(available.len() >= 2);
        let changed = crate::perception::Sight::new(Vec3::new(0.5, 1.62, 0.5), -45.0, 25.0).scan(
            BlockPos::from(position),
            |p| {
                if p.z == 1 {
                    Some(BlockKind::Stone.into())
                } else {
                    scene(p)
                }
            },
        );
        assert!(
            sites(position, &changed).is_empty(),
            "one remaining site must be kept as exit"
        );
        let aimed = crate::perception::Sight::new(Vec3::new(0.5, 1.62, 0.5), -90.0, 58.0)
            .scan(BlockPos::from(position), scene);
        assert!(
            sites(position, &aimed).is_empty(),
            "reproduces why Select observation must bridge the look transition"
        );
    }
    #[test]
    fn any_intervening_block_packet_invalidates_aim_observation() {
        let mut active = place();
        active.receive(&update(BlockPos::new(12, 0, 12), BlockKind::Stone));
        assert!(active.aim_observed.is_none());
        assert!(!active.placed);
    }
    #[test]
    fn inbound_table_confirmation_requires_sent_attempt_and_exact_target() {
        let mut active = place();
        active.receive(&update(active.target, BlockKind::CraftingTable));
        assert!(!active.placed);
        active.sent = true;
        active.receive(&update(BlockPos::new(2, 0, 0), BlockKind::CraftingTable));
        assert!(!active.placed);
        active.receive(&update(active.target, BlockKind::CraftingTable));
        assert!(active.placed);
        active.receive(&update(active.target, BlockKind::Air));
        assert!(!active.placed);
        let cancelled = active.outcome(false, "cancelled");
        assert_eq!(cancelled.attempt_id, 7);
        assert_eq!(cancelled.world_epoch, 5);
        assert!(!cancelled.confirmed_by_server);
    }
    fn native_client() -> (
        Client,
        impl Sized,
        std::sync::Arc<
            std::sync::Mutex<Vec<azalea::protocol::packets::game::ServerboundGamePacket>>,
        >,
        ServerInventory,
    ) {
        use azalea::{
            core::position::ChunkPos,
            entity::{
                Attributes, LocalEntity, LookDirection, Physics, dimensions::EntityDimensions,
                inventory::Inventory, metadata::Health,
            },
            local_player::WorldHolder,
            world::{Chunk, PartialWorld, WorldName, Worlds},
        };
        let name = WorldName::new("minecraft:overworld");
        let mut worlds = Worlds::default();
        let world = worlds.get_or_insert(name.clone(), 384, -64, &Default::default());
        let chunk = world
            .write()
            .chunks
            .upsert(ChunkPos::new(0, 0), Chunk::default());
        for x in 0..16 {
            for z in 0..16 {
                world
                    .read()
                    .set_block_state(BlockPos::new(x, 63, z), BlockKind::Stone.into());
            }
        }
        let position = Vec3::new(8.5, 64.0, 8.5);
        let dimensions = EntityDimensions::new(0.6, 1.8).eye_height(1.62);
        let mut physics = Physics::new(&dimensions, position);
        physics.set_on_ground(true);
        let mut inventory = Inventory::default();
        *inventory.inventory_menu.slot_mut(36).unwrap() =
            ItemStack::new(azalea::registry::builtin::ItemKind::CraftingTable, 1);
        let items = inventory.inventory_menu.slots().to_vec();
        let mut ecs = azalea::ecs::world::World::new();
        ecs.insert_resource(worlds);
        ecs.init_resource::<azalea::ecs::message::Messages<azalea::bot::LookAtEvent>>();
        let packets = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = packets.clone();
        ecs.add_observer(
            move |event: azalea::ecs::prelude::On<SendGamePacketEvent>| {
                captured.lock().unwrap().push(event.packet.clone());
            },
        );
        let entity = ecs
            .spawn((
                azalea::entity::Position::new(position),
                dimensions,
                physics,
                LookDirection::new(-45.0, 25.0),
                Health(20.0),
                inventory,
                LocalEntity,
                Attributes::new(azalea::registry::builtin::EntityKind::Player),
                name,
                WorldHolder {
                    shared: world,
                    partial: std::sync::Arc::new(PartialWorld::new(2, None).into()),
                },
                BlockStatePredictionHandler::default(),
            ))
            .id();
        let bot = Client {
            entity,
            ecs: std::sync::Arc::new(ecs.into()),
        };
        let mut server = None;
        ServerInventory::receive(
            &mut server,
            &ClientboundGamePacket::ContainerSetContent(
                azalea::protocol::packets::game::ClientboundContainerSetContent {
                    container_id: 0,
                    state_id: 1,
                    items,
                    carried_item: ItemStack::Empty,
                },
            ),
            &bot,
        );
        (bot, chunk, packets, server.unwrap())
    }
    fn native_candidate() -> Candidate {
        Candidate {
            id: "table_site:9:64:8".into(),
            description: "test placement".into(),
            target: Some(Position {
                x: 9.5,
                y: 64.0,
                z: 8.5,
            }),
            duration_ms: 5000,
            skill: Some(WoodSkill::PlaceTable {
                support: Position {
                    x: 9.0,
                    y: 63.0,
                    z: 8.0,
                },
                source_slot: 36,
                hotbar_slot: 0,
            }),
        }
    }
    fn native_settings() -> Settings {
        Settings {
            mode: Mode::Live,
            wood_skills: true,
            ..Settings::default()
        }
    }
    fn apply_look_and_pick(bot: &Client) {
        use azalea::ecs::system::RunSystemOnce;
        let look = azalea::bot::direction_looking_at(
            bot.eye_position().unwrap(),
            Vec3::new(9.5, 64.0, 8.5),
        );
        let mut ecs = bot.ecs.write();
        ecs.entity_mut(bot.entity).insert(look);
        ecs.run_system_once(azalea::interact::pick::update_hit_result_component)
            .unwrap();
    }
    #[test]
    fn actual_place_phases_emit_once_and_require_both_server_evidences() {
        let (bot, _chunk, packets, server) = native_client();
        let mut active = Place::prepare(
            &bot,
            &native_candidate(),
            &native_settings(),
            3,
            19,
            &server,
        )
        .unwrap();
        assert!(!active.advance(&bot, 3, Some(&server)).unwrap()); // Stage -> Select
        assert!(!active.advance(&bot, 3, Some(&server)).unwrap()); // selection and look
        apply_look_and_pick(&bot);
        assert!(!active.advance(&bot, 3, Some(&server)).unwrap()); // actual hit -> packet
        assert!(active.sent);
        assert_eq!(
            packets
                .lock()
                .unwrap()
                .iter()
                .filter(|p| matches!(
                    p,
                    azalea::protocol::packets::game::ServerboundGamePacket::UseItemOn(_)
                ))
                .count(),
            1
        );
        assert!(!active.advance(&bot, 3, Some(&server)).unwrap()); // sending alone is not success
        let block = update(active.target, BlockKind::CraftingTable);
        active.receive(&block);
        bot.world()
            .unwrap()
            .read()
            .set_block_state(active.target, BlockKind::CraftingTable.into());
        assert!(!active.advance(&bot, 3, Some(&server)).unwrap()); // block alone is not success
        let packet = azalea::protocol::packets::game::ClientboundSetPlayerInventory {
            slot: 0,
            contents: ItemStack::Empty,
        };
        let mut server = Some(server);
        ServerInventory::receive(
            &mut server,
            &ClientboundGamePacket::SetPlayerInventory(packet.clone()),
            &bot,
        );
        super::super::apply_player_inventory(&bot, &packet);
        assert!(active.advance(&bot, 3, server.as_ref()).unwrap());
        let outcome = active.outcome(true, "confirmed");
        assert_eq!(outcome.attempt_id, 19);
        assert!(outcome.confirmed_by_server);
        assert_eq!(
            packets.lock().unwrap().len(),
            1,
            "no retry after awaiting confirmation"
        );
    }
    #[test]
    fn native_aim_cancellation_for_damage_epoch_expiry_and_changed_blocks_emits_nothing() {
        for reason in ["damage", "epoch", "expiry", "block"] {
            let (bot, _chunk, packets, server) = native_client();
            let mut active = Place::prepare(
                &bot,
                &native_candidate(),
                &native_settings(),
                3,
                19,
                &server,
            )
            .unwrap();
            active.advance(&bot, 3, Some(&server)).unwrap();
            active.advance(&bot, 3, Some(&server)).unwrap();
            apply_look_and_pick(&bot);
            match reason {
                "damage" => {
                    bot.ecs
                        .write()
                        .entity_mut(bot.entity)
                        .insert(azalea::entity::metadata::Health(19.0));
                }
                "expiry" => active.deadline = Instant::now(),
                "block" => active.receive(&update(active.target, BlockKind::Stone)),
                _ => {}
            }
            assert!(
                active
                    .advance(&bot, if reason == "epoch" { 4 } else { 3 }, Some(&server))
                    .is_err(),
                "{reason}"
            );
            assert!(packets.lock().unwrap().is_empty(), "{reason}");
        }
    }
}
