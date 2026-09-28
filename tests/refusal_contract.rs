//! Every local refusal names the check that failed.
//!
//! A live run ended because a local guard refused the goal the model chose and recorded only
//! "Local guard rejected invalid, expired or unsafe action": one sentence that fits an expired
//! deadline, a dead bot, a distant target, a skill nothing prepared and a candidate no executor
//! in this build runs. Nobody could tell which check refused it. These tests pin the concrete
//! reason for each check, including the goal-kind candidate that has no executor at all.
use jev_game_engine::model::{Candidate, Position, WoodSkill};
use jev_game_engine::refusal::{self, LocalExecutor, MAX_ACTION_MS, MAX_MOVE_M, Refusal};

/// A goal-kind candidate exactly as the catalogue offers it: no skill, no target, and a
/// duration from the survival ladder (`GoalKind::duration_ms`).
fn goal_candidate(id: &str, duration_ms: u64) -> Candidate {
    Candidate {
        skill: None,
        id: id.into(),
        description: format!("Goal {id} (bounded catalogue, ladder order)"),
        target: None,
        duration_ms,
    }
}

/// A bounded move to an observed waypoint, the shape `wait` and `waypoint_*` share.
fn move_candidate(duration_ms: u64) -> Candidate {
    Candidate {
        skill: None,
        id: "waypoint_42".into(),
        description: "Navigate toward observed location".into(),
        target: Some(Position {
            x: 6.0,
            y: 63.0,
            z: 7.5,
        }),
        duration_ms,
    }
}

#[test]
fn a_goal_kind_candidate_has_no_local_executor_and_is_refused_by_name() {
    let candidate = goal_candidate("goal_soil", 60_000);
    assert_eq!(
        candidate.local_executor(),
        LocalExecutor::Missing,
        "no executor in this build turns a goal into steps"
    );
    let refusal = refusal::admit(&candidate, Some(20.0), None)
        .expect_err("the guard must refuse a candidate it cannot run");
    assert_eq!(
        refusal,
        Refusal::NoLocalExecutor {
            candidate: "goal_soil".into()
        }
    );
    assert_eq!(
        refusal.to_string(),
        "Local guard rejected action 'goal_soil': no local executor in this build runs it (no skill, no target, and it is not a bounded wait/stop action)",
        "the recording gets this sentence, and it names the check"
    );
}

#[test]
fn a_missing_executor_is_the_reason_even_when_the_duration_also_would_not_fit() {
    // A goal's ladder duration (60 s) is outside the bounded executor's 1..=10 s range as
    // well, and the recorded sentence could not tell the two apart. The structural check is
    // reported first, because a candidate with no executor has no bounds to violate.
    let refusal = refusal::admit(&goal_candidate("goal_planks", 10_000), Some(20.0), None)
        .expect_err("no executor runs it whatever its duration");
    assert_eq!(
        refusal,
        Refusal::NoLocalExecutor {
            candidate: "goal_planks".into()
        },
        "the duration being inside the bound must not turn the refusal into a duration one"
    );
}

#[test]
fn every_failed_check_reports_itself_instead_of_a_sentence_that_fits_all_of_them() {
    // The bounded executor's own duration bound, named with the value that failed it.
    let refusal = refusal::admit(&move_candidate(MAX_ACTION_MS + 1), Some(20.0), Some(3.0))
        .expect_err("a duration outside the bound is refused");
    assert!(matches!(
        refusal,
        Refusal::DurationOutOfBounds { duration_ms } if duration_ms == MAX_ACTION_MS + 1
    ));
    assert!(
        refusal
            .to_string()
            .contains(&(MAX_ACTION_MS + 1).to_string()),
        "the refusal states the duration it refused: {refusal}"
    );

    // A target beyond the mover's verified range, named with the measured distance.
    let refusal = refusal::admit(&move_candidate(2_000), Some(20.0), Some(MAX_MOVE_M + 0.5))
        .expect_err("a target outside the verified range is refused");
    assert!(matches!(
        refusal,
        Refusal::TargetOutOfRange { distance_m } if distance_m > MAX_MOVE_M
    ));
    assert!(
        refusal.to_string().contains("12.5 m"),
        "the refusal states the measured distance: {refusal}"
    );

    // A target that is not a finite world position.
    let mut broken = move_candidate(2_000);
    broken.target = Some(Position {
        x: f64::NAN,
        y: 63.0,
        z: 7.5,
    });
    assert_eq!(
        refusal::admit(&broken, Some(20.0), Some(3.0)),
        Err(Refusal::TargetNotFinite)
    );
}

#[test]
fn the_body_and_the_world_reads_are_refusal_reasons_of_their_own() {
    let wait = Candidate {
        skill: None,
        id: "wait".into(),
        description: "Stay where you are and keep observing".into(),
        target: None,
        duration_ms: 2_000,
    };
    assert_eq!(
        refusal::admit(&wait, None, None),
        Err(Refusal::BodyUnreadable),
        "a body the client could not report is not an empty hand: it is unreadable"
    );
    assert_eq!(
        refusal::admit(&wait, Some(0.0), None),
        Err(Refusal::Dead),
        "a dead bot executes nothing"
    );
    assert_eq!(
        refusal::admit(&move_candidate(2_000), Some(20.0), None),
        Err(Refusal::PositionUnreadable),
        "without the bot's own position the distance to the target cannot be verified"
    );
}

#[test]
fn what_the_guard_can_run_is_admitted_with_the_executor_that_runs_it() {
    let wait = Candidate {
        skill: None,
        id: "wait".into(),
        description: "Stay where you are and keep observing".into(),
        target: None,
        duration_ms: 2_000,
    };
    assert_eq!(
        refusal::admit(&wait, Some(20.0), None),
        Ok(LocalExecutor::StayPut)
    );
    assert_eq!(
        refusal::admit(&move_candidate(2_000), Some(20.0), Some(3.0)),
        Ok(LocalExecutor::Move)
    );
    let gather = Candidate {
        skill: Some(WoodSkill::Gather {
            log: "minecraft:oak_log".into(),
            approach: Position {
                x: 1.5,
                y: 63.0,
                z: 7.5,
            },
            route: Vec::new(),
        }),
        id: "gather_8_63_7".into(),
        description: "Harvest an observed oak log".into(),
        target: Some(Position {
            x: 8.5,
            y: 63.0,
            z: 7.5,
        }),
        duration_ms: 6_000,
    };
    assert_eq!(
        gather.local_executor(),
        LocalExecutor::Skill,
        "a skill is run by its own prepared executor"
    );
    assert_eq!(
        refusal::admit(&gather, Some(20.0), Some(1.0)),
        Err(Refusal::SkillNotPrepared {
            candidate: "gather_8_63_7".into()
        }),
        "the bounded executor never silently walks a skill it did not prepare"
    );
}

#[test]
fn the_mover_keeps_its_own_reason_instead_of_discarding_it() {
    assert_eq!(
        Refusal::MoveRefused {
            reason: "no verified route to the target".into()
        }
        .to_string(),
        "Local guard rejected action: local mover refused the route: no verified route to the target",
    );
}
