//! One engine over the whole world: the same verified stance-and-route machinery for any
//! harvestable block, one mover for every movement, and one catalogue that offers the
//! model the goals and the immediate choices the observed world actually makes possible.
use jev_game_engine::catalog::{self, GoalExecutor, GoalKind, Group, MAX_MOVE_CANDIDATES};
use jev_game_engine::model::{
    CraftingSlot, Landmark, Mode, Observation, Position, Settings, WoodSkill,
};
use jev_game_engine::motion::{self, Plan, Progress};
use jev_game_engine::refusal;
use jev_game_engine::wood;
use jev_game_engine::wood_tree::{self, Cell, Terrain};
use std::collections::BTreeMap;

const OAK: &str = "minecraft:oak_log";
const STONE: &str = "minecraft:stone";
const DIRT: &str = "minecraft:dirt";

/// Flat walkable ground with air above it, the same shape the wood geometry tests use.
fn ground() -> BTreeMap<Cell, Terrain> {
    let mut cells = BTreeMap::new();
    for x in -12..=12 {
        for z in -12..=12 {
            for y in -8..=8 {
                cells.insert(
                    (x, y, z),
                    Terrain {
                        air: y >= 0,
                        support: y < 0,
                        log: None,
                    },
                );
            }
        }
    }
    cells
}

fn block(cells: &mut BTreeMap<Cell, Terrain>, p: Cell, name: &str) {
    cells.insert(
        p,
        Terrain {
            air: false,
            support: true,
            log: Some(name.into()),
        },
    );
}

/// The whole inspection pipeline the live adapter uses, over a synthetic world.
fn trees(cells: &BTreeMap<Cell, Terrain>) -> Vec<wood_tree::TreeView> {
    wood_tree::inspect(
        &Position {
            x: 0.5,
            y: 0.0,
            z: 0.5,
        },
        |p| cells.get(&p).cloned(),
    )
}

fn slots(items: &[(&str, u32)]) -> Vec<CraftingSlot> {
    let mut slots: Vec<CraftingSlot> = (9..=44)
        .map(|slot| CraftingSlot {
            slot,
            item: String::new(),
            count: 0,
        })
        .collect();
    for (index, (item, count)) in items.iter().enumerate() {
        slots[index].item = (*item).into();
        slots[index].count = *count;
    }
    slots
}

fn live_settings() -> Settings {
    Settings {
        wood_skills: true,
        mode: Mode::Live,
        ..Settings::default()
    }
}

fn observation(trees: Vec<wood_tree::TreeView>, items: &[(&str, u32)]) -> Observation {
    Observation {
        world_epoch: 1,
        dimension: Some("minecraft:overworld".into()),
        sequence: 9,
        connected: true,
        position: Position {
            x: 0.5,
            y: 0.0,
            z: 0.5,
        },
        health: 20.,
        food: 20.,
        note: "unified catalogue fixture".into(),
        time_of_day: Some(1_000),
        items: items
            .iter()
            .map(|(item, count)| ((*item).to_owned(), *count))
            .collect(),
        crafting_inventory: Some(slots(items)),
        trees,
        ..Default::default()
    }
}

#[test]
fn technical_observation_pause_is_not_a_model_choice() {
    let observed = observation(Vec::new(), &[]);
    let candidates = catalog::candidates(&observed, 2_000, &live_settings());
    assert!(candidates.iter().all(|candidate| candidate.id != "wait"));
}

#[test]
fn earlier_soil_targets_do_not_hide_a_verified_oak_log() {
    let make_tree = |wood: &str, x: f64| wood_tree::TreeView {
        id: x as u64,
        wood: wood.into(),
        logs: vec![Position { x, y: 0.5, z: 0.5 }],
        incomplete: false,
        capacity_loss: false,
        status: "verified".into(),
        next: vec![wood_tree::Access {
            target: Position { x, y: 0.5, z: 0.5 },
            log: wood.into(),
            route: vec![Position {
                x: x - 1.0,
                y: 0.0,
                z: 0.5,
            }],
        }],
        diagnostics: Default::default(),
    };
    let observed = observation(
        vec![
            make_tree("minecraft:grass_block", 2.5),
            make_tree("minecraft:dirt", 3.5),
            make_tree("minecraft:grass_block", 4.5),
            make_tree("minecraft:oak_log", 5.5),
        ],
        &[],
    );
    let candidates = catalog::candidates(&observed, 2_000, &live_settings());
    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.id == "gather_5_0_0"),
        "a verified observed oak must not disappear behind earlier soil: {candidates:?}"
    );
}

#[test]
fn an_observed_stone_block_yields_a_harvest_candidate_with_a_verified_route() {
    let mut cells = ground();
    block(&mut cells, (2, 0, 0), STONE);
    let trees = trees(&cells);
    assert_eq!(trees.len(), 1, "one observed component: {trees:?}");
    assert_eq!(trees[0].wood, STONE);
    assert!(
        !trees[0].next.is_empty(),
        "the verified stance machinery must offer the stone: {trees:?}"
    );
    assert!(
        !trees[0].next[0].route.is_empty(),
        "the offered stone must carry its route"
    );

    let observation = observation(trees, &[]);
    let candidates = wood::gather_candidates(&observation, &live_settings());
    assert_eq!(candidates.len(), 1, "{candidates:?}");
    assert_eq!(candidates[0].id, "gather_2_0_0");
    match candidates[0].skill.as_ref() {
        Some(WoodSkill::Gather { log, route, .. }) => {
            assert_eq!(log, STONE);
            assert!(
                !route.is_empty(),
                "the candidate carries its verified route"
            );
        }
        other => panic!("expected a gather skill for the stone: {other:?}"),
    }
    assert!(
        candidates[0].description.contains(STONE),
        "the entry names the block: {}",
        candidates[0].description
    );
    assert!(
        candidates[0].description.contains("minecraft:cobblestone"),
        "the entry names the real drop: {}",
        candidates[0].description
    );
    assert_eq!(
        wood::drop_for_block(STONE).as_deref(),
        Some("minecraft:cobblestone")
    );
}

#[test]
fn an_observed_dirt_block_yields_a_candidate_and_its_drop_is_itself() {
    let mut cells = ground();
    block(&mut cells, (0, 0, 2), DIRT);
    let trees = trees(&cells);
    let observation = observation(trees, &[]);
    let candidates = wood::gather_candidates(&observation, &live_settings());
    assert_eq!(candidates.len(), 1, "{candidates:?}");
    assert_eq!(candidates[0].id, "gather_0_0_2");
    assert!(
        candidates[0].description.contains(DIRT),
        "{}",
        candidates[0].description
    );
    assert_eq!(wood::drop_for_block(DIRT).as_deref(), Some(DIRT));
    // The grass block is the same machinery with a different drop: a block-agnostic
    // harvester confirms the item the block really yields.
    assert_eq!(
        wood::drop_for_block("minecraft:grass_block").as_deref(),
        Some("minecraft:dirt")
    );
}

#[test]
fn a_log_a_stone_block_and_a_dirt_block_are_three_separate_choices_with_their_own_drops() {
    let mut cells = ground();
    block(&mut cells, (2, 0, 0), OAK);
    block(&mut cells, (0, 0, 2), STONE);
    block(&mut cells, (-2, 0, 0), DIRT);
    let observation = observation(trees(&cells), &[]);

    let options = catalog::harvest_options(&observation);
    assert_eq!(
        options.len(),
        3,
        "every observed block is reported: {options:?}"
    );
    let drops: BTreeMap<&str, &str> = options
        .iter()
        .map(|option| (option.block.as_str(), option.drop.as_str()))
        .collect();
    assert_eq!(drops[OAK], OAK);
    assert_eq!(drops[STONE], "minecraft:cobblestone");
    assert_eq!(drops[DIRT], DIRT);
    assert_eq!(
        options
            .iter()
            .filter(|option| option.group == Group::Log)
            .count(),
        1
    );
    assert_eq!(
        options
            .iter()
            .filter(|option| option.group == Group::Stone)
            .count(),
        1
    );
    assert_eq!(
        options
            .iter()
            .filter(|option| option.group == Group::Soil)
            .count(),
        1
    );

    let candidates = catalog::candidates(&observation, 2_000, &live_settings());
    let gathers: Vec<_> = candidates
        .iter()
        .filter(|candidate| candidate.id.starts_with("gather_"))
        .collect();
    assert_eq!(
        gathers.len(),
        3,
        "all three observed blocks must be individually choosable: {candidates:?}"
    );
    let ids: Vec<_> = gathers.iter().map(|c| c.id.as_str()).collect();
    let unique: std::collections::BTreeSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), 3, "each block keeps its own id: {ids:?}");
    assert!(
        gathers
            .iter()
            .any(|c| c.description.contains(STONE) && c.description.contains("cobblestone"))
    );
    assert!(
        gathers.iter().any(
            |c| c.description.contains(DIRT) && c.description.contains("yields minecraft:dirt")
        )
    );
}

#[test]
fn the_catalogue_is_stable_and_capped_in_every_category_it_offers() {
    let mut cells = ground();
    block(&mut cells, (2, 0, 0), OAK);
    let mut observation = observation(trees(&cells), &[("minecraft:oak_log", 2)]);
    // More observed move targets than the documented move cap, all inside 12 blocks.
    for step in -8..=8 {
        for z in [0.5_f64, 2.5, 4.5] {
            observation.blocks.push(Landmark {
                name: format!("waypoint:{step}:0:{}", z as i32),
                position: Position {
                    x: f64::from(step) + 0.5,
                    y: 0.0,
                    z,
                },
            });
        }
    }
    let first = catalog::candidates(&observation, 2_000, &live_settings());
    let second = catalog::candidates(&observation, 2_000, &live_settings());
    let ids: Vec<_> = first.iter().map(|c| c.id.clone()).collect();
    let again: Vec<_> = second.iter().map(|c| c.id.clone()).collect();
    assert_eq!(
        ids, again,
        "the same observation must produce the same list"
    );

    let moves: Vec<_> = first
        .iter()
        .filter(|c| c.id.starts_with("waypoint_"))
        .collect();
    assert_eq!(moves.len(), MAX_MOVE_CANDIDATES, "the move cap is enforced");
    assert!(
        moves[0]
            .description
            .contains(&MAX_MOVE_CANDIDATES.to_string()),
        "the cap is stated in the entry: {}",
        moves[0].description
    );
    assert_eq!(
        first.iter().filter(|c| c.id.starts_with("gather_")).count(),
        1
    );
    assert_eq!(
        first.iter().filter(|c| c.id == "wait").count(),
        0,
        "technical pauses stay inside the engine"
    );
}

#[test]
fn an_open_daylight_world_offers_the_wood_goal_first_and_dusk_offers_shelter() {
    let mut cells = ground();
    block(&mut cells, (2, 0, 0), OAK);
    block(&mut cells, (0, 0, 2), STONE);
    block(&mut cells, (-2, 0, 0), DIRT);
    let peaceful = observation(trees(&cells), &[]);
    let goals = catalog::goals(&peaceful, &live_settings());
    let ids: Vec<_> = goals.iter().map(|goal| goal.id.as_str()).collect();
    assert!(
        ids.first() == Some(&"goal_wood"),
        "with a tree observed and a full day left, wood comes first: {ids:?}"
    );
    let gather_goals: Vec<_> = ids
        .iter()
        .filter(|id| {
            GoalKind::from_id(id)
                .map(|kind| kind.group().is_some())
                .unwrap_or(false)
        })
        .collect();
    assert_eq!(gather_goals.first(), Some(&&"goal_wood"));
    assert!(ids.contains(&"goal_stone") && ids.contains(&"goal_soil"));
    assert!(goals[0].description.contains("bounded catalogue"));
    assert!(
        goals[0].description.contains("about 60 s"),
        "a goal states how long it will run: {}",
        goals[0].description
    );

    // Little daylight left and blocks in hand: shelter must be on the list.
    let mut dusk = observation(trees(&cells), &[(DIRT, 10)]);
    dusk.time_of_day = Some(11_500);
    let dusk_goals: Vec<_> = catalog::goals(&dusk, &live_settings())
        .into_iter()
        .map(|goal| goal.id)
        .collect();
    assert!(
        dusk_goals.contains(&"goal_shelter".to_string()),
        "dusk with blocks in hand must offer shelter: {dusk_goals:?}"
    );
    let (shelter, _) = catalog::goals(&dusk, &live_settings())
        .into_iter()
        .find(|goal| goal.id == "goal_shelter")
        .map(|goal| (goal.description, goal.duration_ms))
        .expect("shelter goal");
    assert!(
        shelter.contains("tick(s) of daylight left"),
        "the goal states the daylight it is racing: {shelter}"
    );
    assert_eq!(catalog::daylight_left(&dusk), Some(500));
}

#[test]
fn a_threatened_bot_receives_attack_and_flee_with_its_weapon_visible_either_way() {
    let mut cells = ground();
    block(&mut cells, (2, 0, 0), OAK);
    let trees = trees(&cells);
    for held in [vec![("minecraft:stone_sword", 1)], vec![]] {
        let mut observation = observation(trees.clone(), &held);
        observation.entities.push(Landmark {
            name: "Zombie".into(),
            position: Position {
                x: 6.0,
                y: 0.0,
                z: 0.5,
            },
        });
        for x in [-2.5_f64, 2.5] {
            observation.blocks.push(Landmark {
                name: format!("waypoint:{x}:0:0"),
                position: Position { x, y: 0.0, z: 0.5 },
            });
        }
        let candidates = catalog::candidates(&observation, 2_000, &live_settings());
        let attack = catalog::attacks(&observation, &live_settings())
            .into_iter()
            .next()
            .expect("the observed threat remains knowledge even without an attack executor");
        assert!(
            candidates.iter().all(|c| !c.id.starts_with("attack_")),
            "a movement-only implementation must not be offered as an attack"
        );
        let flee = candidates
            .iter()
            .find(|c| c.id == "flee_0")
            .expect("a flight option for the observed threat");
        let weapon = catalog::weapon_state(&observation);
        assert!(
            attack.description.contains(&weapon),
            "the attack entry names what is held ({weapon}): {}",
            attack.description
        );
        assert!(flee.description.contains("Zombie"));
        if held.is_empty() {
            assert!(
                attack.description.contains("bare hands"),
                "holding nothing is stated, not guessed: {}",
                attack.description
            );
        } else {
            assert!(attack.description.contains("minecraft:stone_sword x1"));
        }
    }
}

#[test]
fn a_threat_does_not_hide_any_other_category_or_offer_a_technical_pause() {
    let mut cells = ground();
    block(&mut cells, (2, 0, 0), OAK);
    block(&mut cells, (0, 0, 2), STONE);
    let mut observation = observation(trees(&cells), &[("minecraft:oak_log", 2)]);
    observation.entities.push(Landmark {
        name: "Zombie".into(),
        position: Position {
            x: 6.0,
            y: 0.0,
            z: 0.5,
        },
    });
    observation.blocks.push(Landmark {
        name: "waypoint:-2:0:0".into(),
        position: Position {
            x: -2.5,
            y: 0.0,
            z: 0.5,
        },
    });
    let candidates = catalog::candidates(&observation, 2_000, &live_settings());
    assert!(
        catalog::goals(&observation, &live_settings())
            .iter()
            .any(|goal| goal.id.starts_with("goal_") && goal.description.contains("Knowledge only")),
        "the threat must not hide strategic goal knowledge"
    );
    assert!(
        candidates
            .iter()
            .all(|candidate| !candidate.id.starts_with("goal_")),
        "a knowledge-only goal cannot be selected as an action"
    );
    for category in ["waypoint_", "flee_", "gather_", "craft_"] {
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.id.starts_with(category)),
            "a nearby threat must not hide {category}: {candidates:?}"
        );
    }
    assert!(candidates.iter().all(|candidate| candidate.id != "wait"));
    let gather = candidates
        .iter()
        .find(|candidate| candidate.id.starts_with("gather_"))
        .expect("work option with a threat nearby");
    assert!(
        gather.description.contains("Zombie") && gather.description.contains("Observed risk"),
        "the threat is stated as a fact on the work option: {}",
        gather.description
    );
}

/// A scripted world for the mover: it records what the one loop asked of it.
struct ScriptedNav {
    position: Position,
    target: Option<Position>,
    grounded: bool,
    verify_calls: usize,
    steps: usize,
    goals: Vec<Position>,
}

impl ScriptedNav {
    fn new(position: Position) -> Self {
        Self {
            position,
            target: None,
            grounded: true,
            verify_calls: 0,
            steps: 0,
            goals: Vec::new(),
        }
    }
}

impl motion::Nav for ScriptedNav {
    fn position(&self) -> Result<Position, &'static str> {
        Ok(self.position.clone())
    }

    fn grounded(&self) -> Result<bool, &'static str> {
        Ok(self.grounded)
    }

    fn verify_route(&mut self, route: &[Position]) -> Result<(), &'static str> {
        self.verify_calls += route.len();
        Ok(())
    }

    fn observed_step(
        &mut self,
        _position: &Position,
        _from: &Position,
        to: &Position,
        _grounded: bool,
    ) -> Result<bool, &'static str> {
        self.target = Some(to.clone());
        Ok(true)
    }

    fn step(&mut self, forward: bool, _jump: bool) {
        if !forward {
            return;
        }
        self.steps += 1;
        let Some(target) = self.target.clone() else {
            return;
        };
        let dx = target.x - self.position.x;
        let dz = target.z - self.position.z;
        if dx.abs() >= dz.abs() && dx != 0.0 {
            self.position.x += dx.signum() * 0.5;
        } else if dz != 0.0 {
            self.position.z += dz.signum() * 0.5;
        } else {
            self.position = target;
        }
    }

    fn start_goal(&mut self, target: &Position) -> Result<(), &'static str> {
        self.goals.push(target.clone());
        Ok(())
    }
}

fn cell_position(x: f64, y: f64, z: f64) -> Position {
    Position { x, y, z }
}

#[test]
fn waypoint_harvest_and_flight_all_travel_through_the_single_mover() {
    // 1. A waypoint: an observed standable cell handed to the world's own pathfinder.
    let goal = cell_position(1.5, 0.0, 0.5);
    let mut nav = ScriptedNav::new(cell_position(0.5, 0.0, 0.5));
    let plan = Plan::Goal { target: &goal };
    assert_eq!(plan.kind(), "goal");
    assert_eq!(motion::follow(&mut nav, plan).unwrap(), Progress::Advancing);
    assert_eq!(
        nav.goals,
        vec![goal.clone()],
        "one goal handed to the world"
    );

    // 2. Flight: the same goal entry point, a different destination.
    let escape = cell_position(-3.5, 0.0, 0.5);
    assert_eq!(
        motion::follow(&mut nav, Plan::Goal { target: &escape }).unwrap(),
        Progress::Advancing
    );
    assert_eq!(nav.goals.last(), Some(&escape));

    // 3. A harvest approach: the verified route, walked by the same loop.
    let route = vec![
        cell_position(0.5, 0.0, 0.5),
        cell_position(1.5, 0.0, 0.5),
        cell_position(2.5, 0.0, 0.5),
    ];
    let mut index = 0;
    let mut ticks = 0;
    loop {
        ticks += 1;
        assert!(ticks <= 32, "the route must terminate");
        if motion::follow(
            &mut nav,
            Plan::Route {
                route: &route,
                index: &mut index,
            },
        )
        .unwrap()
            == Progress::Arrived
        {
            break;
        }
    }
    assert_eq!(index + 1, route.len(), "the route ended at its last cell");
    assert!(
        nav.verify_calls >= route.len(),
        "every step revalidates the route"
    );
    assert!(nav.steps >= 2, "the loop walked the verified edges");
    assert_eq!(
        nav.goals.len(),
        2,
        "no extra goal was invented by the route"
    );

    // An arrival is decided by the mover's own tolerance, not by the caller.
    nav.position = goal.clone();
    assert_eq!(
        motion::follow(&mut nav, Plan::Goal { target: &goal }).unwrap(),
        Progress::Arrived
    );
    assert_eq!(nav.goals.len(), 2, "an arrived goal is not re-queued");
    assert_eq!(motion::PLAYER_HALF_WIDTH, 0.3);
}

#[test]
fn a_route_that_leaves_the_verified_transition_is_refused() {
    let route = vec![cell_position(0.5, 0.0, 0.5), cell_position(1.5, 0.0, 0.5)];
    let mut nav = ScriptedNav::new(cell_position(4.5, 0.0, 0.5));
    let mut index = 0;
    assert!(
        motion::follow(
            &mut nav,
            Plan::Route {
                route: &route,
                index: &mut index,
            },
        )
        .is_err(),
        "a body outside its own transition must be refused"
    );
}

/// A world that makes as many rungs of the ladder possible as an observation can: a log, a
/// stone block and soil to harvest, logs and planks and a torch in hand, dusk close and food
/// low, so every goal kind this build can offer is on the list.
fn rich_observation() -> Observation {
    let mut cells = ground();
    block(&mut cells, (2, 0, 0), OAK);
    block(&mut cells, (0, 0, 2), STONE);
    block(&mut cells, (-2, 0, 0), DIRT);
    let mut observation = observation(
        trees(&cells),
        &[
            ("minecraft:oak_log", 4),
            ("minecraft:oak_planks", 4),
            (DIRT, 12),
            ("minecraft:torch", 1),
        ],
    );
    observation.time_of_day = Some(11_500);
    observation.food = 6.0;
    observation
}

/// The catalogue must not offer work the engine cannot do. A goal whose executor does not
/// exist is offered as knowledge only and says so in its own entry, and the local guard
/// refuses it by name instead of with a sentence that fits every failure.
#[test]
fn a_goal_this_build_cannot_carry_out_is_marked_knowledge_only_and_refused_by_name() {
    let mut cells = ground();
    block(&mut cells, (2, 0, 0), OAK);
    block(&mut cells, (0, 0, 2), STONE);
    block(&mut cells, (-2, 0, 0), DIRT);
    let soil = catalog::goals(&observation(trees(&cells), &[]), &live_settings())
        .into_iter()
        .find(|goal| goal.id == "goal_soil")
        .expect("an observed soil block offers the soil goal");

    assert!(
        soil.description.contains("Knowledge only"),
        "a goal this build cannot carry out must state that in its own entry: {}",
        soil.description
    );
    assert!(
        !soil
            .description
            .contains("carries this goal out on its own"),
        "the entry must not promise work nothing does: {}",
        soil.description
    );
    assert_eq!(
        catalog::goal_executor(GoalKind::Soil),
        GoalExecutor::KnowledgeOnly
    );

    let refusal = refusal::admit(&soil, Some(20.0), None)
        .expect_err("no executor in this build runs a goal-kind candidate");
    assert_eq!(
        refusal,
        refusal::Refusal::NoLocalExecutor {
            candidate: "goal_soil".into()
        }
    );
    assert_eq!(
        refusal.to_string(),
        "Local guard rejected action 'goal_soil': no local executor in this build runs it (no skill, no target, and it is not a bounded wait/stop action)"
    );
}

/// Every offered goal states whether this build can carry it out, and the statement matches
/// what the local guard actually does with it.
#[test]
fn every_offered_goal_states_the_executor_this_build_has_for_it() {
    let observation = rich_observation();
    let goals = catalog::goals(&observation, &live_settings());
    assert!(
        goals.len() >= 5,
        "the rich world must offer several goals: {:?}",
        goals.iter().map(|goal| &goal.id).collect::<Vec<_>>()
    );
    for goal in &goals {
        let kind = GoalKind::from_id(&goal.id).expect("every goal entry carries a goal id");
        match catalog::goal_executor(kind) {
            GoalExecutor::KnowledgeOnly => {
                assert!(
                    goal.description.contains("Knowledge only"),
                    "{} is not executable here and does not say so: {}",
                    goal.id,
                    goal.description
                );
                assert_eq!(
                    refusal::admit(goal, Some(20.0), None),
                    Err(refusal::Refusal::NoLocalExecutor {
                        candidate: goal.id.clone()
                    }),
                    "the catalogue says {} is knowledge only, so the guard must refuse it by name",
                    goal.id
                );
            }
            GoalExecutor::Steps => assert!(
                goal.description
                    .contains("carries this goal out on its own"),
                "{} is offered as work and must say the engine carries it out: {}",
                goal.id,
                goal.description
            ),
        }
    }
}

/// Whether a goal kind has steps this build derives is one fact, and whether anything
/// dispatches them is another. Both are stated in one place so the catalogue cannot promise
/// work the engine does not do, and this pins the second fact: `next_step` and `goal_steps`
/// are derived and nothing dispatches them, so no goal kind is work yet. Wiring a goal loop is
/// what changes it, and it changes this test on purpose.
#[test]
fn a_goal_is_work_only_when_this_build_dispatches_its_steps() {
    let observation = rich_observation();
    for kind in GoalKind::LADDER {
        let derives_steps = !catalog::goal_steps(kind, &observation, &live_settings()).is_empty();
        assert_eq!(
            catalog::resolves_steps(kind),
            derives_steps,
            "{kind:?} disagrees with itself about whether this build can derive its steps"
        );
        assert_eq!(
            catalog::goal_executor(kind),
            GoalExecutor::KnowledgeOnly,
            "{kind:?} cannot be work while no call site dispatches a goal's steps"
        );
    }
}
