use std::collections::VecDeque;

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
