//! Conservative gather feasibility shared by the engine and native adapter.
use crate::model::{Candidate, Mode, Observation, Position, Settings, WoodSkill};

/// The bounded recipe whitelist. The server's result slot still validates the recipe.
pub fn planks_for_log(log: &str) -> Option<String> {
    if !is_usable_log(log) {
        return None;
    }
    Some(format!(
        "{}_planks",
        log.strip_suffix("_log")
            .or_else(|| log.strip_suffix("_stem"))?
    ))
}

pub fn is_planks(item: &str) -> bool {
    matches!(
        item,
        "minecraft:oak_planks"
            | "minecraft:spruce_planks"
            | "minecraft:birch_planks"
            | "minecraft:jungle_planks"
            | "minecraft:acacia_planks"
            | "minecraft:dark_oak_planks"
            | "minecraft:mangrove_planks"
            | "minecraft:cherry_planks"
            | "minecraft:pale_oak_planks"
            | "minecraft:crimson_planks"
            | "minecraft:warped_planks"
            | "minecraft:bamboo_planks"
    )
}

pub fn crafting_candidates(observation: &Observation, settings: &Settings) -> Vec<Candidate> {
    if !settings.wood_skills
        || settings.mode != Mode::Live
        || !observation.connected
        || !observation.health.is_finite()
        || observation.health <= 0.0
        || observation.dimension.as_deref() != Some(settings.allowed_dimension.as_str())
    {
        return vec![];
    }
    let risk = crate::survival::work_risk_text(observation);
    let Some(slots) = &observation.crafting_inventory else {
        return vec![];
    };
    // Reserving an empty slot avoids output merging assumptions (components/stack caps).
    if slots.len() != 36
        || !(9..=44).all(|id| slots.iter().filter(|s| s.slot == id).count() == 1)
        || !slots.iter().any(|s| s.count == 0)
    {
        return vec![];
    }
    let mut logs = std::collections::BTreeSet::new();
    let mut planks = Vec::new();
    for slot in slots.iter().filter(|s| s.count > 0) {
        if is_usable_log(&slot.item) {
            logs.insert(slot.item.clone());
        }
        if is_planks(&slot.item) {
            planks.extend(std::iter::repeat_n(
                slot.item.clone(),
                (slot.count as usize).min(4 - planks.len()),
            ));
        }
    }
    let mut result: Vec<_> = logs
        .into_iter()
        .take(3)
        .filter_map(|log| {
            let planks = planks_for_log(&log)?;
            Some(Candidate {
                id: format!("craft_planks_{log}"),
                description: format!(
                    "Convert exactly one {log} into four {planks} in the inventory grid{risk}"
                ),
                target: None,
                duration_ms: 10_000,
                skill: Some(WoodSkill::CraftPlanks { log, planks }),
            })
        })
        .collect();
    if let Ok(planks) = <[String; 4]>::try_from(planks) {
        result.push(Candidate {
            id: "craft_table".into(),
            description: format!(
                "Consume four valid planks in the inventory grid to craft exactly one table{risk}"
            ),
            target: None,
            duration_ms: 10_000,
            skill: Some(WoodSkill::CraftTable { planks }),
        });
    }
    result
}

pub fn skill_is_current(
    candidate: &Candidate,
    observation: &Observation,
    settings: &Settings,
) -> bool {
    gather_is_current(candidate, observation, settings)
        || recover_is_current(candidate, observation, settings)
        || crate::placement::is_current(candidate, observation, settings)
        || ((1..=10_000).contains(&candidate.duration_ms)
            && crafting_candidates(observation, settings).iter().any(|c| {
                c.id == candidate.id && c.skill == candidate.skill && c.target == candidate.target
            }))
}

pub fn recover_candidate(observation: &Observation, settings: &Settings) -> Option<Candidate> {
    if !has_collection_space(observation)
        || !settings.wood_skills
        || settings.mode != Mode::Live
        || !observation.connected
        || !observation.health.is_finite()
        || observation.health <= 0.0
        || observation.dimension.as_deref() != Some(settings.allowed_dimension.as_str())
        || observation.entity_scan_incomplete
    {
        return None;
    }
    let drop = observation.recoverable_drop.as_ref()?;
    if !is_harvested_drop(&drop.log)
        || drop.route.is_empty()
        || distance(&observation.position, &drop.position) > 12.0
    {
        return None;
    }
    Some(Candidate {
        id: format!("recover_drop_{}", drop.log),
        description: format!(
            "Collect the currently visible {} item by its verified route and confirm inventory gain",
            drop.log
        ),
        target: Some(drop.position.clone()),
        duration_ms: 5_000,
        skill: Some(WoodSkill::RecoverDrop {
            log: drop.log.clone(),
            route: drop.route.clone(),
        }),
    })
}

pub fn recover_is_current(
    candidate: &Candidate,
    observation: &Observation,
    settings: &Settings,
) -> bool {
    (1..=5_000).contains(&candidate.duration_ms)
        && recover_candidate(observation, settings).is_some_and(|current| {
            current.id == candidate.id
                && current.target == candidate.target
                && current.skill == candidate.skill
        })
}

/// Expected ingredient use and output, shared by native execution and engine accounting.
pub fn recipe(skill: &WoodSkill) -> Option<(Vec<String>, String, u32)> {
    match skill {
        WoodSkill::CraftPlanks { log, planks } if planks_for_log(log).as_ref() == Some(planks) => {
            Some((vec![log.clone()], planks.clone(), 4))
        }
        WoodSkill::CraftTable { planks } if planks.iter().all(|p| is_planks(p)) => {
            Some((planks.to_vec(), "minecraft:crafting_table".into(), 1))
        }
        _ => None,
    }
}

/// The exact whitelist deliberately excludes pumpkin/melon stems and other plants.
/// These are the *tree* materials: a log or stem that a wood goal counts as wood.
pub fn is_usable_log(name: &str) -> bool {
    matches!(
        name,
        "minecraft:oak_log"
            | "minecraft:spruce_log"
            | "minecraft:birch_log"
            | "minecraft:jungle_log"
            | "minecraft:acacia_log"
            | "minecraft:dark_oak_log"
            | "minecraft:mangrove_log"
            | "minecraft:cherry_log"
            | "minecraft:pale_oak_log"
            | "minecraft:crimson_stem"
            | "minecraft:warped_stem"
    )
}

/// Bulk materials a first-day survival run breaks with the same harvester: stone for
/// tools and shelter, soil for ground, sand and gravel as loose fill. Bounded on purpose;
/// anything not listed is not a harvest target.
pub fn is_bulk_harvestable(name: &str) -> bool {
    matches!(
        name,
        "minecraft:stone"
            | "minecraft:cobblestone"
            | "minecraft:deepslate"
            | "minecraft:cobbled_deepslate"
            | "minecraft:dirt"
            | "minecraft:coarse_dirt"
            | "minecraft:grass_block"
            | "minecraft:sand"
            | "minecraft:gravel"
            | "minecraft:tuff"
            | "minecraft:andesite"
            | "minecraft:diorite"
            | "minecraft:granite"
    )
}

/// The one harvest whitelist. Perceiving a tree or a stone is the same operation, so
/// this is the only place that decides what may be broken.
pub fn is_harvestable(name: &str) -> bool {
    is_usable_log(name) || is_bulk_harvestable(name)
}

/// The item a block actually yields. Mining confirms its own drop, so the executor
/// compares the inventory against this name and never against the block name.
pub fn drop_for_block(name: &str) -> Option<String> {
    match name {
        "minecraft:stone" => Some("minecraft:cobblestone".into()),
        "minecraft:deepslate" => Some("minecraft:cobbled_deepslate".into()),
        "minecraft:grass_block" => Some("minecraft:dirt".into()),
        _ if is_harvestable(name) => Some(name.to_owned()),
        _ => None,
    }
}

/// Items the one harvester may confirm as an inventory gain (the drop side of
/// [`is_harvestable`]). Used when a dropped item is recovered from a live world.
pub fn is_harvested_drop(item: &str) -> bool {
    is_usable_log(item)
        || matches!(
            item,
            "minecraft:cobblestone"
                | "minecraft:cobbled_deepslate"
                | "minecraft:dirt"
                | "minecraft:sand"
                | "minecraft:gravel"
                | "minecraft:tuff"
                | "minecraft:andesite"
                | "minecraft:diorite"
                | "minecraft:granite"
        )
}

fn distance(a: &Position, b: &Position) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2) + (a.z - b.z).powi(2)).sqrt()
}

pub fn gather_candidates(observation: &Observation, settings: &Settings) -> Vec<Candidate> {
    if !has_collection_space(observation)
        || !settings.wood_skills
        || settings.mode != Mode::Live
        || !observation.connected
        || observation.health <= 0.0
        || !observation.health.is_finite()
        || observation.dimension.as_deref() != Some(settings.allowed_dimension.as_str())
    {
        return vec![];
    }
    let risk = crate::survival::work_risk_text(observation);
    observation.trees.iter().flat_map(|tree| tree.next.iter()).filter_map(|access| {
        let approach = access.route.last()?.clone();
        if !is_harvestable(&access.log) || distance(&observation.position, &access.target)>12.0 {return None;}
        let drop = drop_for_block(&access.log)?;
        Some(Candidate {
            id: format!("gather_{}_{}_{}", access.target.x.floor(), access.target.y.floor(), access.target.z.floor()),
            description: format!(
                "Harvest the observed {} at ({}, {}, {}) by its verified stance and route; mining yields {} and must be confirmed against it; preserve standing support and return before reporting success.{risk}",
                access.log,
                access.target.x.floor(),
                access.target.y.floor(),
                access.target.z.floor(),
                drop,
            ),
            target: Some(access.target.clone()), duration_ms: 10_000,
            skill: Some(WoodSkill::Gather {log:access.log.clone(), approach, route:access.route.clone()}),
        })
    }).collect()
}

/// Revalidates the exact bound operation, rather than choosing a replacement target.
pub fn gather_is_current(
    candidate: &Candidate,
    observation: &Observation,
    settings: &Settings,
) -> bool {
    let mut bound = observation.clone();
    for tree in &mut bound.trees {
        tree.next
            .retain(|access| Some(&access.target) == candidate.target.as_ref());
    }
    (1..=10_000).contains(&candidate.duration_ms)
        && gather_candidates(&bound, settings).iter().any(|current| {
            current.id == candidate.id
                && current.target == candidate.target
                && current.skill == candidate.skill
        })
}

/// Conservative capacity: reserve an empty owned storage/hotbar slot; never assume merging.
pub fn has_collection_space(observation: &Observation) -> bool {
    observation
        .crafting_inventory
        .as_ref()
        .is_some_and(|slots| {
            slots.len() == 36
                && (9..=44).all(|id| slots.iter().filter(|s| s.slot == id).count() == 1)
                && slots.iter().any(|s| s.count == 0)
        })
}
