use jev_game_engine::{
    model::{CraftingSlot, Landmark, Mode, Observation, Position, Settings, WoodSkill},
    placement,
};
fn ready() -> (Observation, Settings) {
    let settings = Settings {
        wood_skills: true,
        mode: Mode::Live,
        ..Settings::default()
    };
    let target = Position {
        x: 1.5,
        y: 0.0,
        z: 0.5,
    };
    let observation = Observation {
        connected: true,
        health: 20.0,
        food: 20.0,
        dimension: Some(settings.allowed_dimension.clone()),
        position: Position {
            x: 0.5,
            y: 0.0,
            z: 0.5,
        },
        blocks: vec![Landmark {
            name: placement::site_name(&target),
            position: target,
        }],
        crafting_inventory: Some(
            (9..=44)
                .map(|slot| CraftingSlot {
                    slot,
                    item: if slot == 9 {
                        placement::TABLE
                    } else {
                        "minecraft:air"
                    }
                    .into(),
                    count: u32::from(slot == 9),
                })
                .collect(),
        ),
        ..Observation::default()
    };
    (observation, settings)
}
#[test]
fn bound_table_requires_reobserved_site_and_same_inventory_slot() {
    let (mut observation, settings) = ready();
    let candidate = placement::candidates(&observation, &settings).remove(0);
    assert!(matches!(
        candidate.skill,
        Some(WoodSkill::PlaceTable {
            source_slot: 9,
            hotbar_slot: 0,
            ..
        })
    ));
    assert!(placement::is_current(&candidate, &observation, &settings));
    observation.crafting_inventory.as_mut().unwrap()[0].count = 0;
    assert!(!placement::is_current(&candidate, &observation, &settings));
    observation.crafting_inventory.as_mut().unwrap()[0].count = 1;
    observation.blocks.clear();
    assert!(!placement::is_current(&candidate, &observation, &settings));
}
#[test]
fn full_hotbar_blocks_storage_table_but_owned_hotbar_table_remains_usable() {
    let (mut observation, settings) = ready();
    for slot in observation
        .crafting_inventory
        .as_mut()
        .unwrap()
        .iter_mut()
        .filter(|s| s.slot >= 36)
    {
        slot.item = "minecraft:stone".into();
        slot.count = 64;
    }
    assert!(placement::candidates(&observation, &settings).is_empty());
    let table = observation
        .crafting_inventory
        .as_mut()
        .unwrap()
        .iter_mut()
        .find(|s| s.slot == 40)
        .unwrap();
    table.item = placement::TABLE.into();
    table.count = 1;
    assert!(matches!(
        placement::candidates(&observation, &settings)[0].skill,
        Some(WoodSkill::PlaceTable {
            source_slot: 40,
            hotbar_slot: 4,
            ..
        })
    ));
}
#[test]
fn completion_requires_exact_coordinate_and_exact_table_consumption() {
    let (mut observation, settings) = ready();
    let candidate = placement::candidates(&observation, &settings).remove(0);
    let before = std::collections::BTreeMap::from([(placement::TABLE.to_owned(), 2)]);
    observation.blocks[0].name = placement::TABLE.into();
    observation.items.insert(placement::TABLE.into(), 2);
    assert!(!placement::completed(&candidate, &observation, &before));
    observation.items.insert(placement::TABLE.into(), 1);
    assert!(placement::completed(&candidate, &observation, &before));
    observation.blocks[0].position.x += 1.0;
    assert!(!placement::completed(&candidate, &observation, &before));
    observation.blocks[0].position.x -= 1.0;
    observation.items.insert(placement::TABLE.into(), 0);
    assert!(!placement::completed(&candidate, &observation, &before));
}
#[test]
fn disabled_wrong_world_and_duplicate_slots_never_offer_placement() {
    let (mut observation, mut settings) = ready();
    settings.wood_skills = false;
    assert!(placement::candidates(&observation, &settings).is_empty());
    settings.wood_skills = true;
    observation.dimension = Some("other:world".into());
    assert!(placement::candidates(&observation, &settings).is_empty());
    observation.dimension = Some(settings.allowed_dimension.clone());
    observation.crafting_inventory.as_mut().unwrap()[1].slot = 9;
    assert!(placement::candidates(&observation, &settings).is_empty());
}
