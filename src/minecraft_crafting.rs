//! Non-predicting inventory clicks and a server-only inventory mirror.
//! Pinned source: https://github.com/azalea-rs/azalea/blob/b65fa8cf1bb957976cefa926b9b500d44767d806/azalea-client/src/plugins/inventory/mod.rs
//! ContainerHandle::click simulates locally, so use its packet primitive without
//! prediction. The server receives the unchanged (pre-click) cursor hash and no
//! predicted slot changes; subsequent packets, never ECS predictions, prove changes.
use crate::model::{Candidate, CraftingSlot, Mode, Settings, SkillOutcome};
use azalea::{
    BlockPos, Client, Vec3,
    container::ContainerHandle,
    entity::inventory::Inventory,
    inventory::ItemStack,
    inventory::operations::ClickType,
    packet::game::SendGamePacketEvent,
    protocol::packets::game::{
        ClientboundGamePacket, ServerboundContainerClick, s_container_click::HashedStack,
    },
};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

#[derive(Clone)]
pub(super) struct ServerInventory {
    pub(super) slots: Vec<ItemStack>,
    pub(super) carried: ItemStack,
    pub(super) state_id: u32,
    pub(super) revision: u64,
    pub(super) slot_revisions: Vec<u64>,
    cursor_revision: u64,
}

impl ServerInventory {
    pub(super) fn receive(state: &mut Option<Self>, packet: &ClientboundGamePacket, bot: &Client) {
        if let ClientboundGamePacket::ContainerSetContent(p) = packet
            && p.container_id == 0
            && p.items.len() == 46
        {
            let revision = state.as_ref().map_or(1, |s| s.revision.wrapping_add(1));
            *state = Some(Self {
                slots: p.items.clone(),
                carried: p.carried_item.clone(),
                state_id: p.state_id,
                revision,
                slot_revisions: vec![revision; 46],
                cursor_revision: revision,
            });
            // Pinned handler copies slots but omits carried/state_id for content packets.
            if let Some(mut inv) = bot.ecs.write().get_mut::<Inventory>(bot.entity)
                && inv.id == 0
            {
                inv.carried = p.carried_item.clone();
                inv.state_id = p.state_id;
            }
            return;
        }
        let Some(s) = state.as_mut() else {
            return;
        };
        s.revision = s.revision.wrapping_add(1);
        match packet {
            ClientboundGamePacket::ContainerSetSlot(p)
                if p.container_id == 0 && (p.slot as usize) < s.slots.len() =>
            {
                s.slots[p.slot as usize] = p.item_stack.clone();
                s.slot_revisions[p.slot as usize] = s.revision;
                s.state_id = p.state_id;
            }
            ClientboundGamePacket::ContainerSetSlot(p) if p.container_id == -1 => {
                s.carried = p.item_stack.clone();
                s.cursor_revision = s.revision;
            }
            ClientboundGamePacket::SetCursorItem(p) => {
                s.carried = p.contents.clone();
                s.cursor_revision = s.revision;
                // Pinned set_cursor_item handler is a no-op. Apply only inbound data.
                if let Some(mut inv) = bot.ecs.write().get_mut::<Inventory>(bot.entity) {
                    inv.carried = p.contents.clone();
                }
            }
            ClientboundGamePacket::SetPlayerInventory(p) => {
                let slot = match p.slot {
                    0..=8 => p.slot as usize + 36,
                    9..=35 => p.slot as usize,
                    _ => return,
                };
                s.slots[slot] = p.contents.clone();
                s.slot_revisions[slot] = s.revision;
            }
            ClientboundGamePacket::ContainerSetSlot(p) if p.container_id == -2 => {
                *state = None;
            }
            ClientboundGamePacket::OpenScreen(_) | ClientboundGamePacket::ContainerClose(_) => {
                *state = None;
            }
            _ => {}
        }
    }

    pub(super) fn idle_view(&self, bot: &Client) -> Option<Vec<CraftingSlot>> {
        let inv = bot.component::<Inventory>().ok()?;
        if inv.id != 0
            || inv.container_menu.is_some()
            || !inv.carried.is_empty()
            || !self.carried.is_empty()
            || self.slots[..5].iter().any(|s| !s.is_empty())
            || inv.inventory_menu.slots() != self.slots
        {
            return None;
        }
        Some(
            (9..45)
                .map(|i| CraftingSlot {
                    slot: i as u16,
                    item: self.slots[i].kind().to_str().into(),
                    count: self.slots[i].count().max(0) as u32,
                })
                .collect(),
        )
    }
}

struct Click {
    slot: usize,
    button: u8,
    before: Vec<ItemStack>,
    before_cursor: ItemStack,
    after: Vec<ItemStack>,
    after_cursor: ItemStack,
}

pub(super) struct Craft {
    handle: ContainerHandle,
    attempt_id: u64,
    id: String,
    epoch: u64,
    dimension: String,
    position: Vec3,
    health: f32,
    deadline: Instant,
    output: String,
    output_count: u32,
    destination: usize,
    clicks: VecDeque<Click>,
    sent: Option<(u64, Instant)>,
    taking_output: bool,
    confirmed_slots: Vec<ItemStack>,
}

fn resized(item: &ItemStack, count: i32) -> ItemStack {
    if count <= 0 {
        return ItemStack::Empty;
    }
    let mut item = item.clone();
    if let ItemStack::Present(data) = &mut item {
        data.count = count;
    }
    item
}

// Exclude only the server-computed recipe preview; it can update between clicks.
fn same_owned(a: &[ItemStack], b: &[ItemStack]) -> bool {
    a.get(1..) == b.get(1..)
}

impl Craft {
    pub(super) fn prepare(
        bot: &Client,
        candidate: &Candidate,
        settings: &Settings,
        epoch: u64,
        attempt_id: u64,
        server: &ServerInventory,
    ) -> Result<Self, ()> {
        let skill = candidate.skill.as_ref().ok_or(())?;
        let (ingredients, output, output_count) = crate::wood::recipe(skill).ok_or(())?;
        let health = bot.health().map_err(|_| ())?;
        if !settings.wood_skills
            || !craft_stance(bot)
            || settings.mode != Mode::Live
            || !health.is_finite()
            || health <= 0.0
            || candidate.target.is_some()
            || !(1..=10_000).contains(&candidate.duration_ms)
            || bot.world_name().map_err(|_| ())?.0.to_string() != settings.allowed_dimension
        {
            return Err(());
        }
        server.idle_view(bot).ok_or(())?;
        let destination = (9..45).find(|i| server.slots[*i].is_empty()).ok_or(())?;
        let mut slots = server.slots.clone();
        let mut cursor = server.carried.clone();
        let mut clicks = VecDeque::new();
        for (grid, ingredient) in ingredients.iter().enumerate() {
            let source = (9..45)
                .find(|i| !slots[*i].is_empty() && slots[*i].kind().to_str() == ingredient)
                .ok_or(())?;
            // Pickup full stack, deposit exactly one, return remainder. No shift-click
            // that could consume multiple recipes or fill the grid accidentally.
            Self::plan_pickup(&mut clicks, &mut slots, &mut cursor, source, false);
            Self::plan_pickup(&mut clicks, &mut slots, &mut cursor, grid + 1, true);
            if !cursor.is_empty() {
                Self::plan_pickup(&mut clicks, &mut slots, &mut cursor, source, false);
            }
        }
        let position = bot.position().map_err(|_| ())?;
        let handle = bot.open_inventory().map_err(|_| ())?.ok_or(())?;
        Ok(Self {
            handle,
            attempt_id,
            id: candidate.id.clone(),
            epoch,
            dimension: settings.allowed_dimension.clone(),
            position,
            health,
            deadline: Instant::now() + Duration::from_millis(candidate.duration_ms),
            output,
            output_count,
            destination,
            clicks,
            sent: None,
            taking_output: false,
            confirmed_slots: server.slots.clone(),
        })
    }

    fn plan_pickup(
        clicks: &mut VecDeque<Click>,
        slots: &mut [ItemStack],
        cursor: &mut ItemStack,
        slot: usize,
        right: bool,
    ) {
        let before = slots.to_vec();
        let before_cursor = cursor.clone();
        if cursor.is_empty() {
            *cursor = slots[slot].clone();
            slots[slot] = ItemStack::Empty;
        } else if right {
            slots[slot] = resized(cursor, 1);
            *cursor = resized(cursor, cursor.count() - 1);
        } else {
            slots[slot] = cursor.clone();
            *cursor = ItemStack::Empty;
        }
        clicks.push_back(Click {
            slot,
            button: u8::from(right),
            before,
            before_cursor,
            after: slots.to_vec(),
            after_cursor: cursor.clone(),
        });
    }

    pub(super) fn outcome(&self, success: bool, reason: &str) -> SkillOutcome {
        SkillOutcome {
            attempt_id: self.attempt_id,
            candidate_id: self.id.clone(),
            world_epoch: self.epoch,
            success,
            confirmed_by_server: success,
            message: reason.into(),
        }
    }

    pub(super) fn advance(
        &mut self,
        bot: &Client,
        epoch: u64,
        server: Option<&ServerInventory>,
    ) -> Result<bool, &'static str> {
        let health = bot.health().map_err(|_| "Craft health unavailable")?;
        if epoch != self.epoch
            || !craft_stance(bot)
            || Instant::now() >= self.deadline
            || bot
                .world_name()
                .map_err(|_| "Craft world unavailable")?
                .0
                .to_string()
                != self.dimension
            || !health.is_finite()
            || health <= 0.0
            || health < self.health
            || bot
                .position()
                .map_err(|_| "Craft position unavailable")?
                .distance_to(self.position)
                > 0.6
        {
            return Err(
                "Craft cancelled: deadline, damage, position or world changed; staged inputs may remain",
            );
        }
        let inv = bot
            .component::<Inventory>()
            .map_err(|_| "Craft inventory unavailable")?;
        if inv.id != 0 || inv.container_menu.is_some() {
            return Err("Craft interrupted by another menu");
        }
        let server = server.ok_or("Craft server inventory unknown")?;
        if let Some((revision, sent_at)) = self.sent {
            let click = self.clicks.front().ok_or("Craft sequence invalid")?;
            let changed_confirmed = (1..46)
                .filter(|i| click.before[*i] != click.after[*i])
                .all(|i| server.slot_revisions[i] > revision && server.slots[i] == click.after[i]);
            let cursor_confirmed = click.before_cursor == click.after_cursor
                || (server.cursor_revision > revision && server.carried == click.after_cursor);
            if changed_confirmed && cursor_confirmed && same_owned(&server.slots, &click.after) {
                self.confirmed_slots = click.after.clone();
                self.clicks.pop_front();
                self.sent = None;
                // Bound one click per game tick, including after confirmation.
                return Ok(false);
            }
            if sent_at.elapsed() >= Duration::from_secs(2) {
                return Err("Craft click not confirmed by server; partial inventory retained");
            }
            return Ok(false);
        }
        if self.clicks.is_empty() {
            if !same_owned(&server.slots, &self.confirmed_slots) {
                return Err("Craft inventory changed after the last confirmed click");
            }
            if self.taking_output {
                if !server.carried.is_empty() || server.slots[..5].iter().any(|s| !s.is_empty()) {
                    return Err("Craft output received but grid/cursor did not clear");
                }
                // Entire recipe expected storage change is independently checked by engine.
                return Ok(true);
            }
            let preview = &server.slots[0];
            if preview.kind().to_str() != self.output || preview.count() != self.output_count as i32
            {
                return Ok(false); // Server recipe authority; bounded by the attempt deadline.
            }
            if !server.carried.is_empty() || !server.slots[self.destination].is_empty() {
                return Err("Craft output space changed");
            }
            let mut after = server.slots.clone();
            for slot in &mut after[..5] {
                *slot = ItemStack::Empty;
            }
            self.clicks.push_back(Click {
                slot: 0,
                button: 0,
                before: server.slots.clone(),
                before_cursor: ItemStack::Empty,
                after: after.clone(),
                after_cursor: preview.clone(),
            });
            let mut cursor = preview.clone();
            Self::plan_pickup(
                &mut self.clicks,
                &mut after,
                &mut cursor,
                self.destination,
                false,
            );
            self.taking_output = true;
        }
        let click = self.clicks.front().ok_or("Craft sequence missing")?;
        if !same_owned(&server.slots, &click.before)
            || server.carried != click.before_cursor
            || inv.carried != server.carried
            || inv.inventory_menu.slots() != server.slots
        {
            return Err("Craft prerequisites changed before click");
        }
        // Revalidate result at the actual pickup tick, never trust an older preview.
        if click.slot == 0 && server.slots[0] != click.before[0] {
            return Err("Craft recipe preview changed");
        }
        // Client::component returns an ECS MappedRwLockReadGuard, not a copy.
        // Release it before world/component access and especially ecs.write().
        drop(inv);
        let world = bot.world().map_err(|_| "Craft registry unavailable")?;
        let carried_item = HashedStack::from_item_stack(&server.carried, &world.read().registries);
        // Do not hold the world lock when acquiring ECS.
        bot.ecs.write().trigger(SendGamePacketEvent::new(
            bot.entity,
            ServerboundContainerClick {
                container_id: self.handle.id(),
                state_id: server.state_id,
                slot_num: click.slot as i16,
                button_num: click.button,
                click_type: ClickType::Pickup,
                changed_slots: Default::default(),
                carried_item,
            },
        ));
        self.sent = Some((server.revision, Instant::now()));
        Ok(false)
    }
}

/// Inventory work requires current grounded body/support contact. This uses the
/// same conservative full-cell stance policy as walking, not remembered terrain.
pub(super) fn craft_stance(bot: &Client) -> bool {
    let Ok(position) = bot.position() else {
        return false;
    };
    if !crate::motion::stance_aligned(&crate::model::Position {
        x: position.x,
        y: position.y,
        z: position.z,
    }) || bot
        .component::<azalea::entity::Physics>()
        .ok()
        .is_none_or(|physics| !physics.on_ground())
    {
        return false;
    }
    let Ok(world) = bot.world() else {
        return false;
    };
    super::standable(&world.read(), BlockPos::from(position))
}

#[cfg(test)]
mod tests {
    use super::*;
    use azalea::protocol::packets::game::{
        ClientboundContainerSetContent, ClientboundSetCursorItem, ClientboundSetPlayerInventory,
    };
    use azalea::registry::builtin::ItemKind;
    fn client() -> Client {
        let mut ecs = azalea::ecs::world::World::new();
        let entity = ecs.spawn(Inventory::default()).id();
        Client {
            entity,
            ecs: std::sync::Arc::new(ecs.into()),
        }
    }
    fn content(items: Vec<ItemStack>) -> ClientboundGamePacket {
        ClientboundGamePacket::ContainerSetContent(ClientboundContainerSetContent {
            container_id: 0,
            state_id: 17,
            items,
            carried_item: ItemStack::Empty,
        })
    }
    #[test]
    fn inventory_mirror_requires_full_server_baseline_and_reconciled_client_slots() {
        let bot = client();
        let mut mirror = None;
        ServerInventory::receive(&mut mirror, &content(vec![ItemStack::Empty; 45]), &bot);
        assert!(mirror.is_none());
        ServerInventory::receive(&mut mirror, &content(vec![ItemStack::Empty; 46]), &bot);
        assert_eq!(mirror.as_ref().unwrap().idle_view(&bot).unwrap().len(), 36);
        assert_eq!(bot.component::<Inventory>().unwrap().state_id, 17);
        *bot.ecs
            .write()
            .get_mut::<Inventory>(bot.entity)
            .unwrap()
            .inventory_menu
            .slot_mut(9)
            .unwrap() = ItemStack::new(ItemKind::OakLog, 1);
        assert!(
            mirror.as_ref().unwrap().idle_view(&bot).is_none(),
            "local prediction cannot become confirmed inventory"
        );
    }
    #[test]
    fn incoming_hotbar_and_cursor_packets_advance_only_their_confirmed_evidence() {
        let bot = client();
        let mut mirror = None;
        ServerInventory::receive(&mut mirror, &content(vec![ItemStack::Empty; 46]), &bot);
        let baseline = mirror.as_ref().unwrap().revision;
        let packet = ClientboundSetPlayerInventory {
            slot: 0,
            contents: ItemStack::new(ItemKind::CraftingTable, 1),
        };
        ServerInventory::receive(
            &mut mirror,
            &ClientboundGamePacket::SetPlayerInventory(packet.clone()),
            &bot,
        );
        let current = mirror.as_ref().unwrap();
        assert_eq!(current.slots[36].count(), 1);
        assert!(current.slot_revisions[36] > baseline);
        assert_eq!(current.slot_revisions[9], baseline);
        assert!(current.idle_view(&bot).is_none());
        super::super::apply_player_inventory(&bot, &packet);
        assert!(mirror.as_ref().unwrap().idle_view(&bot).is_some());
        let cursor = ItemStack::new(ItemKind::OakLog, 1);
        ServerInventory::receive(
            &mut mirror,
            &ClientboundGamePacket::SetCursorItem(ClientboundSetCursorItem {
                contents: cursor.clone(),
            }),
            &bot,
        );
        assert_eq!(bot.component::<Inventory>().unwrap().carried, cursor);
        assert!(mirror.as_ref().unwrap().cursor_revision > baseline);
        assert!(mirror.as_ref().unwrap().idle_view(&bot).is_none());
    }
}
