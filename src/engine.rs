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

/// Pause reasons recorded when the observed world changes under a running session. Both
/// are a local stop, not a failure: an operator (or the headless harness) resumes with
/// `Start` once the new world is observed.
pub const DIMENSION_CHANGED: &str = "Dimension changed: local stop and pending answers invalidated";
pub const WORLD_LIFECYCLE_CHANGED: &str =
    "World lifecycle changed: local stop and pending answers invalidated";
/// Recorded when an observation shows lower health, or none left: a local stop that does
/// not wait for the model.
pub const HEALTH_DECREASED: &str = "Health decreased: local stop without waiting for the model";
/// Recorded when the adapter reports a death (or an observation shows no health left).
/// It ends a session: the respawned bot is somewhere else with nothing it had.
pub const BOT_DIED: &str = "Bot died: local stop; the session ends";
/// The adapter's reason for refusing an action that was still awaiting acceptance when a
/// hit arrived. With the safety reflex on, it is recorded and the session keeps going.
pub const REJECTED_AFTER_HIT: &str = "Health decreased before action acceptance";

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
    /// Earliest instant the paced model loop may issue its next request.
    next_request_at: Instant,
    /// When the safety reflex last dispatched an action.
    last_reflex_at: Option<Instant>,
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
            next_request_at: Instant::now(),
            last_reflex_at: None,
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
        self.event_with_arrival(kind, message, candidates, decision, None);
    }

    /// Records an event together with the local arrival verdict, when the event ends
    /// a bounded navigation action.
    fn event_with_arrival(
        &mut self,
        kind: &str,
        message: impl Into<String>,
        candidates: Vec<Candidate>,
        decision: Option<Decision>,
        arrival: Option<ArrivalVerdict>,
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
                    arrival: None,
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
            arrival,
        });
    }

    /// The operator opted into the safety reflex and a paced loop is running: hits are
    /// survived and recorded instead of being a local stop.
    fn reflex_session(&self) -> bool {
        self.settings.safety_reflex && (self.continuous || self.one_step)
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
                    || settings.objective.chars().count() > 400
                    || settings
                        .objective
                        .chars()
                        .any(|c| c.is_control() && c != '\n')
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
                self.next_request_at = Instant::now();
                self.last_reflex_at = None;
                self.view = View {
                    mode: self.settings.mode.clone(),
                    status: "Connecting".into(),
                    answer_age_limit_ms: self.latency.timing().max_answer_age_ms,
                    objective: self.settings.objective.trim().to_owned(),
                    ..View::default()
                };
                self.observed_at = Instant::now();
                self.adapter = Some(match self.settings.mode {
                    // The distant waypoint stays the default: it lies further away than one
                    // bounded demo goal can close, so the demo shows the honest expiry path
                    // instead of an arrival tuned to look good. `FixtureWaypoint::Reachable`
                    // selects the geometry in which one bounded goal arrives, which is what a
                    // test or an operator watching the offline demo asks for explicitly.
                    Mode::Demo => {
                        let waypoint = match self.settings.fixture_waypoint {
                            FixtureWaypoint::Distant => crate::fixture::DISTANT_WAYPOINT,
                            FixtureWaypoint::Reachable => crate::fixture::REACHABLE_WAYPOINT,
                        };
                        crate::fixture::spawn_with_waypoint(waypoint)
                    }
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
                let previous_health = self.view.observation.as_ref().map(|old| old.health);
                let died = observation.health <= 0.0
                    || self
                        .view
                        .observation
                        .as_ref()
                        .is_some_and(|old| observation.deaths > old.deaths);
                let world_changed = self.view.observation.as_ref().is_some_and(|old| {
                    old.dimension != observation.dimension
                        || old.world_epoch != observation.world_epoch
                });
                self.observed_at = Instant::now();
                self.view.observation = Some(observation.clone());
                // Checked before the world change: a death followed by a respawn in another
                // dimension is a death, not a transfer.
                if died {
                    self.fail(BOT_DIED);
                    return;
                }
                if world_changed {
                    self.fail(if dimension_changed {
                        DIMENSION_CHANGED
                    } else {
                        WORLD_LIFECYCLE_CHANGED
                    });
                    return;
                }
                if !observation.connected {
                    self.invalidate(true);
                    self.view.status = "Minecraft/fixture disconnected".into();
                    self.event("disconnect", observation.note, vec![], None);
                    return;
                }
                if hurt && self.reflex_session() {
                    // With the reflex opted in, a hit is recorded and the session keeps
                    // going: a flight goal keeps running (the adapter no longer stops it
                    // under fire) and the reflex may answer the attacker at once. Pausing on
                    // every hit is what kept the bot standing still under arrows.
                    self.event(
                        "hurt",
                        format!(
                            "Health {:.1} -> {:.1}; reflex session continues without a local stop",
                            previous_health.unwrap_or(observation.health),
                            observation.health
                        ),
                        vec![],
                        None,
                    );
                } else if hurt {
                    self.fail(HEALTH_DECREASED);
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
                    // Under the reflex the hit was already recorded as survivable; the
                    // refused action is simply not executed and the loop asks again.
                    if !(message == REJECTED_AFTER_HIT && self.reflex_session()) {
                        self.fail(message);
                        return;
                    }
                }
            }
        }
        let active_goal = self.active.as_ref().map(|(candidate, started)| {
            (candidate.target.clone(), candidate.duration_ms, *started)
        });
        if let Some((target, duration_ms, started)) = active_goal {
            let measured = self
                .view
                .observation
                .as_ref()
                .filter(|observation| observation.connected)
                .and_then(|observation| {
                    target
                        .as_ref()
                        .map(|target| distance(&observation.position, target))
                })
                .filter(|measured| measured.is_finite());
            let arrived = measured.is_some_and(|measured| measured < ARRIVAL_TOLERANCE_M);
            let elapsed_ms = elapsed(started);
            if arrived || elapsed_ms >= duration_ms {
                if let Some(adapter) = &self.adapter {
                    let _ = adapter.commands.send(AdapterCommand::Stop);
                }
                self.active = None;
                self.view.active_goal = None;
                let message = if arrived {
                    "Local executor: target reached"
                } else if target.is_some() {
                    "Local executor: goal duration expired without arrival"
                } else {
                    "Local executor: goal duration expired"
                };
                self.event_with_arrival(
                    "executor",
                    message,
                    vec![],
                    None,
                    Some(ArrivalVerdict {
                        target,
                        measured_distance_m: measured,
                        tolerance_m: ARRIVAL_TOLERANCE_M,
                        duration_ms,
                        elapsed_ms,
                        arrived,
                    }),
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
                Err(message)
                    if message.starts_with(crate::provider::REJECTED_ANSWER)
                        && (self.continuous || self.one_step) =>
                {
                    // A malformed answer is dropped, never acted on; the paced loop sends
                    // its next request as usual. Safety never waited on this answer.
                    self.view.rejected_answers += 1;
                    self.event("rejected", message, vec![], None);
                    return;
                }
                Err(message) => {
                    self.fail(message);
                    return;
                }
                Ok(decision) => {
                    let same_world =
                        answer_is_current(pending.generation, self.generation, age, pending.timing)
                            && self.observed_at.elapsed() <= Duration::from_secs(1)
                            && self.view.observation.as_ref().is_some_and(|now| {
                                now.connected
                                    && now.dimension == pending.observation.dimension
                                    && now.world_epoch == pending.observation.world_epoch
                            });
                    let unchanged = self.view.observation.as_ref().is_some_and(|now| {
                        now.health >= pending.observation.health
                            && distance(&now.position, &pending.observation.position) <= 1.5
                    });
                    let fresh = same_world && unchanged;
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
                    if same_world && !unchanged && self.reflex_session() {
                        // A hit (or its knockback) overtook the answer. The reflex session
                        // already chose to survive the hit, so the stale answer is dropped,
                        // never acted on, and the paced loop asks again from the new state.
                        self.event(
                            "rejected",
                            "Answer overtaken by damage or knockback under the safety reflex; dropped",
                            vec![],
                            None,
                        );
                        return;
                    }
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
        self.reflex();
        if (self.continuous || self.one_step)
            && self.pending.is_none()
            && self.active.is_none()
            && self.dispatched.is_none()
            && !self.reflex_in_flight()
            && Instant::now() >= self.next_request_at
        {
            self.request();
        }
    }

    /// Whether the action in flight was dispatched by the safety reflex.
    fn reflex_in_flight(&self) -> bool {
        self.dispatched
            .as_ref()
            .is_some_and(|action| action.origin.starts_with("SAFETY-REFLEX"))
    }

    /// Stops local movement and invalidates in-flight model answers without ending the
    /// session's run mode. A pause uses `invalidate`, which also clears the run mode;
    /// the safety reflex continues the paced loop afterwards, so it needs this narrower
    /// stop. Preempting an idling goal means its arrival verdict is never recorded —
    /// the same loss a manual takeover causes, and the `reflex` event names the goal it
    /// superseded so the hole in the chain is visible in the recording.
    fn clear_in_flight(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if let Some(pending) = self.pending.take() {
            pending.task.abort();
        }
        self.active = None;
        self.view.active_goal = None;
        if let Some(adapter) = &self.adapter {
            let _ = adapter.commands.send(AdapterCommand::Stop);
        }
    }

    /// The engine's own bounded safety action, opted in per session.
    ///
    /// The model loop is paced and its answers arrive hundreds of milliseconds later; a
    /// hostile mob inside arm's reach is a seconds-scale event. When a threat is that
    /// close, the engine stops the session's in-flight work (the local stop a pause
    /// performs, without ending the run mode), records why, and dispatches one bounded
    /// flight goal built from the current observation. No model request is spent and the
    /// action carries the `SAFETY-REFLEX` origin, so a recording still separates what the
    /// model chose from what the engine did. It never preempts a navigation goal, so no
    /// arrival verdict is lost, and it does nothing when no observed cell improves the
    /// distance to the threat.
    fn reflex(&mut self) {
        if !self.settings.safety_reflex
            || !(self.continuous || self.one_step)
            || self.adapter.is_none()
            || self.pending.is_some()
            || self.dispatched.is_some()
            || self.reflex_in_flight()
        {
            return;
        }
        if self.last_reflex_at.is_some_and(|last| {
            last.elapsed() < Duration::from_millis(crate::survival::REFLEX_COOLDOWN_MS)
        }) {
            return;
        }
        if self
            .active
            .as_ref()
            .is_some_and(|(candidate, _)| candidate.target.is_some())
        {
            return;
        }
        if self.observed_at.elapsed() > Duration::from_secs(1) {
            return;
        }
        let Some(observation) = self.view.observation.clone().filter(|o| o.connected) else {
            return;
        };
        let Some(threat) =
            crate::survival::nearest_threat_within(&observation, crate::survival::REFLEX_RADIUS_M)
        else {
            return;
        };
        let candidates = candidates(
            &observation,
            self.latency.timing().goal_ms,
            &self.settings.mode,
        );
        // `candidates` orders flight goals best first: the observed cell that puts the
        // most distance between the bot and the threat.
        let Some(candidate) = candidates
            .iter()
            .find(|candidate| candidate.id.starts_with("flee_"))
            .cloned()
        else {
            return;
        };
        let superseded = self
            .active
            .as_ref()
            .map(|(candidate, _)| candidate.id.clone());
        self.clear_in_flight();
        self.last_reflex_at = Some(Instant::now());
        self.view.reflexes += 1;
        self.event(
            "reflex",
            format!(
                "Engine safety action: {} is {:.1} blocks away; {} and fleeing via {}",
                threat.kind,
                threat.distance_m,
                match &superseded {
                    Some(id) =>
                        format!("stopping in-flight work (superseded {id}, no arrival verdict)"),
                    None => "no goal in flight".to_owned(),
                },
                candidate.id
            ),
            candidates.clone(),
            None,
        );
        self.apply(
            candidate,
            "SAFETY-REFLEX; engine-initiated bounded action",
            candidates,
            None,
            None,
        );
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
        // Visibility for the honest case where no waypoint qualifies: the model then
        // only sees bounded alternatives, and the reason is recorded rather than left
        // implicit in an empty candidate list.
        let observed_waypoints = observation
            .blocks
            .iter()
            .filter(|landmark| {
                self.settings.mode == Mode::Demo || landmark.name.starts_with("waypoint:")
            })
            .count();
        let navigable = candidates
            .iter()
            .filter(|candidate| candidate.target.is_some())
            .count();
        let no_navigation = if navigable == 0 {
            format!(
                "; no navigate candidate: {observed_waypoints} observed waypoints, none between {} and 12 blocks away",
                ARRIVAL_TOLERANCE_M
            )
        } else {
            String::new()
        };
        let (tx, receiver) = mpsc::unbounded_channel();
        let request_observation = observation.clone();
        let request_candidates = candidates.clone();
        let objective = self.settings.objective.trim().to_owned();
        // The paced loop may not ask again before this instant, so a long session
        // bounds provider spend by wall-clock rather than by action count alone.
        self.next_request_at =
            Instant::now() + Duration::from_millis(self.settings.request_interval_ms);
        let started = Instant::now();
        let task = tokio::spawn(async move {
            let result = if let Some(key) = key {
                crate::provider::decide(
                    &key,
                    &request_observation,
                    &request_candidates,
                    &objective,
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
                "{} {} for observation {}{}",
                if self.settings.mode == Mode::Demo {
                    "Synthetic fixture decision (no API call)"
                } else {
                    "TypeSafe request"
                },
                self.view.requests,
                observation.sequence,
                no_navigation
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
                && distance(&observation.position, p) > ARRIVAL_TOLERANCE_M
        })
        .take(8)
        .map(|(index, landmark)| {
            let measured = distance(&observation.position, &landmark.position);
            Candidate {
                id: format!("waypoint_{index}"),
                description: format!(
                    "Navigate toward observed location: {} ({measured:.1} blocks away); pathfinder must not mine",
                    landmark.name
                ),
                target: Some(landmark.position.clone()),
                duration_ms: duration_ms.clamp(1, 10_000),
            }
        })
        .collect();
    result.extend(crate::survival::flee_candidates(observation, duration_ms));
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
            deaths: 0,
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
                arrival: None,
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

    /// The offline observation plus a hostile neighbour and two observed standable
    /// cells: the cell away from the threat and one between bot and threat.
    fn threat_observation() -> Observation {
        let mut observation = observation();
        observation.blocks = vec![
            Landmark {
                name: "waypoint:-2:64:0".into(),
                position: Position {
                    x: -2.5,
                    y: 64.0,
                    z: 0.5,
                },
            },
            Landmark {
                name: "waypoint:2:64:0".into(),
                position: Position {
                    x: 2.5,
                    y: 64.0,
                    z: 0.5,
                },
            },
        ];
        observation.entities = vec![Landmark {
            name: "Zombie".into(),
            position: Position {
                x: 3.0,
                y: 64.0,
                z: 0.0,
            },
        }];
        observation
    }

    fn connected_session(
        settings: Settings,
        published: Observation,
    ) -> (Session, mpsc::UnboundedReceiver<AdapterCommand>) {
        let mut session = session();
        session.settings = settings;
        session.view.observation = Some(published.clone());
        session.observed_at = Instant::now();
        let (commands, receiver) = mpsc::unbounded_channel();
        let (_observations_tx, observations) = tokio::sync::watch::channel(Some(published));
        let (_errors_tx, errors) = tokio::sync::watch::channel(None);
        session.adapter = Some(AdapterHandle {
            commands,
            observations,
            errors,
            task: tokio::spawn(std::future::pending()),
        });
        (session, receiver)
    }

    #[test]
    fn hostile_entity_adds_one_flight_candidate_away_from_the_threat() {
        let observation = threat_observation();
        let candidates = candidates(&observation, 2_000, &Mode::Live);
        let flight: Vec<_> = candidates
            .iter()
            .filter(|candidate| candidate.id.starts_with("flee_"))
            .collect();
        assert_eq!(
            flight.len(),
            1,
            "only the observed cell that increases the distance is a flight goal"
        );
        assert_eq!(flight[0].id, "flee_0");
        assert_eq!(flight[0].duration_ms, 2_000);
        assert!(flight[0].description.contains("Zombie"));
        assert_eq!(candidates.last().unwrap().id, "wait");
    }

    #[test]
    fn passive_neighbours_never_become_flight_goals() {
        let mut observation = threat_observation();
        observation.entities = vec![Landmark {
            name: "Cow".into(),
            position: Position {
                x: 3.0,
                y: 64.0,
                z: 0.0,
            },
        }];
        assert!(
            !candidates(&observation, 2_000, &Mode::Live)
                .iter()
                .any(|candidate| candidate.id.starts_with("flee_"))
        );
    }

    #[tokio::test]
    async fn safety_reflex_dispatches_a_flight_goal_without_a_model_request() {
        let (mut session, mut commands) = connected_session(
            Settings {
                mode: Mode::Live,
                safety_reflex: true,
                ..Settings::default()
            },
            threat_observation(),
        );
        session.continuous = true;

        session.tick();

        assert_eq!(session.view.requests, 0, "a reflex spends no model request");
        assert_eq!(session.view.reflexes, 1);
        let reflex = session
            .view
            .events
            .iter()
            .find(|event| event.kind == "reflex")
            .expect("the reflex records why it acted");
        assert!(reflex.message.contains("Zombie") && reflex.message.contains("3.0 blocks"));
        assert!(reflex.message.contains("no goal in flight"));
        assert!(reflex.candidates.iter().any(|c| c.id == "flee_0"));
        let dispatched = session.dispatched.as_ref().expect("reflex dispatched");
        assert!(dispatched.origin.starts_with("SAFETY-REFLEX"));
        assert_eq!(dispatched.candidate.id, "flee_0");
        assert!(
            matches!(commands.try_recv(), Ok(AdapterCommand::Stop)),
            "the reflex stops local movement first"
        );
        assert!(
            matches!(commands.try_recv(), Ok(AdapterCommand::Execute(_))),
            "then dispatches the flight goal"
        );
    }

    #[tokio::test]
    async fn safety_reflex_never_preempts_a_navigation_goal() {
        let (mut session, _commands) = connected_session(
            Settings {
                mode: Mode::Live,
                safety_reflex: true,
                ..Settings::default()
            },
            threat_observation(),
        );
        session.continuous = true;
        let navigation = candidates(&threat_observation(), 2_000, &Mode::Live)
            .into_iter()
            .find(|candidate| candidate.target.is_some())
            .unwrap();
        session.active = Some((navigation.clone(), Instant::now()));

        session.tick();

        assert_eq!(session.view.reflexes, 0);
        assert!(session.dispatched.is_none());
        assert_eq!(
            session
                .active
                .as_ref()
                .map(|(candidate, _)| candidate.id.as_str()),
            Some(navigation.id.as_str()),
            "the goal in flight keeps its arrival verdict"
        );
    }

    #[tokio::test]
    async fn safety_reflex_stays_off_until_it_is_opted_in() {
        let (mut session, _commands) = connected_session(
            Settings {
                mode: Mode::Demo,
                ..Settings::default()
            },
            threat_observation(),
        );
        session.continuous = true;

        session.tick();

        assert_eq!(session.view.reflexes, 0);
        assert!(session.dispatched.is_none());
        assert_eq!(session.view.requests, 1, "the model loop still asks");
    }

    #[tokio::test]
    async fn request_interval_paces_the_continuous_loop() {
        let (mut session, _commands) = connected_session(
            Settings {
                mode: Mode::Demo,
                request_interval_ms: 60_000,
                ..Settings::default()
            },
            threat_observation(),
        );
        session.continuous = true;

        session.tick();
        assert_eq!(session.view.requests, 1, "Start asks immediately");
        // The answer for the first request has been consumed, so only the pacing gate
        // can hold the next one back.
        session.pending = None;
        session.tick();
        assert_eq!(
            session.view.requests, 1,
            "the paced loop waits out the interval"
        );
        session.next_request_at = Instant::now() - Duration::from_millis(1);
        session.tick();
        assert_eq!(session.view.requests, 2);
    }

    /// A session whose adapter feed the test can publish to, unlike `connected_session`,
    /// whose sender is dropped once the first observation is in place.
    fn fed_session(
        settings: Settings,
        published: Observation,
    ) -> (
        Session,
        mpsc::UnboundedReceiver<AdapterCommand>,
        tokio::sync::watch::Sender<Option<Observation>>,
    ) {
        let mut session = session();
        session.settings = settings;
        session.view.observation = Some(published.clone());
        session.observed_at = Instant::now();
        let (commands, receiver) = mpsc::unbounded_channel();
        let (feed, observations) = tokio::sync::watch::channel(Some(published));
        let (_errors_tx, errors) = tokio::sync::watch::channel(None);
        session.adapter = Some(AdapterHandle {
            commands,
            observations,
            errors,
            task: tokio::spawn(std::future::pending()),
        });
        (session, receiver, feed)
    }

    fn next(observation: &Observation) -> Observation {
        let mut next = observation.clone();
        next.sequence += 1;
        next
    }

    #[tokio::test]
    async fn with_the_reflex_a_hit_is_recorded_and_the_session_keeps_running() {
        let start = threat_observation();
        let (mut session, _commands, feed) = fed_session(
            Settings {
                mode: Mode::Live,
                safety_reflex: true,
                ..Settings::default()
            },
            start.clone(),
        );
        session.continuous = true;
        let mut hurt = next(&start);
        hurt.health = 16.0;
        feed.send_replace(Some(hurt));

        session.tick();

        assert!(
            session.view.last_error.is_none(),
            "{:?}",
            session.view.last_error
        );
        assert!(session.continuous, "the run mode survives the hit");
        let event = session
            .view
            .events
            .iter()
            .find(|event| event.kind == "hurt")
            .expect("the hit is recorded");
        assert!(event.message.contains("20.0 -> 16.0"), "{}", event.message);
    }

    #[tokio::test]
    async fn without_the_reflex_a_hit_is_still_a_local_stop() {
        let start = observation();
        let (mut session, _commands, feed) = fed_session(
            Settings {
                mode: Mode::Live,
                ..Settings::default()
            },
            start.clone(),
        );
        session.continuous = true;
        let mut hurt = next(&start);
        hurt.health = 16.0;
        feed.send_replace(Some(hurt));

        session.tick();

        assert_eq!(session.view.last_error.as_deref(), Some(HEALTH_DECREASED));
    }

    /// Regression for the live night run: the bot died and respawned in the hub between
    /// two sampled observations, the engine saw only a dimension change and the harness
    /// kept the session running in the wrong world.
    #[tokio::test]
    async fn a_death_counted_by_the_adapter_ends_even_when_the_respawn_changed_the_world() {
        let start = observation();
        let (mut session, _commands, feed) = fed_session(
            Settings {
                mode: Mode::Live,
                safety_reflex: true,
                ..Settings::default()
            },
            start.clone(),
        );
        session.continuous = true;
        let mut respawned = next(&start);
        respawned.deaths = 1;
        respawned.health = 20.0;
        respawned.world_epoch += 1;
        respawned.dimension = Some("minecraft:hub".into());
        feed.send_replace(Some(respawned));

        session.tick();

        assert_eq!(session.view.last_error.as_deref(), Some(BOT_DIED));
    }

    #[tokio::test]
    async fn no_health_left_is_a_death_with_or_without_the_reflex() {
        for safety_reflex in [false, true] {
            let start = observation();
            let (mut session, _commands, feed) = fed_session(
                Settings {
                    mode: Mode::Live,
                    safety_reflex,
                    ..Settings::default()
                },
                start.clone(),
            );
            session.continuous = true;
            let mut dead = next(&start);
            dead.health = 0.0;
            feed.send_replace(Some(dead));

            session.tick();

            assert_eq!(
                session.view.last_error.as_deref(),
                Some(BOT_DIED),
                "reflex {safety_reflex}"
            );
        }
    }

    #[tokio::test]
    async fn a_skeleton_outside_the_melee_radius_triggers_the_reflex() {
        let mut observation = threat_observation();
        // 10 blocks away: beyond the 6-block melee reflex, inside a skeleton's range.
        observation.entities = vec![Landmark {
            name: "Skeleton".into(),
            position: Position {
                x: 0.5,
                y: 64.0,
                z: -9.5,
            },
        }];
        observation.blocks = vec![
            Landmark {
                name: "waypoint:6:64:0".into(),
                position: Position {
                    x: 6.5,
                    y: 64.0,
                    z: 0.5,
                },
            },
            Landmark {
                name: "waypoint:0:64:6".into(),
                position: Position {
                    x: 0.5,
                    y: 64.0,
                    z: 6.5,
                },
            },
        ];
        let (mut session, _commands) = connected_session(
            Settings {
                mode: Mode::Live,
                safety_reflex: true,
                ..Settings::default()
            },
            observation,
        );
        session.continuous = true;

        session.tick();

        assert_eq!(session.view.reflexes, 1);
        let dispatched = session.dispatched.as_ref().expect("reflex dispatched");
        assert_eq!(
            dispatched.candidate.id, "flee_0",
            "the reflex runs across the line of fire, not straight down it"
        );
    }

    fn pending_with(session: &Session, result: Result<Decision, String>) -> Pending {
        let (sender, receiver) = mpsc::unbounded_channel();
        sender.send(result).unwrap();
        Pending {
            generation: session.generation,
            started: Instant::now(),
            observation: session.view.observation.clone().unwrap(),
            candidates: vec![],
            timing: session.latency.timing(),
            receiver,
            task: tokio::spawn(async {}),
        }
    }

    /// Regression for two live runs that ended as `provider_failed` on one malformed
    /// answer out of dozens: a rejected answer is recorded and dropped, the run goes on.
    #[tokio::test]
    async fn a_rejected_answer_is_dropped_and_the_loop_keeps_running() {
        let (mut session, _commands) = connected_session(
            Settings {
                mode: Mode::Live,
                ..Settings::default()
            },
            observation(),
        );
        session.continuous = true;
        let rejected = format!(
            "{}TypeSafe probabilities do not sum to one (sum 0.9000 over 2 options)",
            crate::provider::REJECTED_ANSWER
        );
        session.pending = Some(pending_with(&session, Err(rejected.clone())));

        session.tick();

        assert!(
            session.view.last_error.is_none(),
            "{:?}",
            session.view.last_error
        );
        assert!(session.continuous);
        assert_eq!(session.view.rejected_answers, 1);
        assert!(
            session
                .view
                .events
                .iter()
                .any(|event| event.kind == "rejected" && event.message == rejected)
        );
        assert!(
            session.dispatched.is_none(),
            "a rejected answer is never acted on"
        );
    }

    #[tokio::test]
    async fn a_transport_failure_still_ends_the_session() {
        let (mut session, _commands) = connected_session(
            Settings {
                mode: Mode::Live,
                ..Settings::default()
            },
            observation(),
        );
        session.continuous = true;
        session.pending = Some(pending_with(
            &session,
            Err("TypeSafe request failed".into()),
        ));

        session.tick();

        assert_eq!(
            session.view.last_error.as_deref(),
            Some("TypeSafe request failed")
        );
    }

    fn stale_answer() -> Decision {
        Decision {
            choice: "wait".into(),
            probabilities: std::collections::BTreeMap::new(),
            confidence: None,
            model: "test".into(),
            input_tokens: None,
            output_tokens: None,
            latency_ms: 0,
        }
    }

    /// Regression for "keep fleeing under fire": with the reflex on, a hit is survived, but
    /// an answer requested before the hit failed the health freshness check and ended the
    /// night run as `provider_failed`. The answer is stale and must be dropped, not fatal.
    #[tokio::test]
    async fn with_the_reflex_an_answer_overtaken_by_a_hit_is_dropped_and_the_loop_keeps_running() {
        let start = observation();
        let (mut session, _commands, feed) = fed_session(
            Settings {
                mode: Mode::Live,
                safety_reflex: true,
                ..Settings::default()
            },
            start.clone(),
        );
        session.continuous = true;
        session.next_request_at = Instant::now() + Duration::from_secs(60);
        session.pending = Some(pending_with(&session, Ok(stale_answer())));
        let mut hurt = next(&start);
        hurt.health = 16.0;
        feed.send_replace(Some(hurt));

        session.tick();

        assert!(
            session.view.last_error.is_none(),
            "{:?}",
            session.view.last_error
        );
        assert!(session.continuous, "the run mode survives the stale answer");
        assert!(session.pending.is_none(), "the answer was consumed");
        assert!(
            session
                .view
                .events
                .iter()
                .any(|event| event.kind == "rejected"),
            "the dropped answer is recorded"
        );
        assert!(
            session.dispatched.is_none() && session.active.is_none(),
            "a stale answer is never acted on"
        );
    }

    /// Without the reflex the same hit is a local stop before the pending answer is read;
    /// the harness resumes `HEALTH_DECREASED`, so this path is already survivable.
    /// See also `without_the_reflex_a_hit_is_still_a_local_stop`.
    #[tokio::test]
    async fn without_the_reflex_an_answer_overtaken_by_a_hit_is_still_a_local_stop() {
        let start = observation();
        let (mut session, _commands, feed) = fed_session(
            Settings {
                mode: Mode::Live,
                ..Settings::default()
            },
            start.clone(),
        );
        session.continuous = true;
        session.pending = Some(pending_with(&session, Ok(stale_answer())));
        let mut hurt = next(&start);
        hurt.health = 16.0;
        feed.send_replace(Some(hurt));

        session.tick();

        assert_eq!(session.view.last_error.as_deref(), Some(HEALTH_DECREASED));
        assert!(session.dispatched.is_none() && session.active.is_none());
    }

    /// Same family: a hit while an action waits for adapter acceptance makes the adapter
    /// reject it with "Health decreased before action acceptance". With the reflex on the
    /// hit is survivable, so the rejection is recorded and the loop goes on.
    #[tokio::test]
    async fn with_the_reflex_an_action_rejected_after_a_hit_is_recorded_and_the_loop_keeps_running()
    {
        let start = observation();
        let (mut session, mut commands, feed) = fed_session(
            Settings {
                mode: Mode::Live,
                safety_reflex: true,
                ..Settings::default()
            },
            start.clone(),
        );
        session.continuous = true;
        session.next_request_at = Instant::now() + Duration::from_secs(60);
        let candidate = Candidate {
            id: "wait".into(),
            description: "Wait".into(),
            target: None,
            duration_ms: 100,
        };
        session.apply(
            candidate.clone(),
            "Jev selected goal; local executor",
            vec![candidate],
            None,
            None,
        );
        let AdapterCommand::Execute(request) = commands.try_recv().unwrap() else {
            panic!("expected dispatch");
        };
        request
            .reply
            .send(Err("Health decreased before action acceptance"))
            .unwrap();
        let mut hurt = next(&start);
        hurt.health = 16.0;
        feed.send_replace(Some(hurt));

        session.tick();

        assert!(
            session.view.last_error.is_none(),
            "{:?}",
            session.view.last_error
        );
        assert!(session.continuous, "the run mode survives the rejection");
        assert!(session.dispatched.is_none() && session.active.is_none());
        assert!(
            session
                .view
                .events
                .iter()
                .any(|event| event.kind == "rejected"),
            "the rejected action is recorded"
        );
        assert!(
            !session
                .view
                .events
                .iter()
                .any(|event| event.kind == "action"),
            "a rejected action is never recorded as accepted"
        );
    }
}
