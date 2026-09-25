use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use tokio::sync::mpsc;

use crate::{
    adapter::{ActionRequest, AdapterCommand, AdapterHandle},
    latency::{LatencyPolicy, Timing, answer_is_current},
    model::*,
};

#[derive(Clone)]
pub struct EngineHandle {
    commands: mpsc::UnboundedSender<Command>,
    view: Arc<Mutex<View>>,
}

impl Default for EngineHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl EngineHandle {
    pub fn new() -> Self {
        let (commands, rx) = mpsc::unbounded_channel();
        let view = Arc::new(Mutex::new(View::default()));
        let shared = view.clone();
        std::thread::spawn(move || {
            match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime.block_on(Session::new(shared).run(rx)),
                Err(_) => {
                    if let Ok(mut view) = shared.lock() {
                        view.status = "Engine could not start".into();
                        view.last_error = Some("Could not create Tokio runtime".into());
                    }
                }
            }
        });
        Self { commands, view }
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    pub fn snapshot(&self) -> View {
        self.view
            .lock()
            .map(|view| view.clone())
            .unwrap_or_else(|_| View {
                status: "Engine status unavailable".into(),
                ..View::default()
            })
    }
}

struct Pending {
    generation: u64,
    started: Instant,
    observation: Observation,
    candidates: Vec<Candidate>,
    timing: Timing,
    receiver: mpsc::UnboundedReceiver<Result<Decision, String>>,
    task: tokio::task::JoinHandle<()>,
}

struct Dispatched {
    accepted_before: Instant,
    origin: String,
    candidate: Candidate,
    sent_at: Instant,
    receiver: tokio::sync::oneshot::Receiver<Result<Instant, &'static str>>,
}

struct Session {
    shared: Arc<Mutex<View>>,
    view: View,
    settings: Settings,
    adapter: Option<AdapterHandle>,
    pending: Option<Pending>,
    dispatched: Option<Dispatched>,
    generation: u64,
    continuous: bool,
    one_step: bool,
    active: Option<(Candidate, Instant)>,
    started: Instant,
    id: String,
    latency: LatencyPolicy,
    observed_at: Instant,
}

impl Session {
    fn new(shared: Arc<Mutex<View>>) -> Self {
        let view = View {
            answer_age_limit_ms: LatencyPolicy::default().timing().max_answer_age_ms,
            ..View::default()
        };
        Self {
            shared,
            view,
            settings: Settings::default(),
            adapter: None,
            pending: None,
            dispatched: None,
            generation: 0,
            continuous: false,
            one_step: false,
            active: None,
            started: Instant::now(),
            id: session_id(),
            latency: LatencyPolicy::default(),
            observed_at: Instant::now(),
        }
    }

    async fn run(mut self, mut commands: mpsc::UnboundedReceiver<Command>) {
        let mut tick = tokio::time::interval(Duration::from_millis(25));
        loop {
            tokio::select! {
                biased;
                command = commands.recv() => match command {
                    Some(command) => self.command(command),
                    None => { self.invalidate(true); break; }
                },
                _ = tick.tick() => self.tick(),
            }
            self.refresh_manual_candidates();
            if let Ok(mut shared) = self.shared.lock() {
                *shared = self.view.clone();
            }
        }
    }

    fn refresh_manual_candidates(&mut self) {
        self.view.manual_candidates = if !self.view.replay
            && self.adapter.is_some()
            && self.observed_at.elapsed() <= Duration::from_secs(1)
        {
            self.view
                .observation
                .as_ref()
                .filter(|o| o.connected)
                .map(|o| candidates(o, self.latency.timing().goal_ms, &self.settings.mode))
                .unwrap_or_default()
        } else {
            vec![]
        };
    }

    fn invalidate(&mut self, disconnect: bool) {
        self.generation = self.generation.wrapping_add(1);
        self.continuous = false;
        self.one_step = false;
        if let Some(pending) = self.pending.take() {
            pending.task.abort();
        }
        self.active = None;
        self.dispatched = None;
        self.view.active_goal = None;
        if let Some(adapter) = &self.adapter {
            let _ = adapter.commands.send(AdapterCommand::Stop);
            if disconnect {
                let _ = adapter.commands.send(AdapterCommand::Disconnect);
            }
        }
        if disconnect {
            self.adapter = None;
            if let Some(observation) = &mut self.view.observation {
                observation.connected = false;
            }
        }
    }

    fn event(
        &mut self,
        kind: &str,
        message: impl Into<String>,
        candidates: Vec<Candidate>,
        decision: Option<Decision>,
    ) {
        if self.view.events.len() >= 4_999 {
            self.invalidate(true);
            self.view.status = "Stopped: event budget reached".into();
            if self.view.events.len() == 4_999 {
                self.view.events.push(Event {
                    sequence: 5_000,
                    elapsed_ms: elapsed(self.started),
                    kind: "budget".into(),
                    message: "The 5000-event limit was reached; connection closed".into(),
                    observation: self.view.observation.clone(),
                    candidates: vec![],
                    decision: None,
                });
            }
            return;
        }
        self.view.events.push(Event {
            sequence: self.view.events.len() as u64 + 1,
            elapsed_ms: elapsed(self.started),
            kind: kind.into(),
            message: message.into(),
            observation: self.view.observation.clone(),
            candidates,
            decision,
        });
    }

    fn fail(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.invalidate(false);
        self.view.status = "Paused: error".into();
        self.view.last_error = Some(message.clone());
        self.event("error", message, vec![], None);
    }

    fn command(&mut self, command: Command) {
        match command {
            Command::Connect(settings) => {
                self.invalidate(true);
                if settings.port == 0
                    || settings.max_requests == 0
                    || settings.max_seconds == 0
                    || settings.bot_name.is_empty()
                    || settings.bot_name.len() > 16
                    || !settings
                        .bot_name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                {
                    self.fail("Invalid connection settings or budgets");
                    return;
                }
                self.settings = settings;
                self.started = Instant::now();
                self.id = session_id();
                self.latency = LatencyPolicy::default();
                self.view = View {
                    mode: self.settings.mode.clone(),
                    status: "Connecting".into(),
                    answer_age_limit_ms: self.latency.timing().max_answer_age_ms,
                    ..View::default()
                };
                self.observed_at = Instant::now();
                self.adapter = Some(match self.settings.mode {
                    Mode::Demo => crate::fixture::spawn(),
                    Mode::Live => crate::minecraft::spawn(self.settings.clone()),
                });
                self.event(
                    "connect",
                    "New session; demo is synthetic, live connects a separate Minecraft bot",
                    vec![],
                    None,
                );
            }
            Command::Start | Command::Step => {
                if self.view.replay {
                    return;
                }
                if self.adapter.is_none() {
                    self.fail("Connect to demo or Minecraft first");
                    return;
                }
                self.invalidate(false);
                self.view.last_error = None;
                self.continuous = matches!(command, Command::Start);
                self.one_step = !self.continuous;
                self.view.status = "Waiting for a fresh observation".into();
                self.event(
                    "control",
                    if self.continuous { "Start" } else { "One goal" },
                    vec![],
                    None,
                );
            }
            Command::Pause => {
                self.invalidate(false);
                self.view.status = "Paused — the world continues".into();
                if !self.view.replay {
                    self.event(
                        "control",
                        "Local stop; pending model answers invalidated",
                        vec![],
                        None,
                    );
                }
            }
            Command::Stop => {
                self.invalidate(true);
                self.view.status = "Stopped".into();
                if !self.view.replay {
                    self.event("control", "Local stop and disconnect", vec![], None);
                }
            }
            Command::Reset => {
                self.invalidate(true);
                self.view = View::default();
                self.started = Instant::now();
                self.id = session_id();
                self.latency = LatencyPolicy::default();
                self.view.answer_age_limit_ms = self.latency.timing().max_answer_age_ms;
            }
            Command::Manual {
                candidate,
                world_epoch,
                dimension,
            } => {
                if self.view.replay {
                    return;
                }
                self.invalidate(false);
                let Some(observation) = self.view.observation.clone().filter(|o| o.connected)
                else {
                    self.fail("No active observation for manual action");
                    return;
                };
                if self.observed_at.elapsed() > Duration::from_secs(1) {
                    self.fail("Observation is too old");
                    return;
                }
                if observation.world_epoch != world_epoch || observation.dimension != dimension {
                    self.fail("Manual action belongs to a different world lifecycle");
                    return;
                }
                let candidates = candidates(
                    &observation,
                    self.latency.timing().goal_ms,
                    &self.settings.mode,
                );
                if candidates.contains(&candidate) {
                    self.apply(
                        candidate,
                        "Manual action; mixed control",
                        candidates,
                        None,
                        None,
                    );
                } else {
                    self.fail("Manual action is not among current candidates");
                }
            }
            Command::Export => {
                let recording = Recording {
                    schema_version: 1,
                    id: self.id.clone(),
                    settings: self.settings.clone(),
                    events: self.view.events.clone(),
                };
                match crate::recording::save(&recording) {
                    Ok(path) => self.view.recording_path = Some(path),
                    Err(_) => {
                        self.view.last_error = Some("Could not save recording".into());
                    }
                }
            }
            Command::Replay(path) => {
                if !self.view.replay {
                    self.invalidate(true);
                }
                match crate::recording::load(&path) {
                    Ok(recording) => {
                        self.settings = recording.settings.clone();
                        self.id = recording.id;
                        self.view = View {
                            status: "Replay — recorded telemetry, no connection".into(),
                            mode: recording.settings.mode,
                            replay: true,
                            observation: recording
                                .events
                                .iter()
                                .rev()
                                .find_map(|e| e.observation.clone()),
                            requests: recording
                                .events
                                .iter()
                                .filter(|e| e.kind == "request")
                                .count() as u32,
                            events: recording.events,
                            recording_path: Some(path),
                            ..View::default()
                        };
                    }
                    Err(_) => {
                        self.view.last_error = Some("Could not load or validate recording".into());
                        if !self.view.replay {
                            self.view.status = "Stopped: could not load replay".into();
                        }
                    }
                }
            }
        }
    }

    fn tick(&mut self) {
        if self.view.replay {
            return;
        }
        let adapter_error = self
            .adapter
            .as_ref()
            .and_then(|a| a.errors.borrow().clone());
        if let Some(message) = adapter_error {
            self.invalidate(true);
            self.fail(message);
            return;
        }
        let observation = self
            .adapter
            .as_ref()
            .and_then(|a| a.observations.borrow().clone());
        if let Some(observation) = observation {
            let changed = self.view.observation.as_ref().is_none_or(|old| {
                old.sequence != observation.sequence
                    || old.connected != observation.connected
                    || old.dimension != observation.dimension
                    || old.world_epoch != observation.world_epoch
            });
            if changed {
                if !observation.position.x.is_finite()
                    || !observation.position.y.is_finite()
                    || !observation.position.z.is_finite()
                    || !observation.health.is_finite()
                {
                    self.fail("Adapter returned invalid observation values");
                    return;
                }
                let hurt = self
                    .view
                    .observation
                    .as_ref()
                    .is_some_and(|old| observation.health < old.health);
                let dimension_changed = self
                    .view
                    .observation
                    .as_ref()
                    .is_some_and(|old| old.dimension != observation.dimension);
                let world_changed = self.view.observation.as_ref().is_some_and(|old| {
                    old.dimension != observation.dimension
                        || old.world_epoch != observation.world_epoch
                });
                self.observed_at = Instant::now();
                self.view.observation = Some(observation.clone());
                if world_changed {
                    self.fail(if dimension_changed {
                        "Dimension changed: local stop and pending answers invalidated"
                    } else {
                        "World lifecycle changed: local stop and pending answers invalidated"
                    });
                    return;
                }
                if !observation.connected {
                    self.invalidate(true);
                    self.view.status = "Minecraft/fixture disconnected".into();
                    self.event("disconnect", observation.note, vec![], None);
                    return;
                }
                if hurt || observation.health <= 0.0 {
                    self.fail("Health decreased: local stop without waiting for the model");
                    return;
                }
                if self.view.status == "Connecting" {
                    self.view.status = "Connected — ready".into();
                }
            }
        }
        if self.adapter.as_ref().is_some_and(|a| a.task.is_finished()) {
            self.invalidate(true);
            self.fail("Adapter stopped");
            return;
        }
        if self.adapter.is_some() && self.observed_at.elapsed() > Duration::from_secs(10) {
            self.invalidate(true);
            self.fail("No fresh observations from adapter");
            return;
        }
        if (self.continuous
            || self.one_step
            || self.active.is_some()
            || self.pending.is_some()
            || self.dispatched.is_some())
            && self.started.elapsed() >= Duration::from_secs(self.settings.max_seconds)
        {
            self.fail("Session time budget reached");
            return;
        }
        if self.active.is_some() && self.observed_at.elapsed() > Duration::from_secs(1) {
            self.fail("Observations became stale: local stop");
            return;
        }
        let acknowledgement =
            self.dispatched
                .as_mut()
                .and_then(|action| match action.receiver.try_recv() {
                    Ok(Ok(accepted_at)) if accepted_at >= action.accepted_before => {
                        Some(Err("Adapter action acceptance deadline expired"))
                    }
                    Ok(Ok(accepted_at))
                        if accepted_at < action.sent_at || accepted_at > Instant::now() =>
                    {
                        Some(Err("Adapter returned an invalid acceptance timestamp"))
                    }
                    Ok(result) => Some(result),
                    Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                        Some(Err("Adapter ended before accepting the action"))
                    }
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty)
                        if Instant::now() >= action.accepted_before
                            || action.sent_at.elapsed() > Duration::from_secs(1) =>
                    {
                        Some(Err("Adapter action acknowledgement timed out"))
                    }
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty) => None,
                });
        if let Some(result) = acknowledgement {
            let action = self.dispatched.take().expect("dispatched action exists");
            match result {
                Ok(accepted_at) => {
                    self.view.active_goal = Some(action.candidate.id.clone());
                    self.view.status = "Executing bounded goal".into();
                    self.event(
                        "action",
                        format!(
                            "{}; adapter accepted bounded action: {}",
                            action.origin, action.candidate.id
                        ),
                        vec![action.candidate.clone()],
                        None,
                    );
                    self.active = Some((action.candidate, accepted_at));
                }
                Err(message) => {
                    self.event("rejected", message, vec![action.candidate], None);
                    self.fail(message);
                    return;
                }
            }
        }
        if let Some((candidate, started)) = &self.active {
            let reached = candidate.target.as_ref().is_some_and(|target| {
                self.view
                    .observation
                    .as_ref()
                    .is_some_and(|o| distance(&o.position, target) < 0.6)
            });
            if reached || elapsed(*started) >= candidate.duration_ms {
                if let Some(adapter) = &self.adapter {
                    let _ = adapter.commands.send(AdapterCommand::Stop);
                }
                self.active = None;
                self.view.active_goal = None;
                self.event(
                    "executor",
                    if reached {
                        "Local executor: target reached"
                    } else {
                        "Local executor: goal duration expired"
                    },
                    vec![],
                    None,
                );
                self.view.status = "Ready for the next goal".into();
            }
        }
        let result = self
            .pending
            .as_mut()
            .and_then(|p| match p.receiver.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    Some(Err("Provider task ended without a response".into()))
                }
                Err(mpsc::error::TryRecvError::Empty) => None,
            });
        if let Some(result) = result {
            let pending = self.pending.take().expect("pending receiver exists");
            let age = elapsed(pending.started);
            self.latency.record(age);
            let timing = self.latency.timing();
            self.view.p50_ms = timing.p50_ms;
            self.view.p95_ms = timing.p95_ms;
            self.view.goal_ms = timing.goal_ms;
            self.view.answer_age_limit_ms = timing.max_answer_age_ms;
            match result {
                Err(message) => {
                    self.fail(message);
                    return;
                }
                Ok(decision) => {
                    let fresh =
                        answer_is_current(pending.generation, self.generation, age, pending.timing)
                            && self.observed_at.elapsed() <= Duration::from_secs(1)
                            && self.view.observation.as_ref().is_some_and(|now| {
                                now.connected
                                    && now.dimension == pending.observation.dimension
                                    && now.world_epoch == pending.observation.world_epoch
                                    && now.health >= pending.observation.health
                                    && distance(&now.position, &pending.observation.position) <= 1.5
                            });
                    self.event(
                        "decision",
                        format!(
                            "Answer for observation {}: {} ms; {}",
                            pending.observation.sequence,
                            age,
                            if self.settings.mode == Mode::Demo {
                                "synthetic fixture controller"
                            } else {
                                "TypeSafe Jev"
                            }
                        ),
                        pending.candidates.clone(),
                        Some(decision.clone()),
                    );
                    if !fresh {
                        self.fail("Stale or changed observation: model answer rejected");
                        return;
                    }
                    if let Some(candidate) = pending
                        .candidates
                        .iter()
                        .find(|c| c.id == decision.choice)
                        .cloned()
                    {
                        let target_current = candidate.target.as_ref().is_none_or(|target| {
                            self.view.observation.as_ref().is_some_and(|now| {
                                now.blocks.iter().any(|landmark| {
                                    (self.settings.mode == Mode::Demo
                                        || landmark.name.starts_with("waypoint:"))
                                        && distance(target, &landmark.position) < 0.01
                                })
                            })
                        });
                        if !target_current {
                            self.fail("Current navigation prerequisites for the goal have changed");
                            return;
                        }
                        self.apply(
                            candidate,
                            if self.settings.mode == Mode::Demo {
                                "Fixture selected goal; local executor"
                            } else {
                                "Jev selected goal; local executor"
                            },
                            pending.candidates,
                            Some(decision),
                            Some(
                                pending.started
                                    + Duration::from_millis(pending.timing.max_answer_age_ms),
                            ),
                        );
                    } else {
                        self.fail("Response selected an unknown candidate");
                        return;
                    }
                }
            }
        }
        if (self.continuous || self.one_step)
            && self.pending.is_none()
            && self.active.is_none()
            && self.dispatched.is_none()
        {
            self.request();
        }
    }

    fn request(&mut self) {
        if self.view.events.len() >= 4_999 {
            self.event("budget", "Event budget", vec![], None);
            return;
        }
        if self.view.requests >= self.settings.max_requests {
            self.fail("Request budget reached");
            return;
        }
        let Some(observation) = self.view.observation.clone().filter(|o| o.connected) else {
            return;
        };
        if self.observed_at.elapsed() > Duration::from_secs(1) {
            return;
        }
        let key = if self.settings.mode == Mode::Live {
            match read_key() {
                Ok(key) => Some(key),
                Err(message) => {
                    self.fail(message);
                    return;
                }
            }
        } else {
            None
        };
        let timing = self.latency.timing();
        let candidates = candidates(&observation, timing.goal_ms, &self.settings.mode);
        let (tx, receiver) = mpsc::unbounded_channel();
        let request_observation = observation.clone();
        let request_candidates = candidates.clone();
        let started = Instant::now();
        let task = tokio::spawn(async move {
            let result = if let Some(key) = key {
                crate::provider::decide(
                    &key,
                    &request_observation,
                    &request_candidates,
                    Duration::from_secs(10),
                )
                .await
                .map_err(|e| e.to_string())
            } else {
                tokio::time::sleep(Duration::from_millis(300)).await;
                Ok(Decision {
                    choice: request_candidates[0].id.clone(),
                    probabilities: BTreeMap::new(),
                    confidence: None,
                    model: "offline-fixture".into(),
                    input_tokens: None,
                    output_tokens: None,
                    latency_ms: elapsed(started),
                })
            };
            let _ = tx.send(result);
        });
        self.pending = Some(Pending {
            generation: self.generation,
            started,
            observation: observation.clone(),
            candidates: candidates.clone(),
            timing,
            receiver,
            task,
        });
        self.one_step = false;
        self.view.requests += 1;
        self.view.status = "Waiting for goal decision".into();
        self.event(
            "request",
            format!(
                "{} {} for observation {}",
                if self.settings.mode == Mode::Demo {
                    "Synthetic fixture decision (no API call)"
                } else {
                    "TypeSafe request"
                },
                self.view.requests,
                observation.sequence
            ),
            candidates,
            None,
        );
    }

    fn apply(
        &mut self,
        candidate: Candidate,
        origin: &str,
        candidates: Vec<Candidate>,
        decision: Option<Decision>,
        response_deadline: Option<Instant>,
    ) {
        if self.started.elapsed() >= Duration::from_secs(self.settings.max_seconds) {
            self.fail("Session time budget reached");
            return;
        }
        if self.view.events.len() >= 4_999 {
            self.event("budget", "Event budget", vec![], None);
            return;
        }
        let Some(observation) = self.view.observation.as_ref().filter(|o| o.connected) else {
            self.fail("No connected observation for action dispatch");
            return;
        };
        let (request, receiver) = ActionRequest::new(candidate.clone(), observation);
        let now = Instant::now();
        let accepted_before = (now + Duration::from_secs(1))
            .min(self.observed_at + Duration::from_secs(1))
            .min(
                self.started
                    .checked_add(Duration::from_secs(self.settings.max_seconds))
                    .unwrap_or(now),
            )
            .min(response_deadline.unwrap_or(now + Duration::from_secs(1)));
        if now >= accepted_before {
            self.fail("Action acceptance deadline expired before dispatch");
            return;
        }
        let request = request.with_deadline(accepted_before);
        let sent = self
            .adapter
            .as_ref()
            .is_some_and(|a| a.commands.send(AdapterCommand::Execute(request)).is_ok());
        if !sent {
            self.fail("Adapter did not receive the action");
            return;
        }
        self.view.active_goal = None;
        self.view.status = "Awaiting adapter acceptance".into();
        self.event(
            "dispatched",
            format!("{origin}; queued for adapter validation: {}", candidate.id),
            candidates,
            decision,
        );
        self.dispatched = Some(Dispatched {
            accepted_before,
            origin: origin.to_owned(),
            candidate,
            sent_at: now,
            receiver,
        });
    }
}

fn candidates(observation: &Observation, duration_ms: u64, mode: &Mode) -> Vec<Candidate> {
    let mut result: Vec<_> = observation
        .blocks
        .iter()
        .enumerate()
        .filter(|(_, landmark)| {
            let p = &landmark.position;
            (*mode == Mode::Demo || landmark.name.starts_with("waypoint:"))
                && p.x.is_finite()
                && p.y.is_finite()
                && p.z.is_finite()
                && distance(&observation.position, p) <= 12.0
                && distance(&observation.position, p) > 0.6
        })
        .take(8)
        .map(|(index, landmark)| Candidate {
            id: format!("waypoint_{index}"),
            description: format!(
                "Navigate toward observed location: {}; pathfinder must not mine",
                landmark.name
            ),
            target: Some(landmark.position.clone()),
            duration_ms: duration_ms.clamp(1, 10_000),
        })
        .collect();
    result.push(Candidate {
        id: "wait".into(),
        description: "Wait without moving".into(),
        target: None,
        duration_ms: duration_ms.clamp(1, 10_000),
    });
    result
}

fn read_key() -> Result<String, String> {
    let key = std::env::var("TYPESAFE_API_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
        .or_else(|| {
            let path = std::env::var("TYPESAFE_API_KEY_FILE").ok()?;
            let metadata = std::fs::metadata(&path).ok()?;
            if metadata.len() > 16_384 {
                return None;
            }
            std::fs::read_to_string(path).ok()
        })
        .ok_or_else(|| "Missing TYPESAFE_API_KEY or TYPESAFE_API_KEY_FILE".to_string())?;
    let key = key.trim();
    if key.is_empty() || key.len() > 16_384 || key.chars().any(char::is_control) {
        return Err("Invalid TypeSafe API key configuration".into());
    }
    Ok(key.to_owned())
}

fn distance(a: &Position, b: &Position) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2) + (a.z - b.z).powi(2)).sqrt()
}
fn elapsed(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u64::MAX as u128) as u64
}
fn session_id() -> String {
    format!(
        "session-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        Session::new(Arc::new(Mutex::new(View::default())))
    }

    fn observation() -> Observation {
        Observation {
            world_epoch: 1,
            dimension: Some("fixture:overworld".into()),
            sequence: 1,
            connected: true,
            position: Position {
                x: 0.,
                y: 64.,
                z: 0.,
            },
            health: 20.,
            food: 20.,
            inventory: vec![],
            blocks: vec![],
            entities: vec![],
            note: "test fixture".into(),
        }
    }

    #[tokio::test]
    async fn dimension_change_at_identical_position_and_sequence_invalidates_ready_answer() {
        assert_world_change_invalidates_ready_answer(false).await;
    }

    #[tokio::test]
    async fn same_dimension_epoch_change_invalidates_ready_answer_and_stops_locally() {
        assert_world_change_invalidates_ready_answer(true).await;
    }

    async fn assert_world_change_invalidates_ready_answer(epoch_only: bool) {
        let mut session = session();
        let old = observation();
        let mut changed = old.clone();
        if epoch_only {
            changed.world_epoch += 1;
        } else {
            changed.dimension = Some("fixture:the_nether".into());
        }
        let expected_dimension = changed.dimension.clone();
        let expected_epoch = changed.world_epoch;
        assert_eq!(old.position, changed.position);
        assert_eq!(old.sequence, changed.sequence);
        session.view.observation = Some(old.clone());
        session.continuous = true;
        let candidate = Candidate {
            id: "wait".into(),
            description: "Wait".into(),
            target: None,
            duration_ms: 2_000,
        };
        session.active = Some((candidate.clone(), Instant::now()));
        session.view.active_goal = Some(candidate.id.clone());
        let (commands, mut commands_rx) = mpsc::unbounded_channel();
        let (_observations_tx, observations) = tokio::sync::watch::channel(Some(changed));
        let (_errors_tx, errors) = tokio::sync::watch::channel(None);
        session.adapter = Some(AdapterHandle {
            commands,
            observations,
            errors,
            task: tokio::spawn(std::future::pending()),
        });
        let (answer_tx, receiver) = mpsc::unbounded_channel();
        answer_tx
            .send(Ok(Decision {
                choice: "wait".into(),
                probabilities: BTreeMap::new(),
                confidence: None,
                model: "offline-fixture".into(),
                input_tokens: None,
                output_tokens: None,
                latency_ms: 1,
            }))
            .unwrap();
        session.pending = Some(Pending {
            generation: session.generation,
            started: Instant::now(),
            observation: old,
            candidates: vec![candidate],
            timing: session.latency.timing(),
            receiver,
            task: tokio::spawn(std::future::pending()),
        });
        let generation_before = session.generation;

        session.tick();

        assert_ne!(session.generation, generation_before);
        assert!(session.pending.is_none());
        assert!(session.active.is_none());
        assert!(session.view.active_goal.is_none());
        assert!(!session.continuous);
        assert!(
            session
                .view
                .last_error
                .as_deref()
                .unwrap()
                .contains(if epoch_only {
                    "World lifecycle changed"
                } else {
                    "Dimension changed"
                })
        );
        assert_eq!(
            session
                .view
                .observation
                .as_ref()
                .unwrap()
                .dimension
                .as_deref(),
            expected_dimension.as_deref()
        );
        assert_eq!(
            session.view.observation.as_ref().unwrap().world_epoch,
            expected_epoch
        );
        assert!(matches!(commands_rx.try_recv(), Ok(AdapterCommand::Stop)));
        session.tick();
        assert!(
            !session
                .view
                .events
                .iter()
                .any(|event| event.kind == "action" || event.kind == "decision")
        );
        assert!(commands_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn expired_manual_action_never_reaches_adapter_execution() {
        let mut session = session();
        session.settings.max_seconds = 1;
        session.started = Instant::now() - Duration::from_secs(2);
        session.view.observation = Some(observation());
        let (commands, mut receiver) = mpsc::unbounded_channel();
        let (_, observations) = tokio::sync::watch::channel(None);
        let (_, errors) = tokio::sync::watch::channel(None);
        session.adapter = Some(AdapterHandle {
            commands,
            observations,
            errors,
            task: tokio::spawn(std::future::pending()),
        });

        session.command(Command::Manual {
            candidate: candidates(&observation(), 2_000, &Mode::Demo)
                .pop()
                .unwrap(),
            world_epoch: observation().world_epoch,
            dimension: observation().dimension,
        });

        while let Ok(command) = receiver.try_recv() {
            assert!(
                !matches!(command, AdapterCommand::Execute(_)),
                "expired manual command reached executor"
            );
        }
        assert!(session.view.active_goal.is_none());
        assert!(
            session
                .view
                .last_error
                .as_deref()
                .unwrap()
                .contains("time budget")
        );
        assert!(
            !session
                .view
                .events
                .iter()
                .any(|event| event.kind == "action")
        );
    }

    fn awaiting_adapter() -> (
        Session,
        mpsc::UnboundedReceiver<AdapterCommand>,
        crate::adapter::ActionRequest,
    ) {
        let mut session = session();
        session.view.observation = Some(observation());
        let (commands, mut receiver) = mpsc::unbounded_channel();
        let (_, observations) = tokio::sync::watch::channel(Some(observation()));
        let (_, errors) = tokio::sync::watch::channel(None);
        session.adapter = Some(AdapterHandle {
            commands,
            observations,
            errors,
            task: tokio::spawn(std::future::pending()),
        });
        let candidate = Candidate {
            id: "wait".into(),
            description: "Wait".into(),
            target: None,
            duration_ms: 100,
        };
        session.apply(
            candidate.clone(),
            "Manual action; mixed control",
            vec![candidate],
            None,
            None,
        );
        let AdapterCommand::Execute(request) = receiver.try_recv().unwrap() else {
            panic!("expected dispatch");
        };
        assert!(session.active.is_none());
        assert!(
            !session
                .view
                .events
                .iter()
                .any(|event| event.kind == "action")
        );
        (session, receiver, request)
    }

    #[tokio::test]
    async fn rejected_acknowledgement_never_records_accepted_action() {
        let (mut session, mut commands, request) = awaiting_adapter();
        request.reply.send(Err("test endpoint rejected")).unwrap();
        session.tick();
        assert!(session.dispatched.is_none());
        assert!(session.active.is_none());
        assert!(session.view.active_goal.is_none());
        assert_eq!(
            session.view.last_error.as_deref(),
            Some("test endpoint rejected")
        );
        assert!(
            !session
                .view
                .events
                .iter()
                .any(|event| event.kind == "action")
        );
        assert!(
            session
                .view
                .events
                .iter()
                .any(|event| event.kind == "rejected")
        );
        assert!(matches!(commands.try_recv(), Ok(AdapterCommand::Stop)));
    }

    #[tokio::test]
    async fn reordered_landmarks_cannot_retarget_a_historical_manual_candidate() {
        let (mut session, mut commands, request) = awaiting_adapter();
        session.dispatched = None;
        drop(request);
        let mut old = observation();
        old.blocks = vec![
            Landmark {
                name: "first".into(),
                position: Position {
                    x: 2.,
                    y: 64.,
                    z: 0.,
                },
            },
            Landmark {
                name: "second".into(),
                position: Position {
                    x: 3.,
                    y: 64.,
                    z: 0.,
                },
            },
        ];
        let clicked = candidates(&old, 2_000, &Mode::Demo)[0].clone();
        let mut reordered = old.clone();
        reordered.blocks.swap(0, 1);
        assert_ne!(
            candidates(&reordered, 2_000, &Mode::Demo)[0].target,
            clicked.target
        );
        session.view.observation = Some(reordered);
        session.command(Command::Manual {
            candidate: clicked,
            world_epoch: old.world_epoch,
            dimension: old.dimension,
        });
        assert_eq!(
            session.view.last_error.as_deref(),
            Some("Manual action is not among current candidates")
        );
        assert!(session.dispatched.is_none());
        while let Ok(command) = commands.try_recv() {
            assert!(!matches!(command, AdapterCommand::Execute(_)));
        }
    }

    #[tokio::test]
    async fn historical_manual_candidate_cannot_cross_world_identity() {
        for dimension_changed in [false, true] {
            let (mut session, mut commands, request) = awaiting_adapter();
            session.dispatched = None;
            drop(request);
            let old = observation();
            let candidate = candidates(&old, 2_000, &Mode::Demo).pop().unwrap();
            let mut changed = old.clone();
            if dimension_changed {
                changed.dimension = Some("fixture:other".into());
            } else {
                changed.world_epoch += 1;
            }
            session.view.observation = Some(changed);
            session.command(Command::Manual {
                candidate,
                world_epoch: old.world_epoch,
                dimension: old.dimension,
            });
            assert_eq!(
                session.view.last_error.as_deref(),
                Some("Manual action belongs to a different world lifecycle")
            );
            while let Ok(command) = commands.try_recv() {
                assert!(!matches!(command, AdapterCommand::Execute(_)));
            }
        }
    }

    #[tokio::test]
    async fn successful_acknowledgement_after_absolute_deadline_is_rejected() {
        let (mut session, mut commands, request) = awaiting_adapter();
        session.dispatched.as_mut().unwrap().accepted_before =
            Instant::now() - Duration::from_millis(1);
        request.reply.send(Ok(Instant::now())).unwrap();
        session.tick();
        assert!(session.active.is_none());
        assert!(session.dispatched.is_none());
        assert_eq!(
            session.view.last_error.as_deref(),
            Some("Adapter action acceptance deadline expired")
        );
        assert!(
            !session
                .view
                .events
                .iter()
                .any(|event| event.kind == "action")
        );
        assert!(matches!(commands.try_recv(), Ok(AdapterCommand::Stop)));
    }

    #[tokio::test]
    async fn timely_acceptance_polled_after_deadline_records_action_and_uses_actual_clock() {
        let (mut session, mut commands, request) = awaiting_adapter();
        let now = Instant::now();
        let accepted_at = now - Duration::from_millis(200);
        let dispatch = session.dispatched.as_mut().unwrap();
        dispatch.sent_at = now - Duration::from_millis(400);
        dispatch.accepted_before = now - Duration::from_millis(100);
        request.reply.send(Ok(accepted_at)).unwrap();
        session.tick();
        // The 100ms action was accepted on time, but has already expired by poll time.
        assert!(
            session
                .view
                .events
                .iter()
                .any(|event| event.kind == "action")
        );
        assert!(
            session
                .view
                .events
                .iter()
                .any(|event| event.kind == "executor")
        );
        assert!(session.view.last_error.is_none());
        assert!(session.active.is_none());
        assert!(matches!(commands.try_recv(), Ok(AdapterCommand::Stop)));
    }

    #[tokio::test]
    async fn manual_choices_refresh_after_latency_changes_without_a_model_request() {
        let (mut session, mut commands, request) = awaiting_adapter();
        session.dispatched = None;
        drop(request);
        session.refresh_manual_candidates();
        let old = session.view.manual_candidates[0].clone();
        assert_eq!(old.duration_ms, 2_000);
        session.latency.record(2_000);
        session.refresh_manual_candidates();
        let current = session.view.manual_candidates[0].clone();
        assert_eq!(current.duration_ms, 8_000);
        let world = session.view.observation.clone().unwrap();
        session.command(Command::Manual {
            candidate: old,
            world_epoch: world.world_epoch,
            dimension: world.dimension.clone(),
        });
        assert!(session.dispatched.is_none());
        while let Ok(command) = commands.try_recv() {
            assert!(!matches!(command, AdapterCommand::Execute(_)));
        }
        session.command(Command::Manual {
            candidate: current.clone(),
            world_epoch: world.world_epoch,
            dimension: world.dimension,
        });
        let request = loop {
            match commands
                .try_recv()
                .expect("expected refreshed manual dispatch")
            {
                AdapterCommand::Execute(request) => break request,
                AdapterCommand::Stop => continue,
                AdapterCommand::Disconnect => panic!("unexpected disconnect"),
            }
        };
        assert_eq!(request.candidate, current);
        assert_eq!(session.view.requests, 0);
    }

    #[tokio::test]
    async fn dispatch_deadline_is_bounded_by_observation_model_and_session() {
        for limiting in ["observation", "model", "session"] {
            let (mut session, mut commands, old_request) = awaiting_adapter();
            session.dispatched = None;
            drop(old_request);
            let cutoff = Instant::now() + Duration::from_millis(100);
            let mut response_deadline = None;
            match limiting {
                "observation" => session.observed_at = cutoff - Duration::from_secs(1),
                "model" => response_deadline = Some(cutoff),
                "session" => {
                    session.settings.max_seconds = 1;
                    session.started = cutoff - Duration::from_secs(1);
                }
                _ => unreachable!(),
            }
            let candidate = candidates(&observation(), 2_000, &Mode::Demo)
                .pop()
                .unwrap();
            session.apply(
                candidate.clone(),
                "test",
                vec![candidate],
                None,
                response_deadline,
            );
            let AdapterCommand::Execute(request) = commands.try_recv().unwrap() else {
                panic!("expected execute");
            };
            assert_eq!(
                request.accepted_before, cutoff,
                "incorrect {limiting} deadline"
            );
            assert_eq!(session.dispatched.as_ref().unwrap().accepted_before, cutoff);
        }
    }

    #[tokio::test]
    async fn acknowledgement_timeout_stops_without_recording_action() {
        let (mut session, mut commands, request) = awaiting_adapter();
        session.dispatched.as_mut().unwrap().sent_at = Instant::now() - Duration::from_secs(2);
        session.tick();
        assert!(session.dispatched.is_none());
        assert!(session.active.is_none());
        assert!(
            session
                .view
                .last_error
                .as_deref()
                .unwrap()
                .contains("acknowledgement timed out")
        );
        assert!(
            !session
                .view
                .events
                .iter()
                .any(|event| event.kind == "action")
        );
        assert!(matches!(commands.try_recv(), Ok(AdapterCommand::Stop)));
        // A late success cannot resurrect an already timed-out dispatch.
        assert!(request.reply.send(Ok(Instant::now())).is_err());
        session.tick();
        assert!(session.active.is_none());
    }

    #[tokio::test]
    async fn delayed_acceptance_starts_action_duration_when_accepted_and_keeps_origin() {
        let (mut session, mut commands, request) = awaiting_adapter();
        session.dispatched.as_mut().unwrap().sent_at = Instant::now() - Duration::from_millis(900);
        let accepted_at = Instant::now();
        request.reply.send(Ok(accepted_at)).unwrap();
        session.tick();
        let (_, action_started) = session
            .active
            .as_ref()
            .expect("100ms action must not expire due to 900ms dispatch delay");
        assert_eq!(*action_started, accepted_at);
        assert!(commands.try_recv().is_err());
        let accepted = session
            .view
            .events
            .iter()
            .find(|event| event.kind == "action")
            .unwrap();
        assert!(accepted.message.contains("Manual action; mixed control"));
        session.active.as_mut().unwrap().1 = Instant::now() - Duration::from_millis(101);
        session.tick();
        assert!(session.active.is_none());
        assert!(matches!(commands.try_recv(), Ok(AdapterCommand::Stop)));
    }

    #[test]
    fn failed_replay_preserves_loaded_recording_and_timeline() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("recording.json");
        let recording = Recording {
            schema_version: 1,
            id: "old-session".into(),
            settings: Settings::default(),
            events: vec![Event {
                sequence: 1,
                elapsed_ms: 60_000,
                kind: "executor".into(),
                message: "recorded".into(),
                observation: Some(observation()),
                candidates: vec![],
                decision: None,
            }],
        };
        std::fs::write(&path, serde_json::to_vec(&recording).unwrap()).unwrap();
        let mut session = session();
        session.command(Command::Replay(path.to_string_lossy().into_owned()));
        let before = serde_json::to_value(&session.view.events).unwrap();
        let previous_path = session.view.recording_path.clone();

        session.command(Command::Replay(
            directory
                .path()
                .join("missing.json")
                .to_string_lossy()
                .into_owned(),
        ));

        assert!(session.view.replay);
        assert!(session.view.last_error.is_some());
        assert_eq!(session.view.recording_path, previous_path);
        assert_eq!(serde_json::to_value(&session.view.events).unwrap(), before);
        assert!(session.view.observation.as_ref().unwrap().connected);
        crate::recording::validate(&Recording {
            schema_version: 1,
            id: session.id,
            settings: session.settings,
            events: session.view.events,
        })
        .unwrap();
    }
}
