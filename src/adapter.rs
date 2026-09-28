use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static NEXT_ATTEMPT: AtomicU64 = AtomicU64::new(1);

use crate::model::{Candidate, Observation};
use crate::refusal::Refusal;
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};

/// An action is bound to the exact observed world lifecycle, including same-dimension respawns.
#[derive(Debug)]
pub struct ActionRequest {
    pub attempt_id: u64,
    pub candidate: Candidate,
    pub world_epoch: u64,
    pub dimension: Option<String>,
    pub accepted_before: Instant,
    pub reply: oneshot::Sender<Result<Instant, Refusal>>,
}

impl ActionRequest {
    pub fn with_deadline(mut self, deadline: Instant) -> Self {
        self.accepted_before = deadline;
        self
    }

    pub fn new(
        candidate: Candidate,
        observation: &Observation,
    ) -> (Self, oneshot::Receiver<Result<Instant, Refusal>>) {
        let (reply, receiver) = oneshot::channel();
        (
            Self {
                attempt_id: NEXT_ATTEMPT.fetch_add(1, Ordering::Relaxed),
                candidate,
                world_epoch: observation.world_epoch,
                dimension: observation.dimension.clone(),
                accepted_before: Instant::now() + Duration::from_secs(1),
                reply,
            },
            receiver,
        )
    }
}

/// Roll back queued execution if acceptance expires or its receiver cancels.
///
/// The returned value is the concrete reason the action did not run: the caller stores it in
/// the reply, records it, and can state it in its own observation note, so no refusal is
/// recorded as a sentence that fits every failure.
pub(crate) fn execute_and_acknowledge(
    reply: oneshot::Sender<Result<Instant, Refusal>>,
    accepted_before: Instant,
    execute: impl FnOnce() -> Result<(), Refusal>,
    cancel: impl FnOnce(),
) -> Result<Instant, Refusal> {
    if reply.is_closed() {
        return Err(Refusal::CancelledBeforeGuard);
    }
    if Instant::now() >= accepted_before {
        let refusal = Refusal::AcceptanceExpiredBeforeGuard;
        let _ = reply.send(Err(refusal.clone()));
        return Err(refusal);
    }
    if let Err(refusal) = execute() {
        let _ = reply.send(Err(refusal.clone()));
        return Err(refusal);
    }
    let accepted_at = Instant::now();
    if accepted_at >= accepted_before {
        cancel();
        let refusal = Refusal::AcceptanceExpiredDuringValidation;
        let _ = reply.send(Err(refusal.clone()));
        return Err(refusal);
    }
    if reply.send(Ok(accepted_at)).is_err() {
        cancel();
        return Err(Refusal::CancelledBeforeGuard);
    }
    Ok(accepted_at)
}

#[derive(Debug)]
pub enum AdapterCommand {
    Execute(Box<ActionRequest>),
    Stop,
    Disconnect,
}

pub struct AdapterHandle {
    pub commands: mpsc::UnboundedSender<AdapterCommand>,
    pub observations: watch::Receiver<Option<Observation>>,
    pub errors: watch::Receiver<Option<String>>,
    pub task: JoinHandle<()>,
}

impl Drop for AdapterHandle {
    fn drop(&mut self) {
        let _ = self.commands.send(AdapterCommand::Disconnect);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn deadline_expiring_during_execution_rolls_back_queued_action() {
        let (reply, mut receiver) = oneshot::channel();
        let queued = Cell::new(false);
        let outcome = execute_and_acknowledge(
            reply,
            Instant::now() + Duration::from_millis(5),
            || {
                queued.set(true);
                std::thread::sleep(Duration::from_millis(10));
                Ok(())
            },
            || queued.set(false),
        );
        assert_eq!(outcome, Err(Refusal::AcceptanceExpiredDuringValidation));
        assert!(!queued.get());
        assert_eq!(
            receiver.try_recv().unwrap(),
            Err(Refusal::AcceptanceExpiredDuringValidation),
            "the caller sees the same reason that rolled the action back"
        );
    }

    #[test]
    fn cancellation_during_execution_rolls_back_before_acceptance() {
        let (reply, receiver) = oneshot::channel();
        let queued = Cell::new(false);
        let rolled_back = Cell::new(false);
        assert!(!reply.is_closed());
        let outcome = execute_and_acknowledge(
            reply,
            Instant::now() + Duration::from_secs(1),
            || {
                queued.set(true);
                // Cancellation happens after preflight, while execution is queued.
                drop(receiver);
                Ok(())
            },
            || {
                queued.set(false);
                rolled_back.set(true);
            },
        );
        assert_eq!(outcome, Err(Refusal::CancelledBeforeGuard));
        assert!(!queued.get());
        assert!(rolled_back.get());
    }

    #[test]
    fn a_cancelled_caller_and_an_expired_deadline_are_different_reasons() {
        let (reply, receiver) = oneshot::channel();
        // The caller dropped its receiver: a cancellation, not an expiry.
        drop(receiver);
        let cancelled = execute_and_acknowledge(
            reply,
            Instant::now() + Duration::from_secs(1),
            || panic!("a cancelled action is never executed"),
            || panic!("nothing was queued, so nothing is rolled back"),
        );
        assert_eq!(cancelled, Err(Refusal::CancelledBeforeGuard));

        let (reply, mut receiver) = oneshot::channel();
        let expired = execute_and_acknowledge(
            reply,
            Instant::now() - Duration::from_millis(1),
            || panic!("an expired action is never executed"),
            || panic!("nothing was queued, so nothing is rolled back"),
        );
        assert_eq!(expired, Err(Refusal::AcceptanceExpiredBeforeGuard));
        assert_eq!(
            receiver.try_recv().unwrap(),
            Err(Refusal::AcceptanceExpiredBeforeGuard)
        );
    }

    #[test]
    fn a_refused_execution_reports_the_guard_reason_to_the_caller() {
        let (reply, mut receiver) = oneshot::channel();
        let refusal = Refusal::NoLocalExecutor {
            candidate: "goal_soil".into(),
        };
        let outcome = execute_and_acknowledge(
            reply,
            Instant::now() + Duration::from_secs(1),
            || Err(refusal.clone()),
            || panic!("a refused execution queues nothing to roll back"),
        );
        assert_eq!(outcome, Err(refusal.clone()));
        assert_eq!(
            receiver.try_recv().unwrap(),
            Err(refusal),
            "the reply must carry the concrete check, not a generic sentence"
        );
    }
}
