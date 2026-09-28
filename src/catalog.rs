//! One catalogue: the goals the observed world currently makes possible, derived on
//! every request from the same observation the UI shows.
//!
//! A catalogue entry is a goal, not a micro-action: it is a commitment the engine then
//! carries out on its own — many blocks, many steps — through the single mover
//! ([`crate::motion`]) and the single harvester ([`crate::wood`]). The engine asks the
//! model again only when the goal is finished, has honestly failed, or survival forces
//! a new decision. No goal is silently narrowed here: an entry either names the work it
//! will do and the observed resource that makes it possible, or it is not offered.
//!
//! Every category is bounded and stated in the entry itself, and the order is fixed
//! (the survival ladder), so two observations of the same world produce the same list
//! and recordings stay comparable.
use std::collections::BTreeMap;

use crate::model::{Candidate, Observation, Position, Settings};
use crate::wood::{self, is_usable_log};

/// The catalogue never offers more goals than this, and says so in each entry.
pub const MAX_GOALS: usize = 9;
/// How many steps the engine may take inside one goal before it reports honest failure.
pub const MAX_GOAL_STEPS: u32 = 64;
/// How many observed harvestable blocks one observation may report. Bounded so one
/// observation stays bounded; the count is stated in the goal text.
pub const MAX_HARVEST_OPTIONS: usize = 64;
/// Ticks in one Minecraft day cycle; dusk begins at 12,000 and night at 13,000.
pub const DAY_TICKS: u64 = 24_000;
pub const DUSK_TICK: u64 = 12_000;
pub const NIGHT_TICK: u64 = 13_000;
/// Daylight left below this is "dusk is close": a build goal outranks a long trip.
pub const SHELTER_DEADLINE_TICKS: u64 = 3_000;

/// Ticks of daylight left in the observed day, or `None` when the server has not sent
/// the time. Derived from the observation's own time of day: "before dusk" cannot be
/// judged without it.
pub fn daylight_left(observation: &Observation) -> Option<u64> {
    let now = observation.time_of_day? % DAY_TICKS;
    Some(if now < DUSK_TICK {
        DUSK_TICK - now
    } else {
        DAY_TICKS - now + DUSK_TICK
    })
}

/// Ticks until the next dawn, used by the survive goal.
pub fn until_dawn(observation: &Observation) -> Option<u64> {
    let now = observation.time_of_day? % DAY_TICKS;
    // Dawn is tick 0, so the wait to the next dawn is the rest of the cycle whether it is
    // currently day or night; there is no second case to branch on.
    Some(DAY_TICKS - now)
}

pub fn is_night(observation: &Observation) -> bool {
    observation
        .time_of_day
        .map(|t| (NIGHT_TICK..DAY_TICKS).contains(&(t % DAY_TICKS)))
        .unwrap_or(false)
}

/// What a harvest goal is about, so wood, stone and soil travel the same path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Log,
    Stone,
    Soil,
}

pub fn group_of(block: &str) -> Option<Group> {
    if is_usable_log(block) {
        Some(Group::Log)
    } else if matches!(
        block,
        "minecraft:stone"
            | "minecraft:cobblestone"
            | "minecraft:deepslate"
            | "minecraft:cobbled_deepslate"
            | "minecraft:tuff"
            | "minecraft:andesite"
            | "minecraft:diorite"
            | "minecraft:granite"
    ) {
        Some(Group::Stone)
    } else if wood::is_bulk_harvestable(block) {
        Some(Group::Soil)
    } else {
        None
    }
}

/// One observed harvestable block: the same fact whether it is a log, a stone or dirt.
#[derive(Clone, Debug, PartialEq)]
pub struct HarvestOption {
    pub block: String,
    pub position: Position,
    /// The item this block really yields (stone yields cobblestone, grass block dirt).
    pub drop: String,
    /// A stance and route for it were verified in the observation that produced this.
    pub verified: bool,
    pub group: Group,
}

/// Every observed harvestable block, in a stable order: supported-and-verified first,
/// then by descending height, then by position, then by name.
pub fn harvest_options(observation: &Observation) -> Vec<HarvestOption> {
    let mut options: BTreeMap<(String, i64, i64, i64), HarvestOption> = BTreeMap::new();
    for tree in &observation.trees {
        if !wood::is_harvestable(&tree.wood) {
            continue;
        }
        for log in &tree.logs {
            let key = (
                tree.wood.clone(),
                log.x.floor() as i64,
                log.y.floor() as i64,
                log.z.floor() as i64,
            );
            if options.len() >= MAX_HARVEST_OPTIONS && !options.contains_key(&key) {
                continue;
            }
            let Some(drop) = wood::drop_for_block(&tree.wood) else {
                continue;
            };
            let Some(group) = group_of(&tree.wood) else {
                continue;
            };
            options.insert(
                key,
                HarvestOption {
                    block: tree.wood.clone(),
                    position: log.clone(),
                    drop,
                    verified: false,
                    group,
                },
            );
        }
        for access in &tree.next {
            let key = (
                tree.wood.clone(),
                access.target.x.floor() as i64,
                access.target.y.floor() as i64,
                access.target.z.floor() as i64,
            );
            if let Some(option) = options.get_mut(&key) {
                option.verified = true;
            }
        }
    }
    let mut list: Vec<HarvestOption> = options.into_values().collect();
    list.sort_by(|a, b| {
        b.verified
            .cmp(&a.verified)
            .then_with(|| b.position.y.total_cmp(&a.position.y))
            .then_with(|| a.position.x.total_cmp(&b.position.x))
            .then_with(|| a.position.z.total_cmp(&b.position.z))
            .then_with(|| a.block.cmp(&b.block))
    });
    list
}

/// The rungs of the survival ladder this engine can act on, in the order a first day
/// needs them. A rung is offered only when the observed world makes it possible.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoalKind {
    /// Bring back logs from the observed trees.
    Wood,
    /// Convert logs already owned into planks.
    Planks,
    /// Craft and place a crafting table from owned planks.
    Table,
    /// Mine cobblestone from observed stone until enough is owned.
    Stone,
    /// Dig soil until enough is owned.
    Soil,
    /// Build shelter from the blocks already held before dusk.
    Shelter,
    /// Keep a light burning where the bot stands.
    Light,
    /// Eat what is edible until not hungry.
    Food,
    /// Stay alive until dawn while something threatens.
    Survive,
}

impl GoalKind {
    /// The ladder, in order. This is also the catalogue's stable order.
    pub const LADDER: [GoalKind; MAX_GOALS] = [
        GoalKind::Wood,
        GoalKind::Planks,
        GoalKind::Table,
        GoalKind::Stone,
        GoalKind::Soil,
        GoalKind::Shelter,
        GoalKind::Light,
        GoalKind::Food,
        GoalKind::Survive,
    ];

    pub fn id(self) -> &'static str {
        match self {
            GoalKind::Wood => "goal_wood",
            GoalKind::Planks => "goal_planks",
            GoalKind::Table => "goal_table",
            GoalKind::Stone => "goal_stone",
            GoalKind::Soil => "goal_soil",
            GoalKind::Shelter => "goal_shelter",
            GoalKind::Light => "goal_light",
            GoalKind::Food => "goal_food",
            GoalKind::Survive => "goal_survive",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::LADDER.into_iter().find(|kind| kind.id() == id)
    }

    /// The harvest group this goal banks, if it is a harvest goal.
    pub fn group(self) -> Option<Group> {
        match self {
            GoalKind::Wood => Some(Group::Log),
            GoalKind::Stone => Some(Group::Stone),
            GoalKind::Soil => Some(Group::Soil),
            _ => None,
        }
    }

    /// The item this goal counts and how many of it the goal is committed to.
    pub fn target_item(self) -> Option<(&'static str, u32)> {
        match self {
            GoalKind::Wood => Some(("any log", 3)),
            GoalKind::Stone => Some(("minecraft:cobblestone", 20)),
            GoalKind::Soil => Some(("minecraft:dirt", 12)),
            GoalKind::Planks => Some(("any planks", 4)),
            GoalKind::Table => Some(("minecraft:crafting_table", 1)),
            _ => None,
        }
    }

    /// A rough run length in milliseconds, stated to the model so it can weigh the goal.
    pub fn duration_ms(self) -> u64 {
        match self {
            GoalKind::Wood => 60_000,
            GoalKind::Planks => 10_000,
            GoalKind::Table => 20_000,
            GoalKind::Stone => 90_000,
            GoalKind::Soil => 60_000,
            GoalKind::Shelter => 120_000,
            GoalKind::Light => 20_000,
            GoalKind::Food => 15_000,
            GoalKind::Survive => 120_000,
        }
    }
}

pub fn is_goal(id: &str) -> bool {
    GoalKind::from_id(id).is_some()
}

fn held(observation: &Observation, item: &str) -> u32 {
    observation.items.get(item).copied().unwrap_or(0)
}

fn held_matching(observation: &Observation, mut matches: impl FnMut(&str) -> bool) -> u32 {
    observation
        .items
        .iter()
        .filter(|(item, count)| **count > 0 && matches(item))
        .map(|(_, count)| *count)
        .sum()
}

fn nearest(position: &Position, options: &[HarvestOption], group: Group) -> Option<f64> {
    options
        .iter()
        .filter(|option| option.group == group)
        .map(|option| {
            let dx = option.position.x - position.x;
            let dy = option.position.y - position.y;
            let dz = option.position.z - position.z;
            (dx * dx + dy * dy + dz * dz).sqrt()
        })
        .min_by(f64::total_cmp)
}

/// Why the observed world makes this goal possible right now, or `None` when it does
/// not. A goal with no reason is not offered: the list is the world, not a menu.
fn possible(
    kind: GoalKind,
    observation: &Observation,
    options: &[HarvestOption],
) -> Option<String> {
    let daylight = daylight_left(observation);
    match kind {
        GoalKind::Wood => {
            let observed = held_matching(observation, is_usable_log);
            let logs = options.iter().filter(|o| o.group == Group::Log).count();
            let verified = options
                .iter()
                .filter(|o| o.group == Group::Log && o.verified)
                .count();
            if logs == 0 || verified == 0 {
                return None;
            }
            let nearest = nearest(&observation.position, options, Group::Log)?;
            Some(format!(
                "{logs} log block(s) observed ({verified} with a verified stance and route), nearest {nearest:.1} blocks away; {observed} log item(s) already held"
            ))
        }
        GoalKind::Planks => {
            let logs = held_matching(observation, is_usable_log);
            let planks = held_matching(observation, wood::is_planks);
            (logs > 0 && observation.crafting_inventory.is_some()).then(|| {
                format!("{logs} log item(s) held for the inventory grid; {planks} planks held")
            })
        }
        GoalKind::Table => {
            let planks = held_matching(observation, wood::is_planks);
            if held(observation, crate::placement::TABLE) > 0 {
                return None;
            }
            (planks >= 4 && observation.crafting_inventory.is_some())
                .then(|| format!("{planks} plank(s) held, four of them make one table in the grid"))
        }
        GoalKind::Stone => {
            let cobble = held(observation, "minecraft:cobblestone")
                + held(observation, "minecraft:cobbled_deepslate");
            let observed = options.iter().filter(|o| o.group == Group::Stone).count();
            let verified = options
                .iter()
                .filter(|o| o.group == Group::Stone && o.verified)
                .count();
            (observed > 0 && verified > 0 && cobble < 20).then(|| {
                let nearest = nearest(&observation.position, options, Group::Stone).unwrap_or(0.0);
                format!(
                    "{observed} stone block(s) observed ({verified} with a verified stance and route), nearest {nearest:.1} blocks away; {cobble} of 20 owned"
                )
            })
        }
        GoalKind::Soil => {
            let soil =
                held(observation, "minecraft:dirt") + held(observation, "minecraft:grass_block");
            let observed = options.iter().filter(|o| o.group == Group::Soil).count();
            let verified = options
                .iter()
                .filter(|o| o.group == Group::Soil && o.verified)
                .count();
            (observed > 0 && verified > 0 && soil < 12).then(|| {
                let nearest = nearest(&observation.position, options, Group::Soil).unwrap_or(0.0);
                format!(
                    "{observed} soil block(s) observed ({verified} with a verified stance and route), nearest {nearest:.1} blocks away; {soil} of 12 owned"
                )
            })
        }
        GoalKind::Shelter => {
            let blocks = held_matching(observation, |item| {
                wood::is_harvestable(item) || wood::is_planks(item)
            });
            let deadline = daylight?;
            (blocks >= 8 && deadline <= SHELTER_DEADLINE_TICKS).then(|| {
                format!(
                    "{blocks} block(s) held; only {deadline} tick(s) of daylight left before mobs can spawn"
                )
            })
        }
        GoalKind::Light => {
            let light = observation
                .items
                .iter()
                .filter(|(item, count)| **count > 0 && item.ends_with("torch"))
                .count();
            (light > 0).then(|| format!("{light} torch item(s) held"))
        }
        GoalKind::Food => (observation.food < 20.0).then(|| {
            format!(
                "food is {:.0} of 20; edible items held: {}",
                observation.food,
                held_matching(observation, is_edible)
            )
        }),
        GoalKind::Survive => {
            let threat = crate::survival::nearest_threat(observation)?;
            Some(format!(
                "{} is {:.1} blocks away; {}",
                threat.kind,
                threat.distance_m,
                match daylight {
                    Some(ticks) => format!("{ticks} tick(s) of daylight left"),
                    None => "the server has not sent the time".into(),
                }
            ))
        }
    }
}

/// What the bot can eat. Bounded whitelist: raw, cooked and baked staples only.
pub fn is_edible(item: &str) -> bool {
    matches!(
        item,
        "minecraft:bread"
            | "minecraft:apple"
            | "minecraft:carrot"
            | "minecraft:potato"
            | "minecraft:baked_potato"
            | "minecraft:cooked_beef"
            | "minecraft:cooked_porkchop"
            | "minecraft:cooked_chicken"
            | "minecraft:cooked_mutton"
            | "minecraft:cooked_cod"
            | "minecraft:cooked_salmon"
            | "minecraft:beef"
            | "minecraft:porkchop"
            | "minecraft:chicken"
            | "minecraft:mutton"
            | "minecraft:sweet_berries"
    )
}

/// The cost and risk a goal carries, stated so the model can weigh it. Daylight is part
/// of it because a walk that ends at dusk is not free.
fn costs(kind: GoalKind, observation: &Observation, options: &[HarvestOption]) -> String {
    let mut parts = Vec::new();
    match daylight_left(observation) {
        Some(ticks) if ticks <= SHELTER_DEADLINE_TICKS => {
            parts.push(format!("only {ticks} tick(s) of daylight left"))
        }
        Some(ticks) => parts.push(format!("{ticks} tick(s) of daylight left")),
        None => parts.push("time of day unknown".into()),
    }
    if let Some(threat) = crate::survival::nearest_threat(observation) {
        parts.push(format!(
            "{} is {:.1} blocks away",
            threat.kind, threat.distance_m
        ));
    }
    match kind {
        GoalKind::Wood | GoalKind::Stone | GoalKind::Soil => {
            let hay = options
                .iter()
                .filter(|option| option.group == kind.group().unwrap_or(Group::Log))
                .count();
            parts.push(format!(
                "{hay} of at most {MAX_HARVEST_OPTIONS} reported harvest target(s) in this observation"
            ));
            parts.push("arriving, mining and collecting must all confirm".into());
        }
        GoalKind::Table => parts.push("one crafted table and one hotbar swap".into()),
        GoalKind::Shelter | GoalKind::Light | GoalKind::Food | GoalKind::Survive => parts.push(
            "this goal's steps are not in this engine build yet; it is offered because the world makes it necessary, and it fails honestly if chosen"
                .into(),
        ),
        GoalKind::Planks => parts.push("four planks per log consumed".into()),
    }
    parts.join("; ")
}

/// Whether this build can carry a goal out, derived from the code that would do it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoalExecutor {
    /// The engine resolves the goal into verified steps and runs them through the one mover
    /// and the one harvester, asking the model again only when the goal is finished, has
    /// honestly failed, or survival forces a new decision.
    Steps,
    /// No executor in this build carries the goal out. Keep it as observed knowledge
    /// for the model, but never offer it as a selectable action.
    KnowledgeOnly,
}

/// Whether anything in this build dispatches the steps a goal resolves to.
///
/// [`goal_steps`] (and [`next_step`], which is its first entry) derive a goal's work — the
/// `gather_*`, `craft_*` and `place_*` candidates the executors already run — and **no call
/// site dispatches them**: the engine sends the goal candidate the model chose to the adapter,
/// the adapter's local guard runs a candidate that carries a skill or a bounded target, and the
/// engine then only waits out `duration_ms` and reports "goal duration expired". A live
/// creative run is the evidence: `goal_soil` was offered, chosen and refused with one sentence
/// that named no check. Until a goal loop exists, this is the single switch that decides
/// between work and knowledge, and `catalog_contract` pins it so the catalogue cannot start
/// promising work again without that loop.
pub const GOAL_STEPS_ARE_DISPATCHED: bool = false;

/// Whether this build derives any step for a goal kind. Judged by the same match
/// [`goal_steps`] runs, so the two cannot disagree about what is derivable.
pub fn resolves_steps(kind: GoalKind) -> bool {
    matches!(
        kind,
        GoalKind::Wood | GoalKind::Planks | GoalKind::Table | GoalKind::Stone | GoalKind::Soil
    )
}

/// What this build does with a goal the model chooses: work, or knowledge only. A goal is work
/// only when the engine both derives its steps and dispatches them.
pub fn goal_executor(kind: GoalKind) -> GoalExecutor {
    if GOAL_STEPS_ARE_DISPATCHED && resolves_steps(kind) {
        GoalExecutor::Steps
    } else {
        GoalExecutor::KnowledgeOnly
    }
}

/// One entry per goal the world currently makes possible, in ladder order.
pub fn goals(observation: &Observation, settings: &Settings) -> Vec<Candidate> {
    if !settings.wood_skills
        || observation.dimension.as_deref() != Some(settings.allowed_dimension.as_str())
    {
        return Vec::new();
    }
    let options = harvest_options(observation);
    let mut found: Vec<(GoalKind, String, String)> = Vec::new();
    for kind in GoalKind::LADDER {
        let Some(reason) = possible(kind, observation, &options) else {
            continue;
        };
        let want = kind.target_item().map(|(_, want)| want).unwrap_or(0);
        let what = match kind {
            GoalKind::Wood => {
                format!("Harvest {want} log(s) from the observed trees and bring them back")
            }
            GoalKind::Planks => format!("Turn the logs you hold into planks ({want} or more)"),
            GoalKind::Table => {
                "Craft one crafting table from held planks and place it on a verified site".into()
            }
            GoalKind::Stone => format!("Mine {want} cobblestone from the observed stone"),
            GoalKind::Soil => format!("Dig {want} dirt from the observed soil"),
            GoalKind::Shelter => {
                "Build a 3x3 shelter with a roof from the blocks you hold before dusk".into()
            }
            GoalKind::Light => "Place a torch inside the shelter and keep it lit".into(),
            GoalKind::Food => "Eat what is edible until you are not hungry".into(),
            GoalKind::Survive => "Survive until dawn without trading a verified step".into(),
        };
        found.push((
            kind,
            format!(
                "{what}; banks {what_item}",
                what_item = if what.is_empty() { kind.id() } else { &what }
            ),
            format!(
                "{reason}. Expected run: about {} s. Cost and risk: {}.",
                kind.duration_ms() / 1_000,
                costs(kind, observation, &options),
            ),
        ));
    }
    found
        .into_iter()
        .take(MAX_GOALS)
        .enumerate()
        .map(|(index, (kind, what, reason))| {
            // What the engine will really do with this goal, said in the entry itself: an
            // entry that promises work nothing in this build performs is what left a live run
            // refused with a sentence that named no check.
            let executor = match goal_executor(kind) {
                GoalExecutor::Steps => "The engine carries this goal out on its own through the one mover and the one harvester and asks again only when it is finished, has honestly failed, or survival forces a new decision.",
                GoalExecutor::KnowledgeOnly => "Knowledge only: no executor in this build turns a goal into steps yet, so this entry is not work. Choosing it is refused with that reason, never run as a no-op, and the refusal names the missing executor. Use the world facts above to reason; choose an option with a local executor to act.",
            };
            Candidate {
                skill: None,
                id: kind.id().into(),
                description: format!(
                    "Goal {}/{} (bounded catalogue, ladder order): {what}. {reason} {executor}",
                    index + 1,
                    MAX_GOALS,
                ),
                // A goal is a commitment, not a position: the engine resolves the exact
                // steps itself from the fresh observations that follow.
                target: None,
                duration_ms: kind.duration_ms(),
            }
        })
        .collect()
}

/// Is the goal already satisfied by this observation? Checked against items and blocks
/// the adapter observed, never against an intention.
pub fn goal_complete(kind: GoalKind, observation: &Observation) -> bool {
    match kind {
        GoalKind::Wood => held_matching(observation, is_usable_log) >= 3,
        GoalKind::Planks => held_matching(observation, wood::is_planks) >= 4,
        GoalKind::Table => held(observation, crate::placement::TABLE) > 0,
        GoalKind::Stone => {
            held(observation, "minecraft:cobblestone")
                + held(observation, "minecraft:cobbled_deepslate")
                >= 20
        }
        GoalKind::Soil => {
            held(observation, "minecraft:dirt") + held(observation, "minecraft:sand") >= 12
        }
        GoalKind::Shelter | GoalKind::Light | GoalKind::Food | GoalKind::Survive => false,
    }
}

/// The first step of a goal, taken from the same executable candidates the engine had before
/// this catalogue existed: one harvester, one mover, no per-skill special case.
/// `None` means no verified step is possible in this observation, which the engine reports as
/// an honest goal failure instead of inventing work.
pub fn next_step(
    kind: GoalKind,
    observation: &Observation,
    settings: &Settings,
) -> Option<Candidate> {
    goal_steps(kind, observation, settings).into_iter().next()
}

/// The step candidates the engine may dispatch inside a goal, in the order it will try
/// them. Bounded, and always derived from this observation.
pub fn goal_steps(
    kind: GoalKind,
    observation: &Observation,
    settings: &Settings,
) -> Vec<Candidate> {
    match kind {
        GoalKind::Wood | GoalKind::Stone | GoalKind::Soil => {
            let Some(group) = kind.group() else {
                return Vec::new();
            };
            wood::gather_candidates(observation, settings)
                .into_iter()
                .filter(|candidate| {
                    matches!(&candidate.skill, Some(crate::model::WoodSkill::Gather { log, .. })
                        if group_of(log) == Some(group))
                })
                .collect()
        }
        GoalKind::Planks => wood::crafting_candidates(observation, settings)
            .into_iter()
            .filter(|candidate| candidate.id.starts_with("craft_planks_"))
            .collect(),
        GoalKind::Table => {
            let mut steps = wood::crafting_candidates(observation, settings)
                .into_iter()
                .filter(|candidate| candidate.id == "craft_table")
                .collect::<Vec<_>>();
            steps.extend(crate::placement::candidates(observation, settings));
            steps
        }
        GoalKind::Shelter | GoalKind::Light | GoalKind::Food | GoalKind::Survive => Vec::new(),
    }
}

/// Selectable goals require an actual step executor. Knowledge-only goals are
/// retained separately in the observation context, never sent as action IDs.
pub fn candidates(
    observation: &Observation,
    duration_ms: u64,
    settings: &Settings,
) -> Vec<Candidate> {
    let mut result = goals(observation, settings)
        .into_iter()
        .filter(|candidate| {
            GoalKind::LADDER.iter().any(|kind| {
                kind.id() == candidate.id && goal_executor(*kind) == GoalExecutor::Steps
            })
        })
        .collect::<Vec<_>>();
    result.extend(tactical(observation, duration_ms, settings));
    result
}

/// The immediately executable options: observed move targets, flight, harvest of every
/// observed harvestable block, a recoverable drop, crafting and placement.
pub fn tactical(
    observation: &Observation,
    duration_ms: u64,
    settings: &Settings,
) -> Vec<Candidate> {
    let mut result = crate::catalog::moves(observation, duration_ms, settings);
    result.extend(crate::wood::gather_candidates(observation, settings));
    result.extend(crate::wood::recover_candidate(observation, settings));
    result.extend(crate::wood::crafting_candidates(observation, settings));
    result.extend(crate::placement::candidates(observation, settings));
    result
}

/// How many observed hostile entities one catalogue reports as attack options.
pub const MAX_ATTACK_CANDIDATES: usize = 4;

/// Everything the model needs in order to judge any entry: what it holds, its body, the
/// time of day and how long until dusk, and the observed threats. Facts only.
pub fn facts(observation: &Observation) -> String {
    let held = if observation.items.is_empty() {
        "holding nothing".to_owned()
    } else {
        format!(
            "holding {}",
            observation
                .items
                .iter()
                .filter(|(_, count)| **count > 0)
                .map(|(item, count)| format!("{item} x{count}"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let body = if observation.health.is_finite() && observation.food.is_finite() {
        format!(
            "health {:.0} of 20, food {:.0} of 20",
            observation.health, observation.food
        )
    } else {
        "body unknown".to_owned()
    };
    let clock = match (observation.time_of_day, daylight_left(observation)) {
        (Some(now), Some(left)) => format!(
            "day tick {now} ({}), {left} tick(s) of daylight left",
            if is_night(observation) {
                "night"
            } else {
                "day"
            }
        ),
        _ => "time of day not sent by the server".to_owned(),
    };
    let threats = observation
        .entities
        .iter()
        .filter(|entity| crate::survival::is_hostile(&entity.name))
        .take(MAX_ATTACK_CANDIDATES)
        .map(|entity| {
            format!(
                "{} {:.1} blocks away",
                entity.name,
                distance(&observation.position, &entity.position)
            )
        })
        .collect::<Vec<_>>();
    let threats = if threats.is_empty() {
        "no hostile entity observed".to_owned()
    } else {
        format!(
            "threats: {}{}",
            threats.join(", "),
            if observation.entity_scan_incomplete {
                " (entity scan truncated, unseen threats possible)"
            } else {
                ""
            }
        )
    };
    let options = harvest_options(observation);
    format!(
        "{held}; {body}; {clock}; {threats}; {} observed harvest target(s), {} observed block position(s), {} observed item pile(s)",
        options.len(),
        observation.blocks.len(),
        observation.resources.len(),
    )
}

/// Which weapon the bot is holding, and how many. "Do I have a weapon for this zombie"
/// is answerable from the entry, not from a guess.
pub fn weapon_state(observation: &Observation) -> String {
    let weapons: Vec<String> = observation
        .items
        .iter()
        .filter(|(item, count)| {
            **count > 0
                && (item.ends_with("_sword")
                    || item.ends_with("_axe")
                    || item.ends_with("_pickaxe")
                    || item.ends_with("_shovel")
                    || item.ends_with("_hoe")
                    || item.ends_with("_trident")
                    || item.ends_with("bow"))
        })
        .map(|(item, count)| format!("{item} x{count}"))
        .collect();
    if weapons.is_empty() {
        "no weapon held (bare hands)".to_owned()
    } else {
        weapons.join(", ")
    }
}

/// Observed attack possibilities remain descriptive knowledge until an attack
/// executor exists; they must not be selectable movement-only impersonations.
pub fn attacks(observation: &Observation, settings: &Settings) -> Vec<Candidate> {
    let _ = settings;
    let weapon = weapon_state(observation);
    observation
        .entities
        .iter()
        .filter(|entity| crate::survival::is_hostile(&entity.name))
        .take(MAX_ATTACK_CANDIDATES)
        .enumerate()
        .map(|(index, entity)| {
            let range = distance(&observation.position, &entity.position);
            Candidate {
                skill: None,
                id: format!("attack_{index}"),
                description: format!(
                    "Observed {} {range:.1} blocks away; held weapon: {weapon}; health {:.0} of 20, food {:.0} of 20. Attack knowledge is capped at {MAX_ATTACK_CANDIDATES} per observation. No attack executor in this build; attack is not selectable, while verified movement and flight remain available.",
                    entity.name, observation.health, observation.food,
                ),
                target: Some(entity.position.clone()),
                duration_ms: 10_000,
            }
        })
        .collect()
}

/// Staying put is a legitimate choice and is offered again, with the facts in front of
/// the model. It is never the only entry: every other category is offered alongside it.
pub fn wait(observation: &Observation) -> Candidate {
    Candidate {
        skill: None,
        id: "wait".into(),
        description: format!(
            "Stay where you are and keep observing. {} Nothing is spent and nothing changes; choose this when waiting is genuinely the best move. This entry never hides the other options: they are all in the same list.",
            facts(observation)
        ),
        target: None,
        duration_ms: 2_000,
    }
}

/// Observed move targets and flight goals, in the same order and with the same ids the
/// engine has always offered (`waypoint_<index>`, `flee_<index>`). Attack knowledge
/// is kept in the observation, not offered without an attack executor. A short pause
/// for the next observation is engine timing, not a strategic candidate for Jev.
pub fn moves(observation: &Observation, duration_ms: u64, settings: &Settings) -> Vec<Candidate> {
    let demo = settings.mode == crate::model::Mode::Demo;
    let mut result: Vec<_> = observation
        .blocks
        .iter()
        .enumerate()
        .filter(|(_, landmark)| {
            let p = &landmark.position;
            (demo || landmark.name.starts_with("waypoint:"))
                && p.x.is_finite()
                && p.y.is_finite()
                && p.z.is_finite()
                && distance(&observation.position, p) <= 12.0
                && distance(&observation.position, p) > crate::model::ARRIVAL_TOLERANCE_M
        })
        .take(MAX_MOVE_CANDIDATES)
        .map(|(index, landmark)| {
            let measured = distance(&observation.position, &landmark.position);
            Candidate {
                skill: None,
                id: format!("waypoint_{index}"),
                description: format!(
                    "Navigate toward observed location: {} ({measured:.1} blocks away); pathfinder must not mine. Move options are capped at {MAX_MOVE_CANDIDATES} per observation (waypoint {} of that cap).",
                    landmark.name,
                    index + 1,
                ),
                target: Some(landmark.position.clone()),
                duration_ms: duration_ms.clamp(1, 10_000),
            }
        })
        .collect();
    result.extend(crate::survival::flee_candidates(observation, duration_ms));
    result
}

/// How many observed move targets one catalogue reports. Bounded so the list cannot
/// grow without limit, and stated in every move entry.
pub const MAX_MOVE_CANDIDATES: usize = 24;

fn distance(a: &Position, b: &Position) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2) + (a.z - b.z).powi(2)).sqrt()
}
