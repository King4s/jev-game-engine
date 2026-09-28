use std::{collections::VecDeque, time::Duration};

/// Rolling request latency, not network ping. Limits remain local and bounded.
#[derive(Clone, Debug, Default)]
pub struct LatencyPolicy {
    samples: VecDeque<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    pub p50_ms: Option<u64>,
    pub p95_ms: Option<u64>,
    pub goal_ms: u64,
    pub max_answer_age_ms: u64,
}

impl LatencyPolicy {
    pub fn record(&mut self, elapsed_ms: u64) {
        if self.samples.len() == 64 {
            self.samples.pop_front();
        }
        self.samples.push_back(elapsed_ms);
    }

    pub fn timing(&self) -> Timing {
        let mut sorted: Vec<_> = self.samples.iter().copied().collect();
        sorted.sort_unstable();
        let percentile = |percent: usize| {
            if sorted.is_empty() {
                None
            } else {
                Some(sorted[(sorted.len() * percent).div_ceil(100) - 1])
            }
        };
        let p95 = percentile(95);
        let estimate = p95.unwrap_or(500);
        Timing {
            p50_ms: percentile(50),
            p95_ms: p95,
            goal_ms: estimate.saturating_mul(4).clamp(2_000, 10_000),
            // A bounded warmup permits measuring a slow first request. Later
            // requests use measured latency; observation freshness is separate.
            max_answer_age_ms: p95
                .map_or(5_000, |value| value.saturating_mul(2).clamp(1_500, 5_000)),
        }
    }

    /// How long a peer may take to answer a bounded action, derived from the measured
    /// acknowledgements themselves instead of a constant tuned to one user's connection.
    /// Connection quality and model distance differ per user, so a fixed second would make
    /// the engine behave differently for each of them. Until a measurement exists the
    /// one-second floor applies, which is the previous behaviour; measured slow peers get
    /// four times their p95, bounded to ten seconds so a stalled peer still fails.
    pub fn action_budget(&self) -> Duration {
        self.timing()
            .p95_ms
            .map_or(Duration::from_secs(1), |value| {
                // Four times the measured p95, bounded above only. A fixed floor here would let a
                // fast peer's budget be set by a constant instead of by what it actually answers,
                // which is exactly how a measured ~150 ms peer ended up waiting a full second.
                Duration::from_millis(value.saturating_mul(4).min(10_000))
            })
    }

    /// The measured p95 of this policy, for reporting what a run actually observed.
    pub fn p95_ms(&self) -> Option<u64> {
        self.timing().p95_ms
    }
}

/// Generation changes invalidate responses independently of latency policy.
pub fn answer_is_current(
    request_generation: u64,
    session_generation: u64,
    elapsed_ms: u64,
    timing: Timing,
) -> bool {
    request_generation == session_generation && elapsed_ms <= timing.max_answer_age_ms
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_budget_follows_measurements_instead_of_a_fixed_second() {
        assert_eq!(
            LatencyPolicy::default().action_budget(),
            Duration::from_secs(1),
            "without a measurement the previous one-second floor applies"
        );
        let mut measured = LatencyPolicy::default();
        measured.record(1_200);
        assert_eq!(
            measured.action_budget(),
            Duration::from_millis(4_800),
            "a measured slow peer gets four times its p95"
        );
        let mut live = LatencyPolicy::default();
        live.record(152);
        assert_eq!(
            live.action_budget(),
            Duration::from_millis(608),
            "a measured 152 ms peer gets 608 ms, not a padded second"
        );
        measured.record(30_000);
        assert_eq!(
            measured.action_budget(),
            Duration::from_secs(10),
            "a stalled peer must still fail inside the bounded ceiling"
        );
    }

    #[test]
    fn warmup_accepts_slow_initial_answer_but_keeps_age_and_generation_bounds() {
        let mut policy = LatencyPolicy::default();
        let initial = policy.timing();
        assert!(answer_is_current(1, 1, 2_000, initial));
        assert!(!answer_is_current(1, 1, 5_001, initial));
        assert!(!answer_is_current(1, 2, 300, initial));
        policy.record(2_000);
        assert_eq!(policy.timing().max_answer_age_ms, 4_000);
        assert_eq!(policy.timing().goal_ms, 8_000);
    }

    #[test]
    fn measured_policy_tightens_after_fast_warmup_without_extending_old_request() {
        let mut policy = LatencyPolicy::default();
        policy.record(300);
        let fast_request = policy.timing();
        assert_eq!(fast_request.max_answer_age_ms, 1_500);
        policy.record(4_000);
        assert!(!answer_is_current(7, 7, 2_000, fast_request));
        assert_eq!(policy.timing().max_answer_age_ms, 5_000);
    }
}
