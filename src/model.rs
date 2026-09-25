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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub legacy_forwarding: bool,
    pub mode: Mode,
    pub port: u16,
    pub bot_name: String,
    pub max_requests: u32,
    pub max_seconds: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            legacy_forwarding: false,
            mode: Mode::Demo,
            port: 25565,
            bot_name: "JevBot".into(),
            max_requests: 30,
            max_seconds: 300,
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
