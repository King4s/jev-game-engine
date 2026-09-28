use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Position {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Landmark {
    pub name: String,
    pub position: Position,
}

/// A useful block or animal near the bot, reported as one of the nearest of its kind.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Resource {
    /// Survival category, for example `log`, `iron_ore`, `bed` or `sheep`.
    pub kind: String,
    /// Registry id of the block or entity, for example `minecraft:oak_log`.
    pub name: String,
    pub position: Position,
    /// Distance in blocks from the observed bot position to `position`.
    pub distance_m: f64,
}

/// `Default` exists so code that builds an observation can name only the fields it
/// knows; the default is a disconnected, empty observation, never a plausible world.
fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Observation {
    /// Adapter-local world lifecycle; changes even on same-dimension respawn.
    #[serde(default)]
    pub world_epoch: u64,
    /// Server-reported dimension identifier; absent in legacy recordings.
    #[serde(default)]
    pub dimension: Option<String>,
    /// Deaths the adapter has seen this connection. Monotonic, so a death survives even
    /// when the respawn observation replaces the dying one before the engine reads it;
    /// absent (zero) in older recordings.
    #[serde(default)]
    pub deaths: u64,
    pub sequence: u64,
    pub connected: bool,
    pub position: Position,
    pub health: f32,
    pub food: f32,
    pub inventory: Vec<String>,
    pub blocks: Vec<Landmark>,
    /// Current visible native state facts, capped at 16; absent in legacy recordings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub block_facts: Vec<crate::perception::BlockFacts>,
    pub entities: Vec<Landmark>,
    /// Bounded native entity scan omitted possible actors; absence is not evidence of safety.
    #[serde(default, skip_serializing_if = "is_false")]
    pub entity_scan_incomplete: bool,
    pub note: String,
    /// Tick within the 24,000-tick day cycle, as last sent by the server. `None` until
    /// the server has sent the time, and in recordings made before it was observed.
    #[serde(default)]
    pub time_of_day: Option<u64>,
    /// Registry id of the item in the selected hotbar slot; `None` when the hand is
    /// empty or the inventory is unknown.
    #[serde(default)]
    pub held_item: Option<String>,
    /// Inventory totals by registry item id, for example `minecraft:oak_log: 12`.
    #[serde(default)]
    pub items: BTreeMap<String, u32>,
    /// Nearest useful blocks and animals, at most three per kind, sorted by distance.
    #[serde(default)]
    pub resources: Vec<Resource>,
    #[serde(default)]
    pub trees: Vec<crate::wood_tree::TreeView>,
    /// Fresh, visible item and reversible route after an interrupted gather.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recoverable_drop: Option<RecoverableDrop>,
    /// Packet-confirmed idle player inventory; absent for legacy/unknown/busy menus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crafting_inventory: Option<Vec<CraftingSlot>>,
    /// Adapter-observed terminal result; never a navigation arrival.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_outcome: Option<SkillOutcome>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RecoverableDrop {
    pub log: String,
    pub position: Position,
    pub route: Vec<Position>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Candidate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill: Option<WoodSkill>,
    pub id: String,
    pub description: String,
    pub target: Option<Position>,
    pub duration_ms: u64,
}

/// Fully bound, opt-in wood operation. Gather targets are exact log centers; crafting has no world target.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WoodSkill {
    CraftPlanks {
        log: String,
        planks: String,
    },
    CraftTable {
        planks: [String; 4],
    },
    PlaceTable {
        support: Position,
        source_slot: u16,
        hotbar_slot: u8,
    },
    Gather {
        log: String,
        approach: Position,
        #[serde(default)]
        route: Vec<Position>,
    },
    RecoverDrop {
        log: String,
        route: Vec<Position>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CraftingSlot {
    pub slot: u16,
    pub item: String,
    pub count: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SkillOutcome {
    /// Unique dispatch attempt; zero in legacy recordings.
    #[serde(default)]
    pub attempt_id: u64,
    pub candidate_id: String,
    pub world_epoch: u64,
    pub success: bool,
    #[serde(default)]
    pub confirmed_by_server: bool,
    pub message: String,
}

fn default_allowed_dimension() -> String {
    "minecraft:overworld".into()
}

/// Engine-side arrival tolerance, in blocks, between the observed bot position and
/// the target bound to the accepted action. This is the single source of truth for
/// candidate filtering and for the recorded arrival verdict: the pathfinder stops
/// near a block centre rather than exactly on it, so the tolerance must not be
/// tightened without measured evidence.
pub const ARRIVAL_TOLERANCE_M: f64 = 0.6;

/// Local, machine-checkable outcome of one bounded navigation action. `arrived` is
/// decided by the engine from observed world state, never by another model request,
/// and `measured_distance_m` is absent when no connected observation was available.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ArrivalVerdict {
    pub target: Option<Position>,
    pub measured_distance_m: Option<f64>,
    pub tolerance_m: f64,
    pub duration_ms: u64,
    pub elapsed_ms: u64,
    pub arrived: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Decision {
    pub choice: String,
    pub probabilities: std::collections::BTreeMap<String, f64>,
    pub confidence: Option<f64>,
    pub model: String,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub latency_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Mode {
    Demo,
    Live,
}

/// Waypoint geometry the offline fixture is built with. `Distant` stays the default so the
/// desktop demo shows the honest expiry path; `Reachable` lets an operator or a test watch one
/// bounded goal arrive inside the same 2000 ms bound.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum FixtureWaypoint {
    #[default]
    Distant,
    Reachable,
}

fn default_route_min_confidence() -> f64 {
    0.5
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    /// Explicit permission for bounded log gathering and inventory crafting in the permitted dimension.
    #[serde(default)]
    pub wood_skills: bool,
    #[serde(default = "default_allowed_dimension")]
    pub allowed_dimension: String,
    #[serde(default)]
    pub legacy_forwarding: bool,
    /// Absent in recordings written before the offline demo could choose its geometry.
    #[serde(default)]
    pub fixture_waypoint: FixtureWaypoint,
    pub mode: Mode,
    pub port: u16,
    pub bot_name: String,
    pub max_requests: u32,
    pub max_seconds: u64,
    /// Operator-stated session goal. Empty means the session has none, which is the
    /// behaviour every earlier recording was made with. It reaches the model as
    /// structured state and is named in the question instructions; the engine never
    /// derives an action from it, because only the model's choice binds a candidate.
    #[serde(default)]
    pub objective: String,
    /// Minimum gap between model requests in a continuous session. `0` keeps the
    /// unpaced behaviour: the next request starts as soon as the previous bounded
    /// action ends. Pacing bounds provider spend over a long session.
    #[serde(default)]
    pub request_interval_ms: u64,
    /// Minimum handler confidence this run accepts before it will act. Jev Route sets the
    /// floor per run; 0.5 is the previous fixed policy and remains the default, so a run
    /// that says nothing behaves exactly as before.
    #[serde(default = "default_route_min_confidence")]
    pub route_min_confidence: f64,
    /// Opt-in engine safety reflex: when a hostile entity is inside the reflex radius
    /// and no navigation goal is in flight, the engine stops the session's in-flight
    /// work and dispatches one bounded flight goal itself, recorded with the
    /// `SAFETY-REFLEX` origin and no model request. Off by default, because it is the
    /// only path where an action's goal does not come from the model.
    #[serde(default)]
    pub safety_reflex: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            wood_skills: false,
            allowed_dimension: default_allowed_dimension(),
            legacy_forwarding: false,
            fixture_waypoint: FixtureWaypoint::Distant,
            mode: Mode::Demo,
            port: 25565,
            bot_name: "JevBot".into(),
            max_requests: 30,
            max_seconds: 300,
            objective: String::new(),
            request_interval_ms: 0,
            route_min_confidence: default_route_min_confidence(),
            safety_reflex: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub sequence: u64,
    pub elapsed_ms: u64,
    pub kind: String,
    pub message: String,
    pub observation: Option<Observation>,
    pub candidates: Vec<Candidate>,
    pub decision: Option<Decision>,
    /// Present on the event that ends a bounded navigation action; absent in
    /// recordings written before the verdict existed.
    #[serde(default)]
    pub arrival: Option<ArrivalVerdict>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Recording {
    pub schema_version: u32,
    pub id: String,
    pub settings: Settings,
    pub events: Vec<Event>,
}

#[derive(Clone, Debug)]
pub struct View {
    /// Recording identity for session-scoped telemetry counts.
    pub session_id: Option<String>,
    /// Current legal manual choices, bound to this snapshot's observation.
    pub manual_candidates: Vec<Candidate>,
    pub status: String,
    pub mode: Mode,
    pub observation: Option<Observation>,
    pub events: Vec<Event>,
    pub requests: u32,
    pub active_goal: Option<String>,
    pub last_error: Option<String>,
    /// Why the last chosen action was not executed, recorded verbatim the way `last_error`
    /// records an end reason — but a refusal is a technical answer, not a verdict on the run,
    /// so it is kept out of `last_error`, which names the reason a session stopped and is what
    /// the harness classifies. The run goes on and the concrete check stays readable (see
    /// `refusal::Refusal`).
    pub last_refusal: Option<String>,
    /// Chosen actions the local guard refused, each one recorded as `rejected` with the check
    /// that refused it. Counted like `rejected_answers`, so a run says how much work the
    /// engine could not carry out instead of leaving it in the recording alone.
    pub refused_actions: u32,
    pub recording_path: Option<String>,
    pub p50_ms: Option<u64>,
    pub p95_ms: Option<u64>,
    /// Measured p95 answer time of the world peer (client and adapter), once any action has
    /// been acknowledged. Reported so a run states the budget it actually used.
    pub client_p95_ms: Option<u64>,
    /// Budget derived from that measurement for acknowledging a dispatched action. Never a
    /// constant tuned to one connection: it is what this run measured.
    pub client_action_budget_ms: u64,
    /// Budget for observing the world, derived from the interval this adapter was measured
    /// to publish at (never from the action acknowledgement, which measures a different
    /// stream). Reported beside the client budget because a run judges both.
    pub observation_budget_ms: u64,
    pub goal_ms: u64,
    pub answer_age_limit_ms: u64,
    pub replay: bool,
    /// Operator-stated session goal in effect, empty when the session has none.
    pub objective: String,
    /// Engine safety-reflex actions dispatched in this session, each one recorded with
    /// its own origin. Distinguishes engine-initiated flight from model choices.
    pub reflexes: u32,
    /// Model answers that arrived but failed validation; dropped without acting.
    pub rejected_answers: u32,
}

impl Default for View {
    fn default() -> Self {
        Self {
            session_id: None,
            manual_candidates: vec![],
            status: "Ready".into(),
            mode: Mode::Demo,
            observation: None,
            events: vec![],
            requests: 0,
            active_goal: None,
            last_error: None,
            last_refusal: None,
            refused_actions: 0,
            recording_path: None,
            p50_ms: None,
            p95_ms: None,
            client_p95_ms: None,
            client_action_budget_ms: 1_000,
            observation_budget_ms: 1_000,
            goal_ms: 2000,
            answer_age_limit_ms: 1500,
            replay: false,
            objective: String::new(),
            reflexes: 0,
            rejected_answers: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Command {
    Connect(Settings),
    Start,
    Pause,
    Step,
    Stop,
    Reset,
    Manual {
        candidate: Candidate,
        world_epoch: u64,
        dimension: Option<String>,
    },
    Export,
    Replay(String),
}
