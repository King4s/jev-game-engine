//! Bound placement candidates. The native adapter alone emits verified site markers;
//! a coordinate remembered by a planner is never a placement authorization.
use crate::model::{Candidate, Mode, Observation, Position, Settings, WoodSkill};

pub const TABLE: &str = "minecraft:crafting_table";

pub fn site_name(target: &Position) -> String {
    format!(
        "table_site:{}:{}:{}",
        target.x.floor(),
        target.y.floor(),
        target.z.floor()
    )
}

pub fn candidates(observation: &Observation, settings: &Settings) -> Vec<Candidate> {
    if !settings.wood_skills
        || settings.mode != Mode::Live
        || observation.dimension.as_deref() != Some(settings.allowed_dimension.as_str())
    {
        return vec![];
    }
    let risk = crate::survival::work_risk_text(observation);
    let Some(slots) = &observation.crafting_inventory else {
        return vec![];
    };
    if slots.len() != 36 || !(9..=44).all(|id| slots.iter().filter(|s| s.slot == id).count() == 1) {
        return vec![];
    }
    let Some(source) = slots
        .iter()
        .filter(|s| s.item == TABLE && s.count > 0)
        .min_by_key(|s| (s.slot < 36, s.slot))
    else {
        return vec![];
    };
    let hotbar = if source.slot >= 36 {
        source.slot
    } else if let Some(empty) = slots.iter().find(|s| s.slot >= 36 && s.count == 0) {
        empty.slot
    } else {
        return vec![];
    };
    observation.blocks.iter().filter_map(|site| {
        let target = &site.position;
        if ![target.x,target.y,target.z].iter().all(|n|n.is_finite() && n.abs()<30_000_000.0)
            || site.name != site_name(target)
            || ((target.x-observation.position.x).powi(2)+(target.y-observation.position.y).powi(2)+(target.z-observation.position.z).powi(2)).sqrt()>3.0 {
            return None;
        }
        Some(Candidate { id:site.name.clone(),
            description: format!("Place one owned crafting table at the observed clear site, keeping occupied space and an exit clear; require server block and inventory confirmation{risk}"),
            target:Some(target.clone()),duration_ms:5_000,
            skill:Some(WoodSkill::PlaceTable { support:Position { x:target.x.floor(),y:target.y.floor()-1.0,z:target.z.floor() },
                source_slot:source.slot,hotbar_slot:(hotbar-36) as u8 }) })
    }).take(3).collect()
}

pub fn is_current(candidate: &Candidate, observation: &Observation, settings: &Settings) -> bool {
    (1..=5_000).contains(&candidate.duration_ms)
        && candidates(observation, settings).iter().any(|current| {
            current.id == candidate.id
                && current.target == candidate.target
                && current.skill == candidate.skill
        })
}

/// Called only after matching server-confirmed attempt/epoch outcome. An item
/// decrement alone is not placement, nor is an unrelated table elsewhere.
pub fn completed(
    candidate: &Candidate,
    observation: &Observation,
    before: &std::collections::BTreeMap<String, u32>,
) -> bool {
    let Some(target) = &candidate.target else {
        return false;
    };
    let old = before.get(TABLE).copied().unwrap_or(0);
    old > 0
        && observation.items.get(TABLE).copied().unwrap_or(0) == old - 1
        && observation.blocks.iter().any(|block| {
            matches!(block.name.as_str(), "crafting_table" | TABLE)
                && block.position.x.floor() == target.x.floor()
                && block.position.y.floor() == target.y.floor()
                && block.position.z.floor() == target.z.floor()
        })
}
