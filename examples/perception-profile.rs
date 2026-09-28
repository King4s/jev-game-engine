//! Deterministic scan-work diagnostic, not a live Minecraft benchmark.
use azalea::{BlockPos, Vec3, block::BlockState, registry::builtin::BlockKind};
use jev_game_engine::{perception::Sight, resources::block_scan_offsets};
use std::{cell::Cell, time::Instant};
fn main() {
    let floor = !std::env::args().any(|a| a == "--open-air");
    let shared = std::env::args().any(|a| a == "--shared");
    for run in 0..5 {
        let reads = Cell::new(0usize);
        let lookup = |p: BlockPos| {
            reads.set(reads.get() + 1);
            Some(BlockState::from(if p == BlockPos::new(0, 1, 4) {
                BlockKind::OakLog
            } else if floor && p.y < 0 {
                BlockKind::Stone
            } else {
                BlockKind::Air
            }))
        };
        let sight = Sight::new(Vec3::new(0.5, 1.62, 0.5), 0.0, 0.0);
        let started = Instant::now();
        let (visible_log, near) = if shared {
            let frame = sight.scan(BlockPos::new(0, 0, 0), lookup);
            (
                frame
                    .iter()
                    .any(|(_, s)| BlockKind::from(s) == BlockKind::OakLog),
                frame.get(BlockPos::new(0, 1, 1)).is_some(),
            )
        } else {
            for x in -12..=12 {
                for z in -12..=12 {
                    for y in -8..=8 {
                        let _ = sight.block(BlockPos::new(x, y, z), lookup);
                    }
                }
            }
            let mut visible_log = false;
            for (x, y, z) in block_scan_offsets() {
                if let Some(block) = sight.block(BlockPos::new(x, y, z), lookup) {
                    visible_log |= BlockKind::from(block) == BlockKind::OakLog;
                }
            }
            (
                visible_log,
                sight.block(BlockPos::new(0, 1, 1), lookup).is_some(),
            )
        };
        println!(
            "run={run} us={} reads={} exhausted={} log={visible_log} near={near}",
            started.elapsed().as_micros(),
            reads.get(),
            sight.exhausted()
        );
    }
}
