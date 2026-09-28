use jev_game_engine::model::{CraftingSlot, Mode, Observation, Settings, WoodSkill};
use jev_game_engine::wood::{crafting_candidates, skill_is_current};

fn ready() -> (Observation, Settings) {
    let settings = Settings {
        wood_skills: true,
        mode: Mode::Live,
        ..Settings::default()
    };
    let observation = Observation {
        connected: true,
        health: 20.0,
        food: 20.0,
        dimension: Some(settings.allowed_dimension.clone()),
        crafting_inventory: Some(
            (9..=44)
                .map(|slot| CraftingSlot {
                    slot,
                    item: "minecraft:air".into(),
                    count: 0,
                })
                .collect(),
        ),
        ..Observation::default()
    };
    (observation, settings)
}

fn set_slot(observation: &mut Observation, slot: u16, item: &str, count: u32) {
    let slots = observation.crafting_inventory.as_mut().unwrap();
    let entry = slots.iter_mut().find(|entry| entry.slot == slot).unwrap();
    entry.item = item.into();
    entry.count = count;
}

#[test]
fn competing_needs_do_not_revoke_a_selected_recipe_without_a_physical_reason() {
    use jev_game_engine::model::{Landmark, Position};
    let (mut observation, settings) = ready();
    assert!(!settings.safety_reflex);
    set_slot(&mut observation, 9, "minecraft:oak_log", 1);
    let selected = crafting_candidates(&observation, &settings).remove(0);
    // Hunger is a competing need, not a physical obstacle. Whether to eat now or keep crafting is
    // Jev's decision, so the engine must neither revoke the chosen recipe nor hide the menu because
    // of it: the hunger stays in the observation, where the decision can weigh it.
    for food in [0.0, 6.0, -1.0, 21.0, f32::NAN, f32::INFINITY] {
        observation.food = food;
        assert!(
            skill_is_current(&selected, &observation, &settings),
            "hunger {food} revoked a recipe the engine can still execute"
        );
        assert!(
            !crafting_candidates(&observation, &settings).is_empty(),
            "hunger {food} hid the crafting possibilities from the menu"
        );
    }
    observation.food = 7.0;
    assert!(skill_is_current(&selected, &observation, &settings));
    for (name, distance) in [("Zombie", 12.0), ("Skeleton", 16.0)] {
        observation.entities = vec![Landmark {
            name: name.into(),
            position: Position {
                x: distance,
                y: 0.0,
                z: 0.0,
            },
        }];
        assert!(
            skill_is_current(&selected, &observation, &settings),
            "{name} at {distance} m revoked a recipe the engine can still execute"
        );
        assert!(
            !crafting_candidates(&observation, &settings).is_empty(),
            "{name} at {distance} m hid the crafting possibilities from the menu"
        );
    }
    observation.entities.clear();
    assert!(skill_is_current(&selected, &observation, &settings));
    // The physical check still belongs to the engine: with the ingredient gone this recipe cannot
    // run, so the plan is no longer current and the choice leaves the menu.
    set_slot(&mut observation, 9, "minecraft:air", 0);
    assert!(!skill_is_current(&selected, &observation, &settings));
    assert!(crafting_candidates(&observation, &settings).is_empty());
}

#[test]
fn mixed_valid_storage_planks_can_make_one_table() {
    let (mut observation, settings) = ready();
    set_slot(&mut observation, 9, "minecraft:oak_planks", 2);
    set_slot(&mut observation, 10, "minecraft:birch_planks", 2);

    let table = crafting_candidates(&observation, &settings)
        .into_iter()
        .find(|candidate| candidate.id == "craft_table")
        .expect("four storage planks should permit one table");
    assert_eq!(
        table.skill,
        Some(WoodSkill::CraftTable {
            planks: [
                "minecraft:oak_planks".into(),
                "minecraft:oak_planks".into(),
                "minecraft:birch_planks".into(),
                "minecraft:birch_planks".into(),
            ],
        })
    );
    set_slot(&mut observation, 10, "minecraft:birch_planks", 1);
    assert!(!skill_is_current(&table, &observation, &settings));
    assert!(
        crafting_candidates(&observation, &settings)
            .iter()
            .all(|candidate| candidate.id != "craft_table")
    );
}

#[test]
fn crafting_requires_one_reserved_empty_storage_slot_and_a_complete_idle_snapshot() {
    let (mut observation, settings) = ready();
    for slot in 9..=44 {
        set_slot(&mut observation, slot, "minecraft:dirt", 1);
    }
    set_slot(&mut observation, 9, "minecraft:oak_log", 1);
    assert!(crafting_candidates(&observation, &settings).is_empty());

    set_slot(&mut observation, 44, "minecraft:air", 0);
    assert!(
        crafting_candidates(&observation, &settings)
            .iter()
            .any(|candidate| candidate.id == "craft_planks_minecraft:oak_log")
    );
    observation.crafting_inventory.as_mut().unwrap().remove(0);
    assert!(crafting_candidates(&observation, &settings).is_empty());
}

#[test]
fn replaced_ingredient_cancels_the_bound_plank_recipe() {
    let (mut observation, settings) = ready();
    set_slot(&mut observation, 9, "minecraft:oak_log", 1);
    let plank = crafting_candidates(&observation, &settings)
        .into_iter()
        .find(|candidate| candidate.id == "craft_planks_minecraft:oak_log")
        .unwrap();
    assert!(skill_is_current(&plank, &observation, &settings));

    set_slot(&mut observation, 9, "minecraft:spruce_log", 1);
    assert!(!skill_is_current(&plank, &observation, &settings));
    assert!(
        crafting_candidates(&observation, &settings)
            .iter()
            .any(|candidate| candidate.id == "craft_planks_minecraft:spruce_log")
    );
}
