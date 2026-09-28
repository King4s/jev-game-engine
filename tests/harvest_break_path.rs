//! In-process evidence for the wood harvest, pinned against the same pinned Azalea client the
//! live adapter drives.
//!
//! The live recording `runs/level1-live/live-dt.json` shows a gather candidate accepted, the
//! bot walking to its verified stance, and then nothing: the block was never removed and the
//! inventory never changed. From the world alone those six attempts are indistinguishable,
//! because a swallowed break and a refused break both leave the target exactly as it was.
//! These tests record what the client itself does with the executor's exact request:
//!
//! 1. the request the executor queues is well formed: it sends `StartDestroyBlock` for the
//!    whitelisted target and the client predicts the block away in creative;
//! 2. a server that answers with a block-changed acknowledgement -- vanilla does that after
//!    every handled action, whether or not it removed the block -- puts the predicted block
//!    back, which is the state `minecraft::mining_decision` must read as "ask again";
//! 3. a client whose own game mode forbids the break with an empty hand consumes the request
//!    and sends nothing at all, so nothing in the world can show the executor why it is idle.
//!
//! What these tests cannot settle is which of 2 and 3 the live server produced: that needs the
//! live client and the operator's server. Both are handled by the same executor path.
use azalea::bot::direction_looking_at;
use azalea::core::direction::Direction;
use azalea::core::game_type::GameMode;
use azalea::core::position::ChunkPos;
use azalea::entity::LookDirection;
use azalea::mining::MiningQueued;
use azalea::protocol::common::movements::{PositionMoveRotation, RelativeMovements};
use azalea::protocol::packets::ConnectionProtocol;
use azalea::protocol::packets::game::{
    ClientboundBlockChangedAck, ClientboundBlockUpdate, ClientboundPlayerPosition,
    ServerboundGamePacket,
};
use azalea::registry::builtin::BlockKind;
use azalea::test_utils::prelude::*;
use azalea::{BlockPos, Vec3};

/// The live notch, scaled to the test origin: the bot's feet at (0,2,3), the floor under it,
/// the wall east of it two blocks high, and the harvestable block on top of that wall. The
/// target is two blocks above the feet and one to the side, exactly like the live candidates
/// `gather_13_68_6` and `gather_12_68_5` next to a bot whose feet were at y=66.
fn target() -> BlockPos {
    BlockPos::new(1, 4, 3)
}

fn scene() -> Vec<(BlockPos, BlockKind)> {
    vec![
        (BlockPos::new(0, 1, 3), BlockKind::GrassBlock),
        (BlockPos::new(0, 2, 2), BlockKind::Dirt),
        (BlockPos::new(0, 3, 2), BlockKind::Dirt),
        (BlockPos::new(0, 4, 2), BlockKind::GrassBlock),
        (BlockPos::new(1, 2, 3), BlockKind::Dirt),
        (BlockPos::new(1, 3, 3), BlockKind::Dirt),
        (target(), BlockKind::GrassBlock),
    ]
}

fn setup(mode: GameMode) -> (Simulation, BlockPos) {
    let mut simulation = Simulation::new(ConnectionProtocol::Game);
    simulation.receive_packet(default_login_packet());
    simulation.receive_packet(make_basic_empty_chunk(ChunkPos::new(0, 0), (384 + 64) / 16));
    simulation.tick();
    for (pos, block) in scene() {
        simulation.receive_packet(ClientboundBlockUpdate {
            pos,
            block_state: block.into(),
        });
    }
    simulation.receive_packet(ClientboundPlayerPosition {
        id: 1,
        change: PositionMoveRotation {
            pos: Vec3::new(0.5, 2.0, 3.5),
            delta: Vec3::ZERO,
            look_direction: LookDirection::default(),
        },
        relative: RelativeMovements::all_absolute(),
    });
    simulation.tick();
    simulation.tick();
    let target = target();
    // Face the block the way the executor does before it queues a break.
    let eye = simulation.component::<azalea::entity::Position>().up(1.62);
    let look = direction_looking_at(eye, target.center());
    simulation.with_component_mut::<LookDirection>(|dir| dir.update(look));
    simulation.with_component_mut::<GameMode>(|current| *current = mode);
    simulation.tick();
    (simulation, target)
}

/// The executor's own request, byte for byte: `src/minecraft.rs` `queue_mining`.
fn queue_break(simulation: &mut Simulation, target: BlockPos) {
    simulation
        .app
        .world_mut()
        .entity_mut(simulation.entity)
        .insert(MiningQueued {
            position: target,
            direction: Direction::Down,
            force: true,
        });
}

fn break_packets(sent: &SentPackets, target: BlockPos) -> Vec<String> {
    let mut packets = Vec::new();
    while let Some(packet) = sent.next() {
        if let ServerboundGamePacket::PlayerAction(action) = &packet
            && action.pos == target
        {
            packets.push(format!("{:?}", action.action));
        }
    }
    packets
}

#[test]
fn the_queued_break_sends_the_break_packet_and_the_client_predicts_the_block_away() {
    let _lock = init();
    let (mut simulation, target) = setup(GameMode::Creative);
    let sent = SentPackets::new(&mut simulation);
    assert_eq!(
        simulation.get_block_state(target),
        Some(BlockKind::GrassBlock.into())
    );

    queue_break(&mut simulation, target);
    for _ in 0..3 {
        simulation.tick();
    }

    // The request is consumed, and the block is gone from the client's own world.
    assert!(simulation.get_component::<MiningQueued>().is_none());
    assert_eq!(
        simulation.get_block_state(target),
        Some(BlockKind::Air.into())
    );
    // And the server was told to break exactly this block, in creative where the break is
    // instant. A block that survives all of this was not refused by this code.
    assert_eq!(break_packets(&sent, target), vec!["StartDestroyBlock"]);
}

#[test]
fn a_refused_break_is_rolled_back_to_the_unchanged_block_the_executor_must_retry() {
    let _lock = init();
    let (mut simulation, target) = setup(GameMode::Creative);
    let sent = SentPackets::new(&mut simulation);

    queue_break(&mut simulation, target);
    for _ in 0..3 {
        simulation.tick();
    }
    assert_eq!(
        simulation.get_block_state(target),
        Some(BlockKind::Air.into())
    );

    // Vanilla acknowledges the client's action sequence after handling it, whether or not the
    // action changed anything. The pinned client rolls its own prediction back when the
    // acknowledged sequence has no matching block update -- its own test
    // `azalea-client/tests/simulation/mine_block_rollback.rs` pins the same behaviour.
    simulation.receive_packet(ClientboundBlockChangedAck { seq: 1 });
    simulation.tick();

    assert_eq!(
        simulation.get_block_state(target),
        Some(BlockKind::GrassBlock.into())
    );
    assert!(
        simulation
            .get_component::<azalea::mining::Mining>()
            .is_none()
    );
    // This is the live state of every failed attempt: the expected block is still there, no
    // mining is in flight, and the queue is empty. Before the fix the executor idled here for
    // the whole 10 s action deadline and then reported a deadline that named no cause.
    assert_eq!(break_packets(&sent, target), vec!["StartDestroyBlock"]);
}

#[test]
fn a_client_that_may_not_break_the_block_consumes_the_request_without_any_packet() {
    let _lock = init();
    // An empty inventory, and a game mode whose own interaction rule forbids breaking with an
    // empty hand: `azalea-client/src/plugins/interact/mod.rs` `check_is_interaction_restricted`
    // answers true here, and `handle_mining_queued` returns before it sends anything.
    let (mut simulation, target) = setup(GameMode::Adventure);
    let sent = SentPackets::new(&mut simulation);

    queue_break(&mut simulation, target);
    for _ in 0..3 {
        simulation.tick();
    }

    // The request is gone, no packet was sent, nothing is in flight, and the block is
    // untouched: the executor cannot see any of this in the world, which is why it must
    // notice that its own request produced no break and say so.
    assert!(simulation.get_component::<MiningQueued>().is_none());
    assert!(
        simulation
            .get_component::<azalea::mining::Mining>()
            .is_none()
    );
    assert_eq!(
        simulation.get_block_state(target),
        Some(BlockKind::GrassBlock.into())
    );
    assert!(break_packets(&sent, target).is_empty());
}
