//! Who chose the action a recorded event describes.
//!
//! The distinction is derived only from fields the event already carries, so a recording stays
//! readable without guessing: an operator takeover announces itself in the recorded message, an
//! `offline-fixture` model identity means the offline fixture answered, a `SAFETY-REFLEX`
//! message means the engine's opt-in safety reflex dispatched a bounded flight goal without a
//! model request, and any other model decision on a decision, dispatch, action or executor
//! event means the configured model selected the goal. Events that carry none of these record
//! no origin.
//!
//! This lives in the library rather than in the desktop UI so the timeline and the offline
//! recording report label the same events the same way, and so a test can assert the separation
//! instead of trusting a screenshot of one case.

use crate::model::Event;

/// Who chose the action a recorded event describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionOrigin {
    /// A goal the configured model selected.
    JevSelected,
    /// A goal the offline fixture's synthetic decision path selected.
    Fixture,
    /// An action the operator took over manually.
    Manual,
    /// A bounded flight goal the engine's opt-in safety reflex dispatched itself, with no
    /// model request. It is the only origin where the goal did not come from the model or
    /// the operator, so it must stay distinguishable from both.
    SafetyReflex,
}

impl ActionOrigin {
    /// The label the timeline and the offline report show for this origin.
    pub fn label(self) -> &'static str {
        match self {
            Self::JevSelected => "JEV-SELECTED",
            Self::Fixture => "FIXTURE",
            Self::Manual => "MANUAL",
            Self::SafetyReflex => "SAFETY-REFLEX",
        }
    }
}

/// Origin of the recorded action, derived only from fields the event carries.
/// A `Manual action` message means a human takeover, a `SAFETY-REFLEX` message
/// means the engine's safety reflex dispatched the action itself (the engine
/// writes that origin into the dispatch and acceptance messages, and the
/// `reflex` event that precedes them), an `offline-fixture` model means the
/// offline fixture, and any other model decision on a decision, dispatch,
/// action or executor event means the model selected it. Events that carry
/// none of these record no origin.
pub fn event_origin(event: &Event) -> Option<ActionOrigin> {
    if event.message.contains("Manual action") {
        return Some(ActionOrigin::Manual);
    }
    if event.kind == "reflex" || event.message.contains("SAFETY-REFLEX") {
        return Some(ActionOrigin::SafetyReflex);
    }
    let decision = event.decision.as_ref()?;
    if decision.model == "offline-fixture" {
        return Some(ActionOrigin::Fixture);
    }
    match event.kind.as_str() {
        "decision" | "dispatched" | "action" | "executor" => Some(ActionOrigin::JevSelected),
        _ => None,
    }
}
