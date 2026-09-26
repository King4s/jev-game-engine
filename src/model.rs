use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
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

#[derive(Clone, Debug, Serialize, Deserialize)]
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
    pub entities: Vec<Landmark>,
    pub note: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Candidate {
    pub id: String,
    pub description: String,
    pub target: Option<Position>,
    pub duration_ms: u64,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
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
            legacy_forwarding: false,
            fixture_waypoint: FixtureWaypoint::Distant,
            mode: Mode::Demo,
            port: 25565,
            bot_name: "JevBot".into(),
            max_requests: 30,
            max_seconds: 300,
            objective: String::new(),
            request_interval_ms: 0,
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
    /// Current legal manual choices, bound to this snapshot's observation.
    pub manual_candidates: Vec<Candidate>,
    pub status: String,
    pub mode: Mode,
    pub observation: Option<Observation>,
    pub events: Vec<Event>,
    pub requests: u32,
    pub active_goal: Option<String>,
    pub last_error: Option<String>,
    pub recording_path: Option<String>,
    pub p50_ms: Option<u64>,
    pub p95_ms: Option<u64>,
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
            manual_candidates: vec![],
            status: "Ready".into(),
            mode: Mode::Demo,
            observation: None,
            events: vec![],
            requests: 0,
            active_goal: None,
            last_error: None,
            recording_path: None,
            p50_ms: None,
            p95_ms: None,
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
