use std::time::{Duration, Instant};

use crate::model::{Candidate, Observation};
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};

/// An action is bound to the exact observed world lifecycle, including same-dimension respawns.
#[derive(Debug)]
pub struct ActionRequest {
    pub candidate: Candidate,
    pub world_epoch: u64,
    pub dimension: Option<String>,
    pub accepted_before: Instant,
    pub reply: oneshot::Sender<Result<Instant, &'static str>>,
}

impl ActionRequest {
    pub fn with_deadline(mut self, deadline: Instant) -> Self {
        self.accepted_before = deadline;
        self
    }

    pub fn new(
        candidate: Candidate,
        observation: &Observation,
    ) -> (Self, oneshot::Receiver<Result<Instant, &'static str>>) {
        let (reply, receiver) = oneshot::channel();
        (
            Self {
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
pub(crate) fn execute_and_acknowledge(
    reply: oneshot::Sender<Result<Instant, &'static str>>,
    accepted_before: Instant,
    execute: impl FnOnce() -> Result<(), &'static str>,
    cancel: impl FnOnce(),
) -> bool {
    if Instant::now() >= accepted_before || reply.is_closed() {
        let _ = reply.send(Err("Action acceptance expired or was cancelled"));
        return false;
    }
    if let Err(reason) = execute() {
        let _ = reply.send(Err(reason));
        return false;
    }
    let accepted_at = Instant::now();
    if accepted_at >= accepted_before {
        cancel();
        let _ = reply.send(Err("Action acceptance expired during validation"));
        return false;
    }
    if reply.send(Ok(accepted_at)).is_err() {
        cancel();
        return false;
    }
    true
}

#[derive(Debug)]
pub enum AdapterCommand {
    Execute(ActionRequest),
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
        let accepted = execute_and_acknowledge(
            reply,
            Instant::now() + Duration::from_millis(5),
            || {
                queued.set(true);
                std::thread::sleep(Duration::from_millis(10));
                Ok(())
            },
            || queued.set(false),
        );
        assert!(!accepted);
        assert!(!queued.get());
        assert!(receiver.try_recv().unwrap().is_err());
    }

    #[test]
    fn cancellation_during_execution_rolls_back_before_acceptance() {
        let (reply, receiver) = oneshot::channel();
        let queued = Cell::new(false);
        let rolled_back = Cell::new(false);
        assert!(!reply.is_closed());
        let accepted = execute_and_acknowledge(
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
        assert!(!accepted);
        assert!(!queued.get());
        assert!(rolled_back.get());
    }
}
