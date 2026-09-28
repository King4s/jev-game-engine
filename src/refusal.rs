//! Why an action did not run, and which executor would have run it.
//!
//! A live run ended because a local guard refused the goal the model had chosen and recorded
//! one sentence — "Local guard rejected invalid, expired or unsafe action" — that fits an
//! expired acceptance deadline, a dead bot, a target beyond the mover's range, a skill nothing
//! prepared and a candidate no executor in this build runs. The reason existed inside the
//! guard and was thrown away, so the recording could not explain the run.
//!
//! Every check below is therefore its own reason. [`admit`] is the whole local decision over a
//! candidate and the facts a client reported, as a pure function: it is what the guard asks
//! before it touches the world, the engine stores its answer in the reply to the caller and in
//! the recording event verbatim, and a test can pin every reason without a Minecraft
//! connection. The engine never widens this list by policy: it refuses only what it can verify
//! is not executable and states which verification failed.
use crate::model::Candidate;

/// The shortest bounded action this build will run. `wait` is offered at 2 s.
pub const MIN_ACTION_MS: u64 = 1;
/// The longest bounded action this build will run.
///
/// A bound, not a measurement: an action must end on its own so the loop can re-observe, and
/// the survival-ladder goals run far longer than this between two model decisions (see
/// `catalog::GoalKind::duration_ms`), which is one of the facts that makes them knowledge only
/// until the engine has a goal loop that resolves them into bounded steps.
pub const MAX_ACTION_MS: u64 = 10_000;
/// The farthest a bounded move may target, matching the mover's verified range.
pub const MAX_MOVE_M: f64 = 12.0;

/// Which local executor runs a candidate, decided from the candidate alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalExecutor {
    /// A bounded stay-put action (`wait`, `stop`): nothing moves, the duration is the action.
    StayPut,
    /// A bounded move to an observed target inside the mover's verified range.
    Move,
    /// A skill with its own prepare/advance executor (harvest, craft, place).
    Skill,
    /// No executor in this build runs it: no skill, no target, and not a bounded stay-put
    /// action. A goal-kind candidate is this until a goal loop resolves it into steps.
    Missing,
}

impl Candidate {
    /// Which local executor this candidate needs, or [`LocalExecutor::Missing`] when nothing
    /// in this build runs it. The catalogue asks the same question of every goal entry and
    /// must not offer work whose answer is `Missing` as if it were work.
    pub fn local_executor(&self) -> LocalExecutor {
        if self.skill.is_some() {
            LocalExecutor::Skill
        } else if self.target.is_some() {
            LocalExecutor::Move
        } else if matches!(self.id.as_str(), "wait" | "stop") {
            LocalExecutor::StayPut
        } else {
            LocalExecutor::Missing
        }
    }
}

/// Why an action was not run.
///
/// One variant per check, so a refusal names the check that failed; the display text is what
/// the caller receives and what the recording shows, and it is written to be enough on its own
/// (which candidate, which check, which value).
#[derive(Clone, Debug, PartialEq)]
pub enum Refusal {
    // The action's own acceptance and lifecycle, checked before any world read.
    /// The acceptance deadline had passed before the local guard ran.
    AcceptanceExpiredBeforeGuard,
    /// The acceptance deadline passed before the action could be executed.
    AcceptanceExpiredBeforeExecution,
    /// The caller cancelled the action before the local guard ran.
    CancelledBeforeGuard,
    /// The acceptance deadline passed while the action was being validated locally.
    AcceptanceExpiredDuringValidation,
    /// The adapter is still logging in, so no world is observable.
    StillConnecting,
    /// No spawned world has been observed yet: there is nothing to execute against.
    WorldNotSpawned,
    /// The world lifecycle the action was bound to has ended (dimension change, respawn).
    WorldLifecycleChanged { action_epoch: u64, world_epoch: u64 },
    /// The world lifecycle changed while the action was still queued for acceptance.
    LifecycleChangedBeforeAcceptance,
    /// The bot is in another world than the one the action was observed in.
    WorldIdentityMismatch {
        expected: Option<String>,
        observed: Option<String>,
    },
    /// Another command replaced this action before it could be validated.
    SupersededByNewerCommand,
    /// A login packet interrupted the pending action.
    LoginInterrupted,
    /// The operator's local stop cancelled the action.
    OperatorStop,
    /// Local game ticks went stale, so no ECS update could carry the action out.
    StaleGameTicks,
    /// No observation was available to bind the action to.
    ObservationUnavailable,
    /// The bot died before the action was accepted.
    DeathBeforeAcceptance,
    /// The bot's health fell before the action was accepted.
    HealthDecreasedBeforeAcceptance,

    // The local guard's own checks over the candidate.
    /// The client could not report the bot's health, so the body is unknown.
    BodyUnreadable,
    /// The bot is dead: nothing is executed.
    Dead,
    /// No executor in this build runs the candidate kind.
    NoLocalExecutor { candidate: String },
    /// A skill candidate reached the bounded executor with no prepared skill.
    SkillNotPrepared { candidate: String },
    /// The candidate's duration is outside the bounded range.
    DurationOutOfBounds { duration_ms: u64 },
    /// The candidate's target is not a finite world position.
    TargetNotFinite,
    /// The bot's own position could not be read, so the distance is unknown.
    PositionUnreadable,
    /// The target is farther than the bounded mover's verified range.
    TargetOutOfRange { distance_m: f64 },
    /// The mover refused the route, and this keeps its own reason.
    MoveRefused { reason: String },

    // The skill executors' and the offline fixture's own guards.
    /// The wood harvester's prepare refused the skill.
    WoodSkillUnprepared,
    /// The crafting executor's prepare refused the skill.
    CraftingUnprepared,
    /// The placement executor's prepare refused the skill.
    PlacementUnprepared,

    // The engine's own bound checks over the adapter's acknowledgement.
    /// The adapter accepted the action after its acceptance deadline.
    AdapterAcceptanceDeadlineExpired,
    /// The adapter returned an acceptance timestamp that cannot be true.
    InvalidAdapterTimestamp,
    /// The adapter ended before it accepted the action.
    AdapterEndedBeforeAccepting,
    /// The adapter never answered within the measured action budget.
    AdapterAcknowledgementTimedOut,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::AcceptanceExpiredBeforeGuard => {
                f.write_str("Action acceptance expired before the local guard ran")
            }
            Refusal::AcceptanceExpiredBeforeExecution => f.write_str(
                "Local guard rejected action: the acceptance deadline passed before it could be executed",
            ),
            Refusal::CancelledBeforeGuard => {
                f.write_str("Action was cancelled before the local guard ran")
            }
            Refusal::AcceptanceExpiredDuringValidation => {
                f.write_str("Action acceptance expired during local validation")
            }
            Refusal::StillConnecting => f.write_str("Minecraft is still connecting"),
            Refusal::WorldNotSpawned => {
                f.write_str("Local guard rejected action: no spawned world observation yet")
            }
            Refusal::WorldLifecycleChanged {
                action_epoch,
                world_epoch,
            } => write!(
                f,
                "Local guard rejected action: it belonged to world lifecycle {action_epoch}, this world is lifecycle {world_epoch}"
            ),
            Refusal::WorldIdentityMismatch { expected, observed } => write!(
                f,
                "Local guard rejected action: it was observed in world {}, the bot is in world {}",
                expected.as_deref().unwrap_or("unknown"),
                observed.as_deref().unwrap_or("unknown"),
            ),
            Refusal::LifecycleChangedBeforeAcceptance => {
                f.write_str("World lifecycle changed before action acceptance")
            }
            Refusal::SupersededByNewerCommand => {
                f.write_str("Action superseded by a newer command")
            }
            Refusal::LoginInterrupted => f.write_str("Login interrupted pending action"),
            Refusal::OperatorStop => f.write_str("Action cancelled by local stop"),
            Refusal::StaleGameTicks => {
                f.write_str("Action rejected because local game ticks became stale")
            }
            Refusal::ObservationUnavailable => {
                f.write_str("World observation unavailable before action acceptance")
            }
            Refusal::DeathBeforeAcceptance => f.write_str("Bot died before action acceptance"),
            Refusal::HealthDecreasedBeforeAcceptance => {
                f.write_str("Health decreased before action acceptance")
            }
            Refusal::BodyUnreadable => {
                f.write_str("Local guard rejected action: the client could not report the bot's health")
            }
            Refusal::Dead => f.write_str("Local guard rejected action: the bot is dead"),
            Refusal::NoLocalExecutor { candidate } => write!(
                f,
                "Local guard rejected action '{candidate}': no local executor in this build runs it (no skill, no target, and it is not a bounded wait/stop action)"
            ),
            Refusal::SkillNotPrepared { candidate } => write!(
                f,
                "Local guard rejected action '{candidate}': the bounded executor prepared no skill for it"
            ),
            Refusal::DurationOutOfBounds { duration_ms } => write!(
                f,
                "Local guard rejected action: duration {duration_ms} ms is outside the bounded {MIN_ACTION_MS}..={MAX_ACTION_MS} ms range"
            ),
            Refusal::TargetNotFinite => {
                f.write_str("Local guard rejected action: the target is not a finite world position")
            }
            Refusal::PositionUnreadable => f.write_str(
                "Local guard rejected action: the bot's own position could not be read, so the target distance is unverified",
            ),
            Refusal::TargetOutOfRange { distance_m } => write!(
                f,
                "Local guard rejected action: the target is {distance_m:.1} m away, beyond the mover's verified {MAX_MOVE_M:.1} m range"
            ),
            Refusal::MoveRefused { reason } => {
                write!(f, "Local guard rejected action: local mover refused the route: {reason}")
            }
            Refusal::WoodSkillUnprepared => {
                f.write_str("Local wood guard rejected stale, disabled or unsafe skill")
            }
            Refusal::CraftingUnprepared => {
                f.write_str("Local crafting guard rejected unknown, busy or changed inventory")
            }
            Refusal::PlacementUnprepared => {
                f.write_str("Local placement guard rejected changed site or inventory")
            }
            Refusal::AdapterAcceptanceDeadlineExpired => {
                f.write_str("Adapter action acceptance deadline expired")
            }
            Refusal::InvalidAdapterTimestamp => {
                f.write_str("Adapter returned an invalid acceptance timestamp")
            }
            Refusal::AdapterEndedBeforeAccepting => {
                f.write_str("Adapter ended before accepting the action")
            }
            Refusal::AdapterAcknowledgementTimedOut => {
                f.write_str("Adapter action acknowledgement timed out")
            }
        }
    }
}

/// The local guard's decision for one candidate: the executor that will run it, or the
/// concrete check that refused it.
///
/// `health` is what the client reported (`None` when the body could not be read at all) and
/// `target_distance_m` is the measured distance to the candidate's target (`None` when the
/// bot's position could not be read). The order is the order the guard applies, and it decides
/// which reason is reported when several checks would fail: what the bot is comes first, then
/// whether any executor in this build runs the candidate at all, then the bounds of that
/// executor. A candidate with no executor has no bounds to violate, so a goal whose duration
/// also exceeds the bound is refused for the executor it lacks, not for its duration.
pub fn admit(
    candidate: &Candidate,
    health: Option<f32>,
    target_distance_m: Option<f64>,
) -> Result<LocalExecutor, Refusal> {
    let Some(health) = health.filter(|health| health.is_finite()) else {
        return Err(Refusal::BodyUnreadable);
    };
    if health <= 0.0 {
        return Err(Refusal::Dead);
    }
    let executor = match candidate.local_executor() {
        LocalExecutor::Missing => {
            return Err(Refusal::NoLocalExecutor {
                candidate: candidate.id.clone(),
            });
        }
        LocalExecutor::Skill => {
            return Err(Refusal::SkillNotPrepared {
                candidate: candidate.id.clone(),
            });
        }
        executor => executor,
    };
    if !(MIN_ACTION_MS..=MAX_ACTION_MS).contains(&candidate.duration_ms) {
        return Err(Refusal::DurationOutOfBounds {
            duration_ms: candidate.duration_ms,
        });
    }
    if executor == LocalExecutor::Move {
        let Some(target) = &candidate.target else {
            return Err(Refusal::NoLocalExecutor {
                candidate: candidate.id.clone(),
            });
        };
        if ![target.x, target.y, target.z]
            .iter()
            .all(|value| value.is_finite() && value.abs() < 30_000_000.0)
        {
            return Err(Refusal::TargetNotFinite);
        }
        let Some(distance_m) = target_distance_m.filter(|distance| distance.is_finite()) else {
            return Err(Refusal::PositionUnreadable);
        };
        if distance_m > MAX_MOVE_M {
            return Err(Refusal::TargetOutOfRange { distance_m });
        }
    }
    Ok(executor)
}
