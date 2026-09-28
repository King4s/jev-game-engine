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
    refusal::Refusal,
};

/// Pause reasons recorded when the observed world changes under a running session. Both
/// are a local stop, not a failure: the run mode survives, the paused loop continues as soon
/// as the adapter publishes the new world, and an operator (or the headless harness) can
/// resume explicitly with `Start` as well.
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
/// hit arrived: the action was bound to a body that no longer held.
///
/// It is the typed reason itself, so the check is matched as the check it is instead of by
/// comparing a sentence. The engine no longer special-cases it: every refusal is recorded
/// with the concrete check and leaves the run alive, so this reason is simply the one the
/// live adapter names when a hit lands on a queued action (see the `rejected` path in
/// [`Session::tick`]).
pub const REJECTED_AFTER_HIT: Refusal = Refusal::HealthDecreasedBeforeAcceptance;

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
    receiver: tokio::sync::oneshot::Receiver<Result<Instant, Refusal>>,
}

type RoutingCompletion = (String, u64, u64, u32, crate::routing::RoutedDecision);
type RoutingStage = (String, u64, u64, crate::routing::StageAudit);

struct BlockedGather {
    id: String,
    reason: String,
    world_epoch: u64,
    dimension: Option<String>,
    position: Position,
    held_item: Option<String>,
    items: BTreeMap<String, u32>,
    approach: Position,
}

impl BlockedGather {
    fn unchanged_prerequisites(&self, observation: &Observation) -> bool {
        self.world_epoch == observation.world_epoch
            && self.dimension == observation.dimension
            && distance(&self.position, &observation.position) < ARRIVAL_TOLERANCE_M
            && self.held_item == observation.held_item
            && self.items == observation.items
    }

    fn still_applies(&self, observation: &Observation, candidate: &Candidate) -> bool {
        let Some(WoodSkill::Gather { approach, .. }) = candidate.skill.as_ref() else {
            return false;
        };
        self.id == candidate.id
            && self.unchanged_prerequisites(observation)
            && distance(&self.approach, approach) < 0.01
    }
}

struct Session {
    route_tx: mpsc::UnboundedSender<RoutingCompletion>,
    route_rx: mpsc::UnboundedReceiver<RoutingCompletion>,
    stage_tx: mpsc::UnboundedSender<RoutingStage>,
    stage_rx: mpsc::UnboundedReceiver<RoutingStage>,
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
    skill_baseline: Option<(u64, std::collections::BTreeMap<String, u32>, u64)>,
    started: Instant,
    id: String,
    latency: LatencyPolicy,
    /// Measured answer time of the world peer (the Minecraft client and adapter), used for
    /// action acknowledgement and observation freshness. Never a fixed second: connection
    /// quality and client throughput differ per user, so the budget follows measurement.
    client: LatencyPolicy,
    observed_at: Instant,
    /// Measured interval between the observations this adapter publishes. Observation
    /// freshness is judged by this stream, never by the action acknowledgement latency:
    /// both are measured, but they measure different things.
    observation_interval: LatencyPolicy,
    /// Set when the observed world changed under a running session: the run mode survives,
    /// but the paced loop waits for a fresh observation of the new world before asking
    /// again. Cleared by an operator command and by that first fresh observation.
    paused: bool,
    /// Earliest instant the paced model loop may issue its next request.
    next_request_at: Instant,
    /// When the safety reflex last dispatched an action.
    last_reflex_at: Option<Instant>,
    /// Reason of the last recorded idle report. A stable empty scene reports once instead of
    /// once per observation, and reports again only when the reason itself changes.
    last_idle_report: Option<String>,
    /// Concrete gather failures that remain impossible under the same observed prerequisites.
    blocked_gathers: Vec<BlockedGather>,
}

impl Session {
    fn new(shared: Arc<Mutex<View>>) -> Self {
        let (route_tx, route_rx) = mpsc::unbounded_channel();
        let (stage_tx, stage_rx) = mpsc::unbounded_channel();
        let view = View {
            answer_age_limit_ms: LatencyPolicy::default().timing().max_answer_age_ms,
            ..View::default()
        };
        Self {
            route_tx,
            route_rx,
            stage_tx,
            stage_rx,
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
            skill_baseline: None,
            started: Instant::now(),
            id: session_id(),
            latency: LatencyPolicy::default(),
            client: LatencyPolicy::default(),
            observed_at: Instant::now(),
            observation_interval: LatencyPolicy::default(),
            paused: false,
            next_request_at: Instant::now(),
            last_reflex_at: None,
            last_idle_report: None,
            blocked_gathers: Vec::new(),
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
        let observation = self.view.observation.clone();
        self.view.manual_candidates = if !self.view.replay
            && self.adapter.is_some()
            && self.observed_at.elapsed() <= self.observation_budget()
        {
            observation
                .filter(|o| o.connected)
                .map(|mut o| self.executable_choices(&mut o, self.latency.timing().goal_ms))
                .unwrap_or_default()
        } else {
            vec![]
        };
    }

    fn cancel_skill_event(&mut self, reason: &str) {
        let candidate = self
            .active
            .as_ref()
            .map(|(c, _)| c)
            .or_else(|| self.dispatched.as_ref().map(|d| &d.candidate))
            .filter(|c| c.skill.is_some())
            .cloned();
        if let Some(candidate) = candidate
            && let Some((epoch, _, attempt)) = self.skill_baseline.take()
        {
            self.event("skill",format!("Wood skill cancelled: attempt={attempt}, world_epoch={epoch}; {reason}; engine requested cancellation, not server-confirmed completion"),vec![candidate],None);
        }
    }

    fn cancel_pending_route(&mut self, reason: &str) {
        self.record_routes();
        if let Some(pending) = self.pending.take() {
            pending.task.abort();
            if self.view.events.len() < 4_999 {
                self.event(
                    "model_route_cancelled",
                    serde_json::json!({
                        "observation_sequence":pending.observation.sequence,
                        "generation":pending.generation,"elapsed_ms":elapsed(pending.started),
                        "status":"cancelled","reason":reason,"remaining_provider_usage":"unavailable"
                    })
                    .to_string(),
                    vec![],
                    None,
                );
            }
        }
    }

    fn invalidate(&mut self, disconnect: bool) {
        self.record_routes();
        self.cancel_skill_event(if disconnect {
            "session stopped/disconnected"
        } else {
            "session invalidated/paused"
        });
        self.generation = self.generation.wrapping_add(1);
        self.continuous = false;
        self.one_step = false;
        self.paused = false;
        self.cancel_pending_route("session invalidated");
        self.active = None;
        self.skill_baseline = None;
        self.dispatched = None;
        self.blocked_gathers.clear();
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
        self.cancel_skill_event(&message);
        self.invalidate(false);
        self.view.status = "Paused: error".into();
        self.view.last_error = Some(message.clone());
        self.event("error", message, vec![], None);
    }

    /// Pauses a session that is still running because the world it observed changed under it.
    ///
    /// A dimension or world-lifecycle change is a mechanical event, not a policy decision: the
    /// world the pending answer and the accepted action belonged to no longer exists, so both
    /// are dropped (the local stop a pause performs), but the run mode survives and the paced
    /// loop continues once the adapter publishes a fresh observation of the new world. Ending
    /// the session is the operator's call, so the reason is recorded exactly as the harness
    /// classifies it (see `harness::classify_error`) and an explicit `Start` resumes too; the
    /// request and time budgets still stop the session on their own.
    fn pause_world_change(&mut self, message: &'static str) {
        self.clear_in_flight(message);
        self.paused = true;
        self.view.status = "Paused — the observed world changed".into();
        self.view.last_error = Some(message.into());
        self.event("world", message, vec![], None);
    }

    /// Ends a world-change pause once the adapter publishes an observation of the new world.
    /// An operator's own pause or stop is never undone here: only an armed run resumes.
    fn resume_world_change(&mut self) {
        self.paused = false;
        if !(self.continuous || self.one_step) {
            return;
        }
        self.view.status = "Resumed in the observed world".into();
        self.view.last_error = None;
        self.event(
            "world",
            "Fresh observation after the world change; the run continues",
            vec![],
            None,
        );
    }

    /// How old an observation may be before the engine stops using it.
    ///
    /// Measured from the interval the adapter itself publishes at, because that is the
    /// quantity being judged: a peer that publishes every 250 ms is not silent 250 ms later.
    /// Deriving this bound from the action acknowledgement latency instead measured a
    /// different stream — the first accepted action measured ~30 ms, the bound collapsed to
    /// ~120 ms, and the engine ended the live run inside the ordinary gap between two
    /// observations. The peer's measured turnaround stays a floor, so this bound is never
    /// tighter than the one it replaces.
    fn observation_budget(&self) -> Duration {
        self.observation_interval
            .action_budget()
            .max(self.client.action_budget())
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
                    session_id: Some(self.id.clone()),
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
                // A manual intervention invalidates pending work, not the observed
                // technical reason why another candidate cannot run yet.
                let blocked_gathers = std::mem::take(&mut self.blocked_gathers);
                self.invalidate(false);
                self.blocked_gathers = blocked_gathers;
                let Some(observation) = self.view.observation.clone().filter(|o| o.connected)
                else {
                    self.fail("No active observation for manual action");
                    return;
                };
                if self.observed_at.elapsed() > self.observation_budget() {
                    self.fail("Observation is too old");
                    return;
                }
                if observation.world_epoch != world_epoch || observation.dimension != dimension {
                    self.fail("Manual action belongs to a different world lifecycle");
                    return;
                }
                let mut current = observation.clone();
                let candidates =
                    self.executable_choices(&mut current, self.latency.timing().goal_ms);
                if candidates.contains(&candidate) {
                    self.apply(
                        candidate,
                        "Manual action; mixed control",
                        candidates,
                        None,
                        None,
                    );
                } else if let Some(blocked) = self
                    .blocked_gathers
                    .iter()
                    .find(|blocked| blocked.still_applies(&observation, &candidate))
                {
                    self.event(
                        "rejected",
                        format!(
                            "Manual action {} unavailable: {}",
                            blocked.id, blocked.reason
                        ),
                        vec![candidate],
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
                        let requests = crate::recording::request_count(&recording);
                        self.settings = recording.settings.clone();
                        self.id = recording.id.clone();
                        self.view = View {
                            session_id: Some(recording.id.clone()),
                            status: "Replay — recorded telemetry, no connection".into(),
                            mode: recording.settings.mode,
                            replay: true,
                            observation: recording
                                .events
                                .iter()
                                .rev()
                                .find_map(|e| e.observation.clone()),
                            requests,
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

    fn record_routes(&mut self) {
        // Replay must remain the saved artifact, never receive late live telemetry.
        if self.view.replay {
            return;
        }
        while let Ok((session_id, generation, observation_sequence, stage)) =
            self.stage_rx.try_recv()
        {
            self.event(
                "model_stage",
                serde_json::json!({
                    "session_id":session_id,"request_generation":generation,
                    "observation_sequence":observation_sequence,"stage":stage
                })
                .to_string(),
                vec![],
                None,
            );
        }
        while let Ok((session_id, generation, observation_sequence, reserved, routed)) =
            self.route_rx.try_recv()
        {
            if session_id == self.id {
                self.view.requests = self
                    .view
                    .requests
                    .saturating_sub(reserved.saturating_sub(routed.api_calls));
            }
            let (status, executor, reason) = match &routed.outcome {
                crate::routing::RouteOutcome::Resolved { decision } => (
                    "resolved",
                    Some(serde_json::json!({"model":decision.model,
                            "input_tokens":decision.input_tokens,"output_tokens":decision.output_tokens,
                            "latency_ms":decision.latency_ms})),
                    None,
                ),
                crate::routing::RouteOutcome::Blocked { reason } => ("blocked", None, Some(reason)),
            };
            self.event(
                    "model_route",
                    serde_json::json!({
                    "api_calls":routed.api_calls,
                    "session_id":session_id,
                        "request_generation":generation,"current_generation":generation == self.generation,
                        "stages":routed.stages,
                        "observation_sequence":observation_sequence,"route":routed.route,
                        "status":status,"executor":executor,"reason":reason,
                        "total_latency_ms":routed.total_latency_ms,
                        "usage_rule":"Jev route and Jev action share one request; count once"
                    })
                    .to_string(),
                    vec![],
                    None,
                );
        }
    }

    fn tick(&mut self) {
        self.record_routes();
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
                // Measured before `observed_at` moves. Only two observations the engine was
                // actually following are a cadence sample: a transfer gap, and the silence a
                // pause waited out, are not the rate this adapter publishes at and must not
                // become the bound that judges it.
                if !world_changed && !self.paused && self.view.observation.is_some() {
                    self.observation_interval.record(elapsed(self.observed_at));
                    self.view.observation_budget_ms =
                        u64::try_from(self.observation_budget().as_millis()).unwrap_or(u64::MAX);
                }
                self.observed_at = Instant::now();
                self.view.observation = Some(observation.clone());
                // Checked before the world change: a death followed by a respawn in another
                // dimension is a death, not a transfer.
                if died {
                    self.fail(BOT_DIED);
                    return;
                }
                if world_changed {
                    self.pause_world_change(if dimension_changed {
                        DIMENSION_CHANGED
                    } else {
                        WORLD_LIFECYCLE_CHANGED
                    });
                    return;
                }
                // The adapter is publishing from the world the pause was waiting for.
                if self.paused && observation.connected {
                    self.resume_world_change();
                }
                if !observation.connected {
                    self.invalidate(true);
                    self.view.status = "Minecraft/fixture disconnected".into();
                    self.event("disconnect", observation.note, vec![], None);
                    return;
                }
                if hurt && self.reflex_session() {
                    if self.active.as_ref().is_some_and(|(c, _)| c.skill.is_some())
                        || self
                            .dispatched
                            .as_ref()
                            .is_some_and(|d| d.candidate.skill.is_some())
                    {
                        self.clear_in_flight("damage during wood skill");
                    }
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
        if self.active.is_some() && self.observed_at.elapsed() > self.observation_budget() {
            self.fail("Observations became stale: local stop");
            return;
        }
        // The peer's answer budget is measured, never a fixed second: a distant model or a
        // busy client must not fail a deadline tuned to somebody else's connection.
        let client_budget = self.client.action_budget();
        let acknowledgement =
            self.dispatched
                .as_mut()
                .and_then(|action| match action.receiver.try_recv() {
                    Ok(Ok(accepted_at)) if accepted_at >= action.accepted_before => {
                        Some(Err(Refusal::AdapterAcceptanceDeadlineExpired))
                    }
                    Ok(Ok(accepted_at))
                        if accepted_at < action.sent_at || accepted_at > Instant::now() =>
                    {
                        Some(Err(Refusal::InvalidAdapterTimestamp))
                    }
                    Ok(result) => Some(result),
                    Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                        Some(Err(Refusal::AdapterEndedBeforeAccepting))
                    }
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty)
                        if Instant::now() >= action.accepted_before
                            || action.sent_at.elapsed() > client_budget =>
                    {
                        Some(Err(Refusal::AdapterAcknowledgementTimedOut))
                    }
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty) => None,
                });
        if let Some(result) = acknowledgement {
            let action = self.dispatched.take().expect("dispatched action exists");
            self.client
                .record(u64::try_from(action.sent_at.elapsed().as_millis()).unwrap_or(u64::MAX));
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
                Err(refusal) => {
                    // The concrete check the local guard or the engine's own bounds failed is
                    // what the recording keeps, verbatim.
                    //
                    // A refusal is a technical answer about one action, never a verdict on the
                    // run: ending the session on one had the engine deciding policy, and it
                    // killed a live creative run the moment the model chose a goal this build
                    // has no executor for. Nothing is executed, the reason is reported where a
                    // client looks for one and where the harness classifies one, and the paced
                    // loop asks again with the refused option still in the catalogue and still
                    // marked. The request and time budgets end the session on their own, and
                    // nothing here resets or refunds either of them, so asking again is bounded
                    // and every cycle keeps counting.
                    self.view.refused_actions += 1;
                    self.view.last_refusal = Some(refusal.to_string());
                    self.view.status = if self.continuous {
                        "Refused action not executed; the loop asks again".into()
                    } else {
                        "Ready: the chosen goal was refused; no action executed".into()
                    };
                    self.event(
                        "rejected",
                        refusal.to_string(),
                        vec![action.candidate],
                        None,
                    );
                    return;
                }
            }
        }
        self.finish_skill();
        let active_goal = self
            .active
            .as_ref()
            .filter(|(candidate, _)| candidate.skill.is_none())
            .map(|(candidate, started)| {
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
                self.skill_baseline = None;
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
            self.record_routes();
            let pending = self.pending.take().expect("pending receiver exists");
            let age = elapsed(pending.started);
            self.latency.record(age);
            let timing = self.latency.timing();
            self.view.p50_ms = timing.p50_ms;
            self.view.client_p95_ms = self.client.p95_ms();
            self.view.client_action_budget_ms =
                u64::try_from(self.client.action_budget().as_millis()).unwrap_or(u64::MAX);
            self.view.p95_ms = timing.p95_ms;
            self.view.goal_ms = timing.goal_ms;
            self.view.answer_age_limit_ms = timing.max_answer_age_ms;
            match result {
                Err(message) if crate::provider::is_rejected_answer(&message) => {
                    // A malformed answer is dropped, never acted on; the paced loop sends
                    // its next request as usual. Safety never waited on this answer.
                    self.view.rejected_answers += 1;
                    self.event("rejected", message, vec![], None);
                    if !self.continuous {
                        self.view.status =
                            "Ready: model answer rejected; no action executed".into();
                    }
                    return;
                }
                Err(message) => {
                    self.fail(message);
                    return;
                }
                Ok(decision) => {
                    let same_world =
                        answer_is_current(pending.generation, self.generation, age, pending.timing)
                            && self.observed_at.elapsed() <= self.observation_budget()
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
                                decision.model.as_str()
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
                        let target_current = if candidate.skill.is_some() {
                            self.view.observation.as_ref().is_some_and(|now| {
                                crate::wood::skill_is_current(&candidate, now, &self.settings)
                            })
                        } else {
                            candidate.target.as_ref().is_none_or(|target| {
                                self.view.observation.as_ref().is_some_and(|now| {
                                    now.blocks.iter().any(|landmark| {
                                        (self.settings.mode == Mode::Demo
                                            || landmark.name.starts_with("waypoint:"))
                                            && distance(target, &landmark.position) < 0.01
                                    })
                                })
                            })
                        };
                        if !target_current {
                            self.fail("Current navigation prerequisites for the goal have changed");
                            return;
                        }
                        self.apply(
                            candidate,
                            if self.settings.mode == Mode::Demo {
                                "Fixture selected goal; local executor"
                            } else if decision.model == crate::routing::ASTRA_MODEL
                                || decision.model.starts_with("gpt-6-astra-")
                            {
                                "Astra selected goal; local executor"
                            } else if decision.model == crate::routing::JEV_ROUTER_MODEL {
                                "Jev Router selected goal; local executor"
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
            && !self.paused
            && self.pending.is_none()
            && self.active.is_none()
            && self.dispatched.is_none()
            && !self.reflex_in_flight()
            && Instant::now() >= self.next_request_at
        {
            self.request();
        }
    }

    fn finish_skill(&mut self) {
        let Some((candidate, started)) = self.active.clone().filter(|(c, _)| c.skill.is_some())
        else {
            return;
        };
        let outcome = self.view.observation.as_ref().and_then(|observation| {
            observation
                .skill_outcome
                .as_ref()
                .filter(|result| {
                    result.candidate_id == candidate.id
                        && result.world_epoch == observation.world_epoch
                        && self
                            .skill_baseline
                            .as_ref()
                            .is_some_and(|(epoch, _, attempt)| {
                                *epoch == result.world_epoch && *attempt == result.attempt_id
                            })
                })
                .cloned()
        });
        let expired = elapsed(started) >= candidate.duration_ms;
        if outcome.is_none() && !expired {
            return;
        }
        let gained = self
            .skill_baseline
            .as_ref()
            .is_some_and(|(epoch, before, _)| {
                self.view.observation.as_ref().is_some_and(|now| {
                    if !now.connected || now.world_epoch != *epoch {
                        return false;
                    }
                    match candidate.skill.as_ref() {
                        Some(WoodSkill::PlaceTable { .. }) => {
                            crate::placement::completed(&candidate, now, before)
                        }
                        Some(
                            WoodSkill::Gather { log, .. } | WoodSkill::RecoverDrop { log, .. },
                        ) => {
                            now.items.get(log).copied().unwrap_or(0)
                                > before.get(log).copied().unwrap_or(0)
                        }
                        Some(skill) => crate::wood::recipe(skill).is_some_and(
                            |(ingredients, output, count)| {
                                let mut expected = before.clone();
                                for item in ingredients {
                                    let Some(total) = expected.get_mut(&item) else {
                                        return false;
                                    };
                                    let Some(remaining) = total.checked_sub(1) else {
                                        return false;
                                    };
                                    *total = remaining;
                                }
                                *expected.entry(output).or_default() += count;
                                expected.retain(|_, count| *count > 0);
                                let mut actual = now.items.clone();
                                actual.retain(|_, count| *count > 0);
                                actual == expected
                            },
                        ),
                        None => false,
                    }
                })
            });
        let success = outcome
            .as_ref()
            .is_some_and(|result| result.success && result.confirmed_by_server)
            && gained;
        let blocked = if outcome.as_ref().is_some_and(|result| {
            !result.success
                && result
                    .message
                    .starts_with("Gather break not started by the local client")
        }) {
            match (candidate.skill.as_ref(), self.view.observation.as_ref()) {
                (Some(WoodSkill::Gather { approach, .. }), Some(now)) => Some(BlockedGather {
                    id: candidate.id.clone(),
                    reason: outcome
                        .as_ref()
                        .expect("failed outcome exists")
                        .message
                        .clone(),
                    world_epoch: now.world_epoch,
                    dimension: now.dimension.clone(),
                    position: now.position.clone(),
                    held_item: now.held_item.clone(),
                    items: now.items.clone(),
                    approach: approach.clone(),
                }),
                _ => None,
            }
        } else {
            None
        };
        let message = if success {
            "Wood skill succeeded: server confirmed completion and expected inventory change"
                .to_owned()
        } else {
            format!(
                "Wood skill failed: {}",
                outcome.map(|result| result.message).unwrap_or_else(|| {
                    "action deadline expired without confirmed inventory gain".into()
                })
            )
        };
        if let Some(adapter) = &self.adapter {
            let _ = adapter.commands.send(AdapterCommand::Stop);
        }
        self.active = None;
        self.skill_baseline = None;
        self.view.active_goal = None;
        if let Some(blocked) = blocked {
            self.blocked_gathers.retain(|prior| prior.id != blocked.id);
            self.blocked_gathers.push(blocked);
        }
        self.event("skill", message, vec![candidate], None);
        self.view.status = "Ready for the next goal".into();
    }

    /// Whether the action in flight was dispatched by the safety reflex.
    fn reflex_in_flight(&self) -> bool {
        self.dispatched
            .as_ref()
            .is_some_and(|action| action.origin.starts_with("SAFETY-REFLEX"))
    }

    /// Stops local movement and invalidates in-flight model answers without ending the
    /// session's run mode. A pause uses `invalidate`, which also clears the run mode; the
    /// safety reflex and a world change both continue the run afterwards, so they need this
    /// narrower stop. Preempting an idling goal means its arrival verdict is never recorded —
    /// the same loss a manual takeover causes, and the `reflex` event names the goal it
    /// superseded so the hole in the chain is visible in the recording.
    fn clear_in_flight(&mut self, reason: &str) {
        self.cancel_skill_event(reason);
        self.dispatched = None;
        self.generation = self.generation.wrapping_add(1);
        self.cancel_pending_route(reason);
        self.active = None;
        self.skill_baseline = None;
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
            || self.paused
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
            .is_some_and(|(candidate, _)| candidate.skill.is_none() && candidate.target.is_some())
        {
            return;
        }
        if self.observed_at.elapsed() > self.observation_budget() {
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
        self.clear_in_flight("nearby threat safety reflex");
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

    /// Re-evaluate technical gather blockers against the observation supplied to this choice.
    /// This seam is independent of provider credentials and machine-speed freshness gates.
    fn executable_choices(
        &mut self,
        observation: &mut Observation,
        goal_ms: u64,
    ) -> Vec<Candidate> {
        let mut candidates = candidates_with_settings(observation, goal_ms, &self.settings);
        self.blocked_gathers.retain(|blocked| {
            blocked.unchanged_prerequisites(observation)
                && candidates
                    .iter()
                    .find(|candidate| candidate.id == blocked.id)
                    .is_none_or(|candidate| blocked.still_applies(observation, candidate))
        });
        for blocked in &self.blocked_gathers {
            candidates.retain(|candidate| candidate.id != blocked.id);
            observation.note.push_str(&format!(
                "; temporarily unavailable {}: {} (retry after a changed position, tool, inventory, route or world)",
                blocked.id, blocked.reason
            ));
        }
        for goal in crate::catalog::goals(observation, &self.settings) {
            if crate::catalog::GoalKind::LADDER.iter().any(|kind| {
                kind.id() == goal.id
                    && crate::catalog::goal_executor(*kind)
                        == crate::catalog::GoalExecutor::KnowledgeOnly
            }) {
                observation.note.push_str(&format!(
                    "; knowledge only {}: {} (no executor; not selectable)",
                    goal.id, goal.description
                ));
            }
        }
        for attack in crate::catalog::attacks(observation, &self.settings) {
            observation.note.push_str(&format!(
                "; knowledge only {}: {} (no attack executor; not selectable)",
                attack.id, attack.description
            ));
        }
        candidates
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
        if self.observed_at.elapsed() > self.observation_budget() {
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
        let mut observation = observation;
        let candidates = self.executable_choices(&mut observation, timing.goal_ms);
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
        if candidates.is_empty() {
            // Nothing actionable is on offer, so there is nothing to ask. Keep observing and
            // record the reason instead of sending a request the model cannot answer usefully.
            // Re-observing costs no provider call, so the idle cadence is the configured pacing
            // itself; a fixed one-second floor here only delayed noticing that the world changed.
            self.next_request_at =
                Instant::now() + Duration::from_millis(self.settings.request_interval_ms);
            let reason = format!(
                "No actionable candidate; engine observes without a model request{no_navigation}"
            );
            if self.last_idle_report.as_deref() != Some(reason.as_str()) {
                self.last_idle_report = Some(reason.clone());
                self.event("idle", reason, vec![], None);
            }
            return;
        }
        self.last_idle_report = None;
        let (tx, receiver) = mpsc::unbounded_channel();
        let request_observation = observation.clone();
        let request_candidates = candidates.clone();
        let objective = self.settings.objective.trim().to_owned();
        // The paced loop may not ask again before this instant, so a long session
        // bounds provider spend by wall-clock rather than by action count alone.
        self.next_request_at =
            Instant::now() + Duration::from_millis(self.settings.request_interval_ms);
        let started = Instant::now();
        let route_tx = self.route_tx.clone();
        let stage_tx = self.stage_tx.clone();
        let session_id = self.id.clone();
        let generation = self.generation;
        let remaining_requests = self
            .settings
            .max_requests
            .saturating_sub(self.view.requests);
        let router_key = read_router_key_with(|name| std::env::var(name).ok());
        // This run's own handler-confidence floor, decided per run rather than baked in.
        let request_min_confidence = self.settings.route_min_confidence;
        let reserved = if key.is_some() && router_key.is_some() {
            remaining_requests.min(2)
        } else {
            1
        };
        // Reserve before any provider future can run. Cancellation retains an
        // uncertain reservation; only a completed result refunds unused calls.
        self.view.requests += reserved;
        let task = tokio::spawn(async move {
            let result = if let Some(key) = key {
                match crate::routing::decide_observed(
                    (&key, router_key.as_deref()),
                    crate::routing::RoutingRequest {
                        observation: &request_observation,
                        candidates: &request_candidates,
                        objective: &objective,
                        timeout: Duration::from_secs(10),
                        remaining_requests: reserved,
                        min_confidence: request_min_confidence,
                    },
                    |stage| {
                        let _ = stage_tx.send((
                            session_id.clone(),
                            generation,
                            request_observation.sequence,
                            stage,
                        ));
                    },
                )
                .await
                {
                    Ok(routed) => {
                        let result = match &routed.outcome {
                            crate::routing::RouteOutcome::Resolved { decision } => {
                                Ok(decision.clone())
                            }
                            crate::routing::RouteOutcome::Blocked { reason } => Err(reason.clone()),
                        };
                        let _ = route_tx.send((
                            session_id,
                            generation,
                            request_observation.sequence,
                            reserved,
                            routed,
                        ));
                        result
                    }
                    Err(error) => Err(error.to_string()),
                }
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
        if let Some(event) = self.view.events.last_mut() {
            event.observation = Some(observation);
        }
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
        if candidate.skill.is_some()
            && !crate::wood::skill_is_current(&candidate, observation, &self.settings)
        {
            self.event(
                "rejected",
                "Wood prerequisites changed before dispatch",
                vec![candidate],
                None,
            );
            return;
        }
        let (request, receiver) = ActionRequest::new(candidate.clone(), observation);
        self.skill_baseline = candidate.skill.as_ref().map(|_| {
            (
                observation.world_epoch,
                observation.items.clone(),
                request.attempt_id,
            )
        });
        let now = Instant::now();
        let accept_budget = self.client.action_budget();
        let accepted_before = (now + accept_budget)
            .min(self.observed_at + accept_budget)
            .min(
                self.started
                    .checked_add(Duration::from_secs(self.settings.max_seconds))
                    .unwrap_or(now),
            )
            .min(response_deadline.unwrap_or(now + accept_budget));
        if now >= accepted_before {
            self.fail("Action acceptance deadline expired before dispatch");
            return;
        }
        let request = request.with_deadline(accepted_before);
        let sent = self.adapter.as_ref().is_some_and(|a| {
            a.commands
                .send(AdapterCommand::Execute(Box::new(request)))
                .is_ok()
        });
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

/// The whole catalogue handed to the model, derived on every request from this
/// observation: the goals the world makes possible, then the immediately executable
/// options of every category. Nothing is filtered out because another category exists.
fn candidates_with_settings(
    observation: &Observation,
    duration_ms: u64,
    settings: &Settings,
) -> Vec<Candidate> {
    crate::catalog::candidates(observation, duration_ms, settings)
}

/// The move, attack, flight and stay-put options alone. Kept as its own entry point for
/// the engine's own flight choice and for the demo fixture.
fn candidates(observation: &Observation, duration_ms: u64, mode: &Mode) -> Vec<Candidate> {
    let settings = Settings {
        mode: mode.clone(),
        ..Settings::default()
    };
    crate::catalog::moves(observation, duration_ms, &settings)
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

fn read_router_key_with(get_var: impl FnOnce(&str) -> Option<String>) -> Option<String> {
    get_var("OPENROUTER_API_KEY").filter(|key| !key.trim().is_empty())
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

    #[test]
    fn openai_key_cannot_become_router_credential() {
        let key = read_router_key_with(|name| match name {
            "OPENAI_API_KEY" => Some("fake-openai-key".into()),
            "OPENROUTER_API_KEY" => None,
            _ => panic!("unexpected credential source"),
        });
        assert!(key.is_none());

        let key = read_router_key_with(|name| match name {
            "OPENROUTER_API_KEY" => Some("fake-router-key".into()),
            _ => panic!("unexpected credential source"),
        });
        assert_eq!(key.as_deref(), Some("fake-router-key"));
    }

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
            // One reachable waypoint, so the fixture has a legal bounded action to choose.
            // There is no idle candidate any more: an engine with nothing actionable records
            // an idle report instead of asking the model.
            blocks: vec![Landmark {
                name: "waypoint:4:64:0".into(),
                position: Position {
                    x: 4.5,
                    y: 64.0,
                    z: 0.5,
                },
            }],
            entities: vec![],
            note: "test fixture".into(),
            ..Default::default()
        }
    }

    #[test]
    fn retried_same_log_rejects_old_attempt_outcomes_even_after_inventory_gain() {
        for old_success in [false, true] {
            let mut session = session();
            let mut observed = observation();
            let candidate = Candidate {
                id: "gather_2_64_0".into(),
                description: "test gather".into(),
                target: Some(Position {
                    x: 2.5,
                    y: 64.5,
                    z: 0.5,
                }),
                duration_ms: 10_000,
                skill: Some(WoodSkill::Gather {
                    log: "minecraft:oak_log".into(),
                    approach: observed.position.clone(),
                    route: vec![observed.position.clone()],
                }),
            };
            let (first, _first_reply) = ActionRequest::new(candidate.clone(), &observed);
            let (retry, _retry_reply) = ActionRequest::new(candidate.clone(), &observed);
            assert_ne!(first.attempt_id, retry.attempt_id);
            session.skill_baseline = Some((
                observed.world_epoch,
                observed.items.clone(),
                retry.attempt_id,
            ));
            observed.items.insert("minecraft:oak_log".into(), 1);
            observed.skill_outcome = Some(crate::model::SkillOutcome {
                candidate_id: candidate.id.clone(),
                world_epoch: observed.world_epoch,
                attempt_id: first.attempt_id,
                success: old_success,
                confirmed_by_server: true,
                message: "old attempt".into(),
            });
            session.active = Some((candidate, Instant::now()));
            session.view.observation = Some(observed);
            session.finish_skill();
            assert!(
                session.active.is_some(),
                "an old terminal result cannot end a retry"
            );
            assert!(!session.view.events.iter().any(|e| e.kind == "skill"));
            let result = session
                .view
                .observation
                .as_mut()
                .unwrap()
                .skill_outcome
                .as_mut()
                .unwrap();
            result.attempt_id = retry.attempt_id;
            result.success = true;
            session.finish_skill();
            assert!(session.active.is_none());
            assert!(
                session
                    .view
                    .events
                    .iter()
                    .any(|e| e.kind == "skill" && e.message.contains("succeeded"))
            );
        }
    }

    #[test]
    fn crafting_requires_exact_ingredient_consumption_and_output_not_just_item_gain() {
        for (consume, extra_change, expected_success) in [
            (true, false, true),
            (false, false, false),
            (true, true, false),
        ] {
            let mut session = session();
            let mut observed = observation();
            observed.items.insert("minecraft:oak_log".into(), 2);
            observed.items.insert("minecraft:dirt".into(), 3);
            session.skill_baseline = Some((observed.world_epoch, observed.items.clone(), 91));
            if consume {
                observed.items.insert("minecraft:oak_log".into(), 1);
            }
            observed.items.insert("minecraft:oak_planks".into(), 4);
            if extra_change {
                observed.items.insert("minecraft:dirt".into(), 2);
            }
            let candidate = Candidate {
                id: "craft_planks".into(),
                description: "test".into(),
                target: None,
                duration_ms: 10_000,
                skill: Some(WoodSkill::CraftPlanks {
                    log: "minecraft:oak_log".into(),
                    planks: "minecraft:oak_planks".into(),
                }),
            };
            observed.skill_outcome = Some(crate::model::SkillOutcome {
                candidate_id: candidate.id.clone(),
                world_epoch: observed.world_epoch,
                attempt_id: 91,
                success: true,
                confirmed_by_server: true,
                message: "server result".into(),
            });
            session.active = Some((candidate, Instant::now()));
            session.view.observation = Some(observed);
            session.finish_skill();
            let event = session
                .view
                .events
                .iter()
                .find(|e| e.kind == "skill")
                .unwrap();
            assert_eq!(
                event.message.contains("succeeded"),
                expected_success,
                "{}",
                event.message
            );
        }
    }

    #[test]
    fn gather_needs_server_confirmation_and_inventory_gain_together() {
        for (confirmed, gain) in [(false, true), (true, false)] {
            let mut session = session();
            let mut observed = observation();
            let candidate = Candidate {
                id: "gather".into(),
                description: "test".into(),
                target: None,
                duration_ms: 10_000,
                skill: Some(WoodSkill::Gather {
                    log: "minecraft:oak_log".into(),
                    approach: observed.position.clone(),
                    route: vec![],
                }),
            };
            session.skill_baseline = Some((observed.world_epoch, observed.items.clone(), 77));
            if gain {
                observed.items.insert("minecraft:oak_log".into(), 1);
            }
            observed.skill_outcome = Some(crate::model::SkillOutcome {
                candidate_id: candidate.id.clone(),
                world_epoch: observed.world_epoch,
                attempt_id: 77,
                success: true,
                confirmed_by_server: confirmed,
                message: "test completion".into(),
            });
            session.active = Some((candidate, Instant::now()));
            session.view.observation = Some(observed);
            session.finish_skill();
            assert!(
                session
                    .view
                    .events
                    .iter()
                    .any(|e| e.kind == "skill" && e.message.contains("failed"))
            );
            assert!(
                !session
                    .view
                    .events
                    .iter()
                    .any(|e| e.message.contains("succeeded"))
            );
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
            skill: None,
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
        assert!(
            session.continuous,
            "a world change pauses the run instead of ending it"
        );
        assert!(session.paused, "the paused loop waits for the new world");
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

    /// Regression for the live practice run, which ended as `provider_failed` seconds after
    /// the bot was moved into the practice world while the engine was connected: a world
    /// change is a mechanical event, not a policy decision. The engine drops the pending
    /// answer and the accepted action that belonged to the vanished world, keeps the run
    /// mode, and continues in the new world once the adapter publishes it. Ending the
    /// session is the operator's call, not the engine's.
    #[tokio::test]
    async fn a_world_change_pauses_the_run_and_resumes_in_the_new_world() {
        let start = observation();
        let (mut session, mut commands, feed) = fed_session(Settings::default(), start.clone());
        session.continuous = true;
        let candidate = candidates(&start, 2_000, &Mode::Demo).pop().unwrap();
        session.active = Some((candidate.clone(), Instant::now()));
        session.view.active_goal = Some(candidate.id.clone());
        let (answer_tx, receiver) = mpsc::unbounded_channel();
        answer_tx
            .send(Ok(Decision {
                choice: candidate.id.clone(),
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
            observation: start.clone(),
            candidates: vec![candidate],
            timing: session.latency.timing(),
            receiver,
            task: tokio::spawn(std::future::pending()),
        });

        // The adapter publishes the transfer while an answer and an action are in flight.
        let mut moved = next(&start);
        moved.dimension = Some("fixture:the_nether".into());
        moved.world_epoch += 1;
        feed.send_replace(Some(moved.clone()));
        session.tick();

        assert!(
            session.continuous,
            "a world change is a local stop, not the end of the run"
        );
        assert_eq!(session.view.last_error.as_deref(), Some(DIMENSION_CHANGED));
        assert!(
            session.view.status.starts_with("Paused"),
            "{}",
            session.view.status
        );
        assert!(
            session.pending.is_none(),
            "the answer for the vanished world is dropped"
        );
        assert!(
            session.active.is_none(),
            "the accepted action for the vanished world is dropped"
        );
        assert!(session.view.active_goal.is_none());
        assert_eq!(
            session.view.requests, 0,
            "the paused loop spends no request"
        );
        assert!(
            !session
                .view
                .events
                .iter()
                .any(|event| event.kind == "action" || event.kind == "decision"),
            "nothing from the vanished world is acted on"
        );
        assert!(matches!(commands.try_recv(), Ok(AdapterCommand::Stop)));

        // The adapter publishes the new world; the run continues without an operator.
        feed.send_replace(Some(next(&moved)));
        session.tick();

        assert_eq!(
            session.view.last_error, None,
            "the pause reason clears when the new world is observed"
        );
        assert!(
            !session.view.status.starts_with("Paused"),
            "{}",
            session.view.status
        );
        assert_eq!(
            session.view.requests, 1,
            "the run asks the model again in the observed world"
        );
    }

    /// The regression a world-change pause must never cause: the first attempt cleared the
    /// run mode with `invalidate`, which silently disabled the continuous request-budget
    /// stop that `tests/engine_acceptance.rs` requires. The pause keeps the run mode, so the
    /// session still stops by itself once its request budget is used up.
    #[tokio::test]
    async fn a_world_change_pause_still_stops_at_the_request_budget() {
        let start = observation();
        let (mut session, _commands, feed) = fed_session(
            Settings {
                max_requests: 1,
                ..Settings::default()
            },
            start.clone(),
        );
        session.continuous = true;
        session.view.requests = 1;

        let mut moved = next(&start);
        moved.dimension = Some("fixture:the_nether".into());
        feed.send_replace(Some(moved.clone()));
        session.tick();
        assert_eq!(session.view.last_error.as_deref(), Some(DIMENSION_CHANGED));
        assert!(session.continuous, "the pause keeps the run mode");

        feed.send_replace(Some(next(&moved)));
        session.tick();

        assert_eq!(
            session.view.last_error.as_deref(),
            Some("Request budget reached"),
            "the run stops by itself once its request budget is used up"
        );
    }

    /// Regression for the live run's actual death: the first accepted action measured a
    /// ~30 ms acknowledgement, the engine derived a ~120 ms bound from that measurement and
    /// then killed the session inside the ordinary 250 ms gap between two observations of
    /// the adapter's own publishing cadence. Observation freshness must be judged by the
    /// interval the adapter is measured to publish at, never by how fast it validates one
    /// action: the second measures a different stream.
    #[tokio::test]
    async fn observation_freshness_follows_the_measured_publish_interval_not_the_action_acknowledgement()
     {
        let start = observation();
        let (mut session, _commands, feed) = fed_session(Settings::default(), start.clone());
        // A world peer that publishes an observation every 250 ms.
        for _ in 0..8 {
            session.observed_at = Instant::now() - Duration::from_millis(250);
            let latest = session.view.observation.clone().unwrap();
            feed.send_replace(Some(next(&latest)));
            session.tick();
        }
        // The same peer validates a dispatched action in 30 ms.
        session.client.record(30);
        assert_eq!(session.client.action_budget(), Duration::from_millis(120));
        let candidate = candidates(&start, 2_000, &Mode::Demo).pop().unwrap();
        session.active = Some((candidate, Instant::now()));
        session.observed_at = Instant::now() - Duration::from_millis(250);

        session.tick();

        assert_eq!(
            session.view.last_error, None,
            "a peer inside its own measured publish interval is not silent"
        );
        assert!(session.active.is_some(), "the bounded action keeps running");
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
        Box<crate::adapter::ActionRequest>,
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
            skill: None,
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
        request
            .reply
            .send(Err(Refusal::NoLocalExecutor {
                candidate: "goal_soil".into(),
            }))
            .unwrap();
        session.tick();
        assert!(session.dispatched.is_none());
        assert!(session.active.is_none());
        assert!(session.view.active_goal.is_none());
        // The reason is reported the way `last_error` reports an end reason, but a refusal is
        // not one: `last_error` is what the harness classifies, and a run that ended on a
        // choice the engine merely could not carry out died as `provider_failed`.
        assert_eq!(
            session.view.last_refusal.as_deref(),
            Some(
                "Local guard rejected action 'goal_soil': no local executor in this build runs it (no skill, no target, and it is not a bounded wait/stop action)"
            )
        );
        assert!(
            session.view.last_error.is_none(),
            "a refusal is a technical answer, not the end of the run: {:?}",
            session.view.last_error
        );
        assert_eq!(session.view.refused_actions, 1);
        assert!(
            !session
                .view
                .events
                .iter()
                .any(|event| event.kind == "action")
        );
        let rejected = session
            .view
            .events
            .iter()
            .find(|event| event.kind == "rejected")
            .expect("the refusal is recorded");
        assert_eq!(
            rejected.message,
            "Local guard rejected action 'goal_soil': no local executor in this build runs it (no skill, no target, and it is not a bounded wait/stop action)",
            "the recording carries the concrete check, not a sentence that fits every failure"
        );
        assert!(
            commands.try_recv().is_err(),
            "a refusal stops nothing: the loop keeps the adapter it asks again through"
        );
    }

    /// A world with one observed soil block, so the catalogue offers `goal_soil` — the goal
    /// no executor in this build runs, and the one the live creative run chose.
    fn soil_observation() -> Observation {
        let mut observed = observation();
        let soil = Position {
            x: 2.5,
            y: 64.0,
            z: 0.5,
        };
        observed.trees = vec![crate::wood_tree::TreeView {
            id: 1,
            wood: "minecraft:dirt".into(),
            logs: vec![soil.clone()],
            incomplete: false,
            capacity_loss: false,
            status: "verified".into(),
            next: vec![crate::wood_tree::Access {
                target: soil,
                log: "minecraft:dirt".into(),
                route: vec![],
            }],
            diagnostics: crate::wood_tree::TreeDiagnostics::default(),
        }];
        observed
    }

    fn soil_settings() -> Settings {
        Settings {
            mode: Mode::Demo,
            wood_skills: true,
            allowed_dimension: "fixture:overworld".into(),
            ..Settings::default()
        }
    }

    /// The catalogue's own soil entry, exactly as the model receives it.
    fn soil_goal(observation: &Observation, settings: &Settings) -> Candidate {
        crate::catalog::goals(observation, settings)
            .into_iter()
            .find(|candidate| candidate.id == "goal_soil")
            .expect("an observed soil block offers the soil goal")
    }

    /// Answers the next dispatched action the way the adapter does when the local guard
    /// refuses it: `Err(reason)`, the concrete check the guard named.
    fn refuse(
        commands: &mut mpsc::UnboundedReceiver<AdapterCommand>,
        candidate: &Candidate,
    ) -> Refusal {
        let AdapterCommand::Execute(request) = commands.try_recv().expect("a dispatched action")
        else {
            panic!("expected a dispatched action");
        };
        let refusal = Refusal::NoLocalExecutor {
            candidate: candidate.id.clone(),
        };
        request
            .reply
            .send(Err(refusal.clone()))
            .expect("the engine still awaits this action");
        refusal
    }

    /// Dispatches one goal, gets it refused, and returns the session ready for the next
    /// cycle. Shared by the budget and continuation tests below.
    fn refuse_one_goal(
        session: &mut Session,
        commands: &mut mpsc::UnboundedReceiver<AdapterCommand>,
        start: &Observation,
    ) -> Refusal {
        // The peer this engine's other tests model too: it validates a dispatched action in
        // 30 ms. A test ticks on demand, so its acknowledgement is read in the same
        // millisecond it was sent and the measured acceptance budget would collapse to zero.
        // That budget is not what these tests judge; the request and time budgets are.
        session.client.record(30);
        let goal = soil_goal(start, &session.settings);
        session.apply(
            goal.clone(),
            "Jev selected goal; local executor",
            vec![goal.clone()],
            None,
            None,
        );
        let refusal = refuse(commands, &goal);
        session.tick();
        refusal
    }

    #[tokio::test]
    async fn failed_local_mine_is_not_reoffered_until_its_prerequisites_change() {
        let mut start = soil_observation();
        start.trees[0].wood = "minecraft:oak_log".into();
        start.trees[0].next[0].log = "minecraft:oak_log".into();
        start.trees[0].next[0].route = vec![start.position.clone()];
        start.crafting_inventory = Some(
            (9..=44)
                .map(|slot| crate::model::CraftingSlot {
                    slot,
                    item: "minecraft:air".into(),
                    count: 0,
                })
                .collect(),
        );
        let mut settings = soil_settings();
        settings.mode = Mode::Live;
        let (mut session, _commands, _feed) = fed_session(settings, start.clone());
        let gather = crate::wood::gather_candidates(&start, &session.settings)
            .into_iter()
            .next()
            .expect("observed target has a gather candidate");
        let mut failed = start.clone();
        failed.skill_outcome = Some(SkillOutcome {
            attempt_id: 7,
            candidate_id: gather.id.clone(),
            world_epoch: start.world_epoch,
            success: false,
            confirmed_by_server: false,
            message: "Gather break not started by the local client: target unchanged".into(),
        });
        session.view.observation = Some(failed);
        session.active = Some((gather.clone(), Instant::now()));
        session.skill_baseline = Some((start.world_epoch, start.items.clone(), 7));
        session.finish_skill();
        assert!(
            session.view.events.iter().any(|event| {
                event.kind == "skill" && event.message.contains("break not started")
            })
        );
        session.observed_at = Instant::now();
        session.refresh_manual_candidates();
        assert!(
            session
                .view
                .manual_candidates
                .iter()
                .all(|c| c.id != gather.id),
            "the same failed gather must not be a manual button"
        );
        assert!(
            session
                .view
                .manual_candidates
                .iter()
                .any(|c| c.id.starts_with("waypoint_"))
        );
        let mut current = session.view.observation.clone().expect("connected");
        let choices = session.executable_choices(&mut current, 2_000);
        assert!(choices.iter().all(|c| c.id != gather.id));
        session.command(Command::Manual {
            candidate: gather.clone(),
            world_epoch: start.world_epoch,
            dimension: start.dimension.clone(),
        });
        assert!(
            session.dispatched.is_none(),
            "manual bypass must not dispatch the blocked gather"
        );
        assert!(
            session.view.last_error.is_none(),
            "local refusal is not a run failure"
        );
        assert!(session.view.events.iter().any(|event| {
            event.kind == "rejected"
                && event.message.contains(&gather.id)
                && event.message.contains("break not started")
        }));
        assert!(choices.iter().any(|c| c.id.starts_with("waypoint_")));
        assert!(current.note.contains(&gather.id) && current.note.contains("break not started"));
        let mut changed = session.view.observation.clone().expect("connected");
        changed.held_item = Some("minecraft:iron_axe".into());
        let refreshed = session.executable_choices(&mut changed, 2_000);
        assert!(refreshed.iter().any(|c| c.id == gather.id));
    }

    #[tokio::test]
    async fn unsupported_attack_is_recorded_as_knowledge_not_offered_as_an_action() {
        let mut start = soil_observation();
        start.entities.push(Landmark {
            name: "Zombie".into(),
            position: Position {
                x: 6.0,
                y: 64.0,
                z: 0.5,
            },
        });
        let (mut session, _commands, _feed) = fed_session(soil_settings(), start.clone());
        let mut observed = start;
        let choices = session.executable_choices(&mut observed, 2_000);
        assert!(
            choices
                .iter()
                .all(|candidate| !candidate.id.starts_with("attack_"))
        );
        assert!(
            choices
                .iter()
                .any(|candidate| candidate.id.starts_with("waypoint_"))
        );
        assert!(observed.note.contains("attack_0") && observed.note.contains("no attack executor"));
    }

    #[tokio::test]
    async fn knowledge_only_goal_is_context_not_a_model_or_manual_action() {
        let start = soil_observation();
        let (mut session, _commands, _feed) = fed_session(soil_settings(), start);
        // The observed peer's measured interval keeps freshness a hang detector here;
        // candidate semantics do not depend on test-machine load.
        session.observation_interval.record(2_500);
        session.refresh_manual_candidates();
        assert!(
            session
                .view
                .manual_candidates
                .iter()
                .all(|candidate| !candidate.id.starts_with("goal_")),
            "knowledge-only goals must not become manual buttons"
        );
        assert!(
            session
                .view
                .manual_candidates
                .iter()
                .any(|c| c.id.starts_with("waypoint_"))
        );

        session.request();
        let request = session
            .view
            .events
            .iter()
            .rev()
            .find(|event| event.kind == "request")
            .expect("an executable waypoint still prompts Jev");
        assert!(
            request
                .candidates
                .iter()
                .all(|c| !c.id.starts_with("goal_"))
        );
        assert!(
            request
                .candidates
                .iter()
                .any(|c| c.id.starts_with("waypoint_"))
        );
        let note = &request
            .observation
            .as_ref()
            .expect("recorded Jev context")
            .note;
        assert!(
            note.contains("goal_soil") && note.contains("no executor"),
            "the missing executor must remain visible as knowledge: {note}"
        );
    }

    /// A historical `goal_soil` refusal is still recorded verbatim and does not end the
    /// run. After it, Jev sees actionable alternatives while the unsupported goal remains
    /// visible only as knowledge in the recorded observation.
    #[tokio::test]
    async fn a_refused_goal_is_recorded_verbatim_and_the_loop_asks_again() {
        let start = soil_observation();
        let (mut session, mut commands, _feed) = fed_session(soil_settings(), start.clone());
        session.continuous = true;
        session.next_request_at = Instant::now() + Duration::from_secs(60);
        let goal = soil_goal(&start, &session.settings);
        assert!(
            goal.description.contains("Knowledge only"),
            "the historical goal records why it cannot execute: {}",
            goal.description
        );
        session.apply(
            goal.clone(),
            "Jev selected goal; local executor",
            vec![goal.clone()],
            None,
            None,
        );
        let refusal = refuse(&mut commands, &goal);

        session.tick();

        assert!(
            session.view.last_error.is_none(),
            "a refusal is not a verdict on the run: {:?}",
            session.view.last_error
        );
        assert!(session.continuous, "the run mode survives a refusal");
        assert!(session.dispatched.is_none() && session.active.is_none());
        assert!(session.view.active_goal.is_none());
        assert_eq!(session.view.refused_actions, 1);
        assert_eq!(
            session.view.last_refusal.as_deref(),
            Some(refusal.to_string().as_str()),
            "the concrete check is reported where the end reason used to be"
        );
        assert!(
            !session
                .view
                .events
                .iter()
                .any(|event| event.kind == "action"),
            "a refused action is never recorded as executed"
        );
        let recorded = session
            .view
            .events
            .iter()
            .find(|event| event.kind == "rejected")
            .expect("the refusal is recorded");
        assert_eq!(
            recorded.message,
            refusal.to_string(),
            "the recording carries the concrete check verbatim"
        );
        assert_eq!(recorded.candidates.len(), 1);
        assert_eq!(recorded.candidates[0].id, "goal_soil");

        // The loop asks again with executable alternatives, not the unsupported goal.
        session.next_request_at = Instant::now() - Duration::from_millis(1);
        session.tick();
        assert_eq!(
            session.view.requests, 1,
            "the run asks again and counts the request it spends"
        );
        let request = session
            .view
            .events
            .iter()
            .rev()
            .find(|event| event.kind == "request")
            .expect("the loop asks again after the refusal");
        assert!(
            request
                .candidates
                .iter()
                .all(|candidate| candidate.id != "goal_soil")
        );
        assert!(
            request
                .candidates
                .iter()
                .any(|candidate| candidate.id.starts_with("waypoint_"))
        );
        let note = &request.observation.as_ref().expect("recorded context").note;
        assert!(note.contains("goal_soil") && note.contains("no executor"));
    }

    /// A refusal must not buy the run extra requests and must not end it by policy: the
    /// request that was answered with the refusal is the request the budget counted, and the
    /// session still stops by itself when that budget is used up.
    #[tokio::test]
    async fn the_run_still_stops_at_its_request_budget_after_a_refusal() {
        let start = soil_observation();
        let (mut session, mut commands, _feed) = fed_session(
            Settings {
                max_requests: 1,
                ..soil_settings()
            },
            start.clone(),
        );
        session.continuous = true;

        session.tick();
        assert_eq!(
            session.view.requests, 1,
            "the one permitted request is spent"
        );
        session.pending = None;
        let refusal = refuse_one_goal(&mut session, &mut commands, &start);
        assert!(
            session.view.last_error.is_none(),
            "the refusal itself is not the end: {refusal}"
        );
        assert!(session.continuous);

        session.next_request_at = Instant::now() - Duration::from_millis(1);
        session.tick();

        assert_eq!(
            session.view.last_error.as_deref(),
            Some("Request budget reached"),
            "the run stops on its own budget, not on the refusal"
        );
        assert_eq!(
            session.view.requests, 1,
            "a refusal neither resets nor refunds the request count"
        );
        assert_eq!(
            session
                .view
                .events
                .iter()
                .filter(|event| event.kind == "request")
                .count(),
            1,
            "the budget is not exceeded by an extra request"
        );
        assert!(!session.continuous, "a budget stop ends the run mode");
    }

    /// A refused goal cannot turn the loop into a spin: the pacing gate holds the loop back,
    /// every cycle spends exactly one counted request, and the run's own time budget still
    /// ends it. Nothing here is measured in wall-clock seconds a machine could change: the
    /// session's own start is what the time budget is measured from.
    #[tokio::test]
    async fn a_refusal_cannot_loop_forever_because_the_budgets_still_end_the_run() {
        let start = soil_observation();
        let (mut session, mut commands, _feed) = fed_session(
            Settings {
                request_interval_ms: 60_000,
                ..soil_settings()
            },
            start.clone(),
        );
        session.continuous = true;

        // The paced loop does not ask while its own interval is running.
        session.next_request_at = Instant::now() + Duration::from_secs(60);
        session.tick();
        assert_eq!(
            session.view.requests, 0,
            "the pacing gate holds the loop back instead of spinning"
        );
        assert!(session.pending.is_none());

        for expected in 1..=3u32 {
            session.next_request_at = Instant::now() - Duration::from_millis(1);
            session.tick();
            assert_eq!(
                session.view.requests, expected,
                "every cycle spends exactly one counted request"
            );
            session.pending = None;
            let refusal = refuse_one_goal(&mut session, &mut commands, &start);
            assert!(
                session.view.last_error.is_none(),
                "cycle {expected} does not end the run: {refusal}"
            );
        }
        assert_eq!(session.view.refused_actions, 3);
        assert_eq!(
            session
                .view
                .events
                .iter()
                .filter(|event| event.kind == "rejected")
                .count(),
            3,
            "every refused cycle is recorded with its own reason"
        );
        assert_eq!(session.view.requests, 3);

        // The session's own time budget still ends a run that keeps refusing goals.
        session.started = Instant::now() - Duration::from_secs(session.settings.max_seconds + 1);
        session.tick();
        assert_eq!(
            session.view.last_error.as_deref(),
            Some("Session time budget reached")
        );
        assert_eq!(
            session.view.requests, 3,
            "the end costs no further request and starts no new budget"
        );
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
            session.view.last_refusal.as_deref(),
            Some("Adapter action acceptance deadline expired"),
            "the concrete check is recorded; the run is not ended by it"
        );
        assert!(
            session.view.last_error.is_none(),
            "{:?}",
            session.view.last_error
        );
        assert!(
            !session
                .view
                .events
                .iter()
                .any(|event| event.kind == "action"),
            "an acceptance after the deadline is never recorded as an action"
        );
        assert!(
            commands.try_recv().is_err(),
            "nothing is stopped: the loop keeps the adapter it asks again through"
        );
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
    async fn acknowledgement_timeout_is_recorded_without_recording_an_action() {
        let (mut session, mut commands, request) = awaiting_adapter();
        session.dispatched.as_mut().unwrap().sent_at = Instant::now() - Duration::from_secs(2);
        session.tick();
        assert!(session.dispatched.is_none());
        assert!(session.active.is_none());
        assert!(
            session
                .view
                .last_refusal
                .as_deref()
                .unwrap()
                .contains("acknowledgement timed out"),
            "the concrete check is recorded; the run is not ended by it"
        );
        assert!(
            session.view.last_error.is_none(),
            "{:?}",
            session.view.last_error
        );
        assert!(
            !session
                .view
                .events
                .iter()
                .any(|event| event.kind == "action")
        );
        assert!(
            commands.try_recv().is_err(),
            "nothing is stopped: the loop keeps the adapter it asks again through"
        );
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
        assert!(
            candidates.iter().all(|candidate| candidate.id != "wait"),
            "technical observation pauses are internal, not Jev choices: {candidates:?}"
        );
        assert!(
            candidates
                .iter()
                .all(|candidate| !candidate.id.starts_with("attack_")),
            "attack without an executor is knowledge, not a selectable action: {candidates:?}"
        );
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

    fn activate_gather(session: &mut Session, attempt: u64) {
        let observed = session.view.observation.as_ref().unwrap();
        let candidate = Candidate {
            id: "gather_active".into(),
            description: "active native gather".into(),
            target: Some(Position {
                x: 2.5,
                y: 65.5,
                z: 0.5,
            }),
            duration_ms: 10_000,
            skill: Some(WoodSkill::Gather {
                log: "minecraft:oak_log".into(),
                approach: observed.position.clone(),
                route: vec![observed.position.clone()],
            }),
        };
        session.skill_baseline = Some((observed.world_epoch, observed.items.clone(), attempt));
        session.active = Some((candidate, Instant::now()));
    }

    #[tokio::test]
    async fn threat_preempts_gather_and_records_attempt_before_flight() {
        let (mut session, mut commands) = connected_session(
            Settings {
                mode: Mode::Live,
                safety_reflex: true,
                ..Settings::default()
            },
            threat_observation(),
        );
        session.continuous = true;
        activate_gather(&mut session, 401);
        session.reflex();
        assert!(session.active.is_none());
        assert!(matches!(commands.try_recv(), Ok(AdapterCommand::Stop)));
        assert!(matches!(
            commands.try_recv(),
            Ok(AdapterCommand::Execute(_))
        ));
        let cancelled = session
            .view
            .events
            .iter()
            .position(|e| {
                e.kind == "skill"
                    && e.message.contains("attempt=401")
                    && e.message.contains("cancelled")
            })
            .unwrap();
        let dispatched = session
            .view
            .events
            .iter()
            .position(|e| e.kind == "dispatched")
            .unwrap();
        assert!(cancelled < dispatched);
        assert_eq!(
            session.view.events[cancelled].candidates[0].id,
            "gather_active"
        );
        assert_eq!(session.view.requests, 0);
    }

    #[tokio::test]
    async fn pause_stop_and_world_failure_retain_one_correlated_skill_cancellation() {
        for operation in 0..3 {
            let (mut session, mut commands) = connected_session(Settings::default(), observation());
            activate_gather(&mut session, 402);
            match operation {
                0 => session.command(Command::Pause),
                1 => session.command(Command::Stop),
                _ => session.fail(WORLD_LIFECYCLE_CHANGED),
            }
            session.invalidate(false);
            assert!(session.active.is_none());
            assert!(session.skill_baseline.is_none());
            assert!(matches!(commands.try_recv(), Ok(AdapterCommand::Stop)));
            let cancelled: Vec<_> = session
                .view
                .events
                .iter()
                .filter(|e| e.kind == "skill")
                .collect();
            assert_eq!(cancelled.len(), 1);
            assert!(cancelled[0].message.contains("attempt=402"));
            assert!(cancelled[0].message.contains("world_epoch=1"));
            assert!(cancelled[0].message.contains("not server-confirmed"));
        }
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
        for rejected in [
            format!(
                "{}TypeSafe probabilities do not sum to one",
                crate::provider::REJECTED_ANSWER
            ),
            format!(
                "{}OpenRouter selected an unavailable action",
                crate::provider::OPENROUTER_REJECTED_ANSWER
            ),
        ] {
            let (mut session, _commands) = connected_session(
                Settings {
                    mode: Mode::Live,
                    ..Settings::default()
                },
                observation(),
            );
            session.continuous = true;
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
            skill: None,
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
        request.reply.send(Err(REJECTED_AFTER_HIT)).unwrap();
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
    fn routed_blocker(api_calls: u32) -> crate::routing::RoutedDecision {
        crate::routing::RoutedDecision {
            api_calls,
            route: None,
            stages: vec![],
            outcome: crate::routing::RouteOutcome::Blocked {
                reason: "test blocker".into(),
            },
            total_latency_ms: 2,
        }
    }

    #[test]
    fn route_refund_survives_generation_change_but_not_session_change() {
        let mut session = session();
        session.view.requests = 4;
        session.generation = 8;
        session
            .route_tx
            .send((session.id.clone(), 7, 10, 2, routed_blocker(1)))
            .unwrap();
        session
            .route_tx
            .send(("previous-session".into(), 7, 10, 2, routed_blocker(1)))
            .unwrap();
        session.record_routes();
        assert_eq!(session.view.requests, 3);
        assert!(session.active.is_none());
        assert_eq!(
            session
                .view
                .events
                .iter()
                .filter(|e| e.kind == "model_route")
                .count(),
            2
        );
        let metadata: serde_json::Value =
            serde_json::from_str(&session.view.events[0].message).unwrap();
        assert_eq!(metadata["current_generation"], false);
    }

    #[test]
    fn replay_is_not_mutated_by_late_route_completion() {
        let mut session = session();
        session.view.replay = true;
        session.view.requests = 9;
        session
            .route_tx
            .send((session.id.clone(), 0, 1, 2, routed_blocker(1)))
            .unwrap();
        session.record_routes();
        assert!(session.view.events.is_empty());
        assert_eq!(session.view.requests, 9);
    }

    #[tokio::test]
    async fn reflex_cancellation_preserves_usage_without_refunding_unknown_work() {
        let mut session = session();
        session.view.observation = Some(observation());
        session.view.requests = 2;
        session.continuous = true;
        session.pending = Some(pending_with(&session, Err("not consumed".into())));
        for status in ["started", "completed"] {
            session
                .stage_tx
                .send((
                    session.id.clone(),
                    session.generation,
                    1,
                    crate::routing::StageAudit {
                        requested_model: "jev-latest".into(),
                        served_model: None,
                        latency_ms: 12,
                        input_tokens: (status == "completed").then_some(80),
                        output_tokens: None,
                        status: status.into(),
                    },
                ))
                .unwrap();
        }
        session.clear_in_flight("nearby threat safety reflex");
        assert!(session.pending.is_none());
        assert!(session.continuous);
        assert_eq!(session.view.requests, 2);
        assert_eq!(
            session
                .view
                .events
                .iter()
                .filter(|e| e.kind == "model_route_cancelled")
                .count(),
            1
        );
        let recording = Recording {
            schema_version: 1,
            id: session.id.clone(),
            settings: session.settings.clone(),
            events: session.view.events.clone(),
        };
        assert_eq!(crate::recording::request_count(&recording), 1);
        session.clear_in_flight("second stop");
        assert_eq!(
            session
                .view
                .events
                .iter()
                .filter(|e| e.kind == "model_route_cancelled")
                .count(),
            1
        );
    }
}
