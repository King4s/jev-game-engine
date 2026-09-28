use azalea::{BlockPos, Vec3, block::BlockState, registry::builtin::BlockKind};
use jev_game_engine::{
    model::Observation,
    perception::{self, Optical, Sight},
};
use std::collections::BTreeSet;

fn camera() -> Sight {
    Sight::new(Vec3::new(0.5, 1.62, 0.5), 0.0, 0.0)
}
fn changed(kind: BlockKind, property: &str, value: &str) -> BlockState {
    let initial: BlockState = kind.into();
    let mut block = initial.to_trait().boxed();
    block.set_property(property, value).unwrap();
    block.as_block_state()
}

#[test]
fn wall_hides_resources_and_entities_glass_transmits_only_sight() {
    let target = BlockPos::new(0, 1, 4);
    let scene = |wall: BlockKind, p: BlockPos| {
        Some(BlockState::from(if p == target {
            BlockKind::OakLog
        } else if p.z == 2 {
            wall
        } else {
            BlockKind::Air
        }))
    };
    let opaque = camera().scan(BlockPos::new(0, 0, 0), |p| scene(BlockKind::Stone, p));
    assert!(opaque.get(target).is_none());
    assert!(
        opaque
            .iter()
            .all(|(_, b)| BlockKind::from(b) != BlockKind::OakLog)
    );
    assert!(
        !camera().visible(Vec3::new(0.5, 1.6, 4.5), false, |p| scene(
            BlockKind::Stone,
            p
        ))
    );
    let glass = camera().scan(BlockPos::new(0, 0, 0), |p| scene(BlockKind::Glass, p));
    assert_eq!(glass.get(target), Some(BlockKind::OakLog.into()));
    assert!(!perception::clear_stance(BlockKind::Glass.into()));
    let facts = perception::facts(BlockPos::new(0, 1, 2), BlockKind::Glass.into());
    assert_eq!(facts.optical, Optical::Clear);
    assert_eq!(facts.collision_boxes, vec![[0., 0., 0., 1., 1., 1.]]);
    assert_eq!(facts.interaction_boxes, vec![[0., 0., 0., 1., 1., 1.]]);
}

#[test]
fn changed_block_and_unload_replace_previous_frame_truth() {
    let target = BlockPos::new(0, 1, 3);
    let first = camera().scan(BlockPos::new(0, 0, 0), |p| {
        Some(if p == target {
            BlockKind::OakLog.into()
        } else {
            BlockState::AIR
        })
    });
    assert_eq!(first.get(target), Some(BlockKind::OakLog.into()));
    for kind in [BlockKind::Stone, BlockKind::Air, BlockKind::Water] {
        let next = camera().scan(BlockPos::new(0, 0, 0), |p| {
            Some(if p == target {
                kind.into()
            } else {
                BlockState::AIR
            })
        });
        assert_eq!(next.get(target), Some(kind.into()));
        assert!(
            next.iter()
                .all(|(_, b)| BlockKind::from(b) != BlockKind::OakLog)
        );
    }
    let unloaded = camera().scan(BlockPos::new(0, 0, 0), |_| None);
    assert!(unloaded.get(target).is_none());
    assert_eq!(unloaded.iter().count(), 0);
    // A new world cannot inherit old coordinates: the input is the sole state source.
    let other_world = camera().scan(BlockPos::new(0, 0, 0), |_| Some(BlockState::AIR));
    assert!(
        other_world
            .iter()
            .all(|(_, b)| BlockKind::from(b) == BlockKind::Air)
    );
}

#[test]
fn field_of_view_distance_and_invalid_coordinates_do_not_read_hidden_cells() {
    let sight = camera();
    assert!(
        sight
            .block(BlockPos::new(0, 1, -4), |_| panic!("behind camera queried"))
            .is_none()
    );
    assert!(
        sight
            .block(BlockPos::new(0, 1, 40), |_| panic!("beyond range queried"))
            .is_none()
    );
    assert!(
        !sight.visible(Vec3::new(f64::NAN, 1., 2.), false, |_| panic!(
            "NaN queried"
        ))
    );
    assert!(
        Sight::new(Vec3::new(0.5, 1.62, 0.5), 180., 0.)
            .block(BlockPos::new(0, 1, -4), |_| Some(BlockState::AIR))
            .is_some()
    );
}

#[test]
fn scan_exhaustion_preserves_nearby_cells_and_entity_budget_is_independent() {
    let sight = camera().with_budget(200);
    let frame = sight.scan(BlockPos::new(0, 0, 0), |_| Some(BlockState::AIR));
    assert!(sight.exhausted());
    assert_eq!(sight.reads(), 200);
    assert!(frame.get(BlockPos::new(0, 1, 1)).is_some());
    assert!(frame.get(BlockPos::new(0, 1, 15)).is_none());
    let entities = camera().with_budget(64);
    assert!(entities.visible(Vec3::new(0.5, 1.6, 2.5), false, |_| Some(BlockState::AIR)));
    assert!(entities.reads() <= 64);
}

#[test]
fn all_air_variants_are_clear_but_empty_hazard_shapes_are_not_stances() {
    for kind in [BlockKind::Air, BlockKind::CaveAir, BlockKind::VoidAir] {
        assert!(perception::is_air(kind.into()));
        assert!(perception::clear_stance(kind.into()));
        assert_eq!(perception::optical(kind.into()), Optical::Clear);
    }
    for kind in [
        BlockKind::Water,
        BlockKind::Lava,
        BlockKind::Fire,
        BlockKind::PowderSnow,
        BlockKind::SweetBerryBush,
    ] {
        assert!(!perception::clear_stance(kind.into()));
    }
}

#[test]
fn every_water_and_lava_level_requires_fluid_context_including_falling_levels() {
    for kind in [BlockKind::Water, BlockKind::Lava] {
        for level in 0..=15 {
            let state = changed(kind, "level", &level.to_string());
            assert_eq!(
                perception::optical(state),
                Optical::FluidContextRequired,
                "{kind:?} level {level}"
            );
        }
    }
    let wet = changed(BlockKind::GlassPane, "waterlogged", "true");
    let facts = perception::facts(BlockPos::new(0, 0, 0), wet);
    assert_eq!(facts.fluid, "water");
    assert_eq!(facts.optical, Optical::FluidContextRequired);
    assert!(!perception::clear_stance(wet));
}

#[test]
fn block_facts_follow_actual_door_slab_and_waterlogged_states() {
    let p = BlockPos::new(0, 0, 0);
    let closed = perception::facts(p, changed(BlockKind::OakDoor, "open", "false"));
    let open = perception::facts(p, changed(BlockKind::OakDoor, "open", "true"));
    assert_eq!(closed.properties["open"], "false");
    assert_eq!(open.properties["open"], "true");
    assert_ne!(closed.collision_boxes, open.collision_boxes);
    let bottom = perception::facts(p, changed(BlockKind::OakSlab, "type", "bottom"));
    let top = perception::facts(p, changed(BlockKind::OakSlab, "type", "top"));
    assert_ne!(bottom.collision_boxes, top.collision_boxes);
    assert!(!perception::full_support(changed(
        BlockKind::OakSlab,
        "waterlogged",
        "true"
    )));
}

#[test]
fn every_catalogue_state_matches_native_identity_property_domains_and_runtime_facts() {
    let audit: serde_json::Value =
        serde_json::from_str(include_str!("../docs/block-property-audit-26.2.json")).unwrap();
    let blocks = audit["blocks"].as_array().unwrap();
    assert_eq!(blocks.len(), 1196);
    assert_eq!(u32::from(BlockState::MAX_STATE) + 1, 32366);
    let mut seen = BTreeSet::new();
    for block in blocks {
        let first = block["state_id_range_26_2"][0].as_u64().unwrap() as u32;
        let last = block["state_id_range_26_2"][1].as_u64().unwrap() as u32;
        assert_eq!(
            u64::from(last - first + 1),
            block["state_count"].as_u64().unwrap()
        );
        for id in first..=last {
            assert!(seen.insert(id), "duplicate state {id}");
            let facts =
                perception::facts(BlockPos::new(3, 4, 5), BlockState::try_from(id).unwrap());
            assert_eq!(facts.name, block["id"].as_str().unwrap(), "state {id}");
            assert_eq!(usize::from(facts.state_id_26_2), id as usize);
            let properties = block["properties"].as_array().unwrap();
            assert_eq!(facts.properties.len(), properties.len());
            for p in properties {
                let value = &facts.properties[p["name"].as_str().unwrap()];
                assert!(
                    p["values"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|v| v.as_str() == Some(value.as_str())),
                    "state {id}"
                );
            }
            assert!(!facts.unresolved.is_empty());
            assert!(
                facts
                    .collision_boxes
                    .iter()
                    .chain(&facts.interaction_boxes)
                    .flatten()
                    .all(|n| n.is_finite())
            );
        }
    }
    assert_eq!(seen.len(), 32366);
}

#[test]
fn legacy_observation_and_provider_preserve_visible_state_facts() {
    let mut observation = Observation::default();
    let mut legacy = serde_json::to_value(&observation).unwrap();
    legacy.as_object_mut().unwrap().remove("block_facts");
    assert!(
        serde_json::from_value::<Observation>(legacy)
            .unwrap()
            .block_facts
            .is_empty()
    );
    observation.block_facts.push(perception::facts(
        BlockPos::new(1, 2, 3),
        changed(BlockKind::OakDoor, "open", "true"),
    ));
    let request = jev_game_engine::provider::request_body(&observation, &[], "");
    assert_eq!(
        request["state"]["observation"]["block_facts"][0]["properties"]["open"],
        "true"
    );
    let restored: Observation =
        serde_json::from_value(serde_json::to_value(observation).unwrap()).unwrap();
    assert_eq!(restored.block_facts[0].name, "minecraft:oak_door");
}
