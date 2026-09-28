use jev_game_engine::{
    model::{Candidate, Observation},
    routing::{self, RouteOutcome, RoutingRequest, RoutingTransport, StageAudit, TransportFuture},
};
use serde_json::{Value, json};
use std::{collections::VecDeque, time::Duration};

enum Reply {
    Json(Value),
    Fail,
    FailWith(String),
    Pending,
    Delay(Duration, Value),
}
#[derive(Default)]
struct Script {
    replies: VecDeque<Reply>,
    calls: Vec<(&'static str, String, Value, Duration)>,
}
impl Script {
    fn new(replies: Vec<Reply>) -> Self {
        Self {
            replies: replies.into(),
            ..Self::default()
        }
    }
}
impl RoutingTransport for Script {
    fn post<'a>(
        &'a mut self,
        endpoint: &'static str,
        key: &'a str,
        body: Value,
        timeout: Duration,
    ) -> TransportFuture<'a> {
        self.calls.push((endpoint, key.into(), body, timeout));
        let reply = self.replies.pop_front().expect("unexpected provider call");
        Box::pin(async move {
            match reply {
                Reply::Json(v) => Ok(v),
                Reply::Fail => Err(anyhow::anyhow!("scripted connection failure")),
                Reply::FailWith(message) => Err(anyhow::anyhow!(message)),
                Reply::Pending => std::future::pending().await,
                Reply::Delay(delay, v) => {
                    tokio::time::sleep(delay).await;
                    Ok(v)
                }
            }
        })
    }
}
fn candidates() -> Vec<Candidate> {
    ["wait", "gather"]
        .into_iter()
        .map(|id| Candidate {
            id: id.into(),
            description: id.into(),
            target: None,
            duration_ms: 1000,
            skill: None,
        })
        .collect()
}
fn jev(handler: &str) -> Value {
    let probs = if handler == "jev" {
        json!({"jev":0.9,"jev_router":0.1})
    } else {
        json!({"jev":0.1,"jev_router":0.9})
    };
    json!({"model":"jev-1.13.0","answers":{
        "handler":{"type":"choice","choice":handler,"confidence":0.8,"probabilities":probs},
        "action":{"type":"choice","choice":"wait","confidence":0.7,"probabilities":{"wait":0.9,"gather":0.1}}},
        "usage":{"input_tokens":80,"output_tokens":12}})
}
fn jev_router_completion(content: &str) -> Value {
    json!({
        "object": "chat.completion", "model": "anthropic/claude-sonnet-4.5",
        "choices": [{"index": 0, "finish_reason": "stop", "message": {
            "role": "assistant", "content": content,
        }}],
        "usage": {"prompt_tokens": 42, "completion_tokens": 7},
    })
}

#[test]
fn jev_router_request_uses_fixed_model_and_bounded_schema() {
    let body = routing::jev_router_request(&Observation::default(), &candidates(), "Gather safely")
        .unwrap();
    assert_eq!(
        routing::JEV_ROUTER_ENDPOINT,
        "https://openrouter.ai/api/v1/chat/completions"
    );
    assert_eq!(body["model"], "typesafe/jev-router");
    assert_eq!(body["provider"]["require_parameters"], true);
    assert_eq!(body["tool_choice"], "none");
    assert!(body.get("tools").is_none());
    assert_eq!(body["response_format"]["type"], "json_schema");
    let schema = &body["response_format"]["json_schema"];
    assert_eq!(schema["strict"], true);
    assert_eq!(schema["schema"]["additionalProperties"], false);
    assert_eq!(
        schema["schema"]["properties"]["choice"]["enum"],
        json!(["gather", "wait"])
    );
    assert!(
        body["messages"][1]["content"]
            .as_str()
            .unwrap()
            .contains("Gather safely")
    );
}

#[test]
fn jev_router_accepts_only_one_supplied_candidate_without_inventing_probabilities() {
    let accepted = routing::validate_jev_router(
        jev_router_completion("{\"choice\":\"gather\"}"),
        &candidates(),
        12,
    )
    .unwrap();
    assert_eq!(accepted.choice, "gather");
    assert_eq!(accepted.model, routing::JEV_ROUTER_MODEL);
    assert!(accepted.probabilities.is_empty());
    assert_eq!(accepted.confidence, None);
    assert_eq!(accepted.input_tokens, Some(42));
    assert_eq!(accepted.output_tokens, Some(7));

    let mut invalid = vec![
        jev_router_completion("{\"choice\":\"unknown\"}"),
        jev_router_completion("{\"choice\":\"gather\",\"confidence\":1}"),
        jev_router_completion("gather"),
    ];
    let mut truncated = jev_router_completion("{\"choice\":\"gather\"}");
    truncated["choices"][0]["finish_reason"] = json!("length");
    invalid.push(truncated);
    let mut tools = jev_router_completion("{\"choice\":\"gather\"}");
    tools["choices"][0]["message"]["tool_calls"] = json!([{}]);
    invalid.push(tools);
    let mut wrong_model = jev_router_completion("{\"choice\":\"gather\"}");
    wrong_model["model"] = json!("bad\nmodel");
    invalid.push(wrong_model);
    for response in invalid {
        assert!(routing::validate_jev_router(response, &candidates(), 12).is_err());
    }
}

#[test]
fn jev_router_rejects_legacy_function_call_even_with_valid_choice() {
    let mut response = jev_router_completion("{\"choice\":\"gather\"}");
    response["choices"][0]["message"]["function_call"] =
        json!({"name": "gather", "arguments": "{}"});

    assert!(routing::validate_jev_router(response, &candidates(), 12).is_err());
}

#[test]
fn old_stage_audit_json_roundtrips_without_served_model() {
    let old = json!({
        "requested_model": "typesafe/jev-router",
        "latency_ms": 12,
        "input_tokens": 42,
        "output_tokens": 7,
        "status": "completed"
    });
    let audit: StageAudit = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(audit.served_model, None);
    assert_eq!(serde_json::to_value(audit).unwrap(), old);
}

async fn run(
    script: &mut Script,
    key: Option<&str>,
    budget: u32,
    timeout: Duration,
) -> (routing::RoutedDecision, Vec<StageAudit>) {
    let observation = Observation::default();
    let candidates = candidates();
    let mut stages = vec![];
    let result = routing::decide_with_transport(
        ("fake-jev-secret", key),
        RoutingRequest {
            observation: &observation,
            candidates: &candidates,
            objective: "Gather safely",
            timeout,
            remaining_requests: budget,
            min_confidence: 0.5,
        },
        |s| stages.push(s),
        script,
    )
    .await
    .unwrap();
    (result, stages)
}

/// The same call with this run's own handler-confidence floor instead of the default.
async fn run_with_floor(
    script: &mut Script,
    key: Option<&str>,
    budget: u32,
    timeout: Duration,
    min_confidence: f64,
) -> (routing::RoutedDecision, Vec<StageAudit>) {
    let observation = Observation::default();
    let candidates = candidates();
    let mut stages = vec![];
    let result = routing::decide_with_transport(
        ("fake-jev-secret", key),
        RoutingRequest {
            observation: &observation,
            candidates: &candidates,
            objective: "Gather safely",
            timeout,
            remaining_requests: budget,
            min_confidence,
        },
        |s| stages.push(s),
        script,
    )
    .await
    .unwrap();
    (result, stages)
}
fn resolved(result: &routing::RoutedDecision) -> &jev_game_engine::model::Decision {
    let RouteOutcome::Resolved { decision } = &result.outcome else {
        panic!("not resolved: {:?}", result.outcome)
    };
    decision
}
fn blocked(result: &routing::RoutedDecision) -> &str {
    let RouteOutcome::Blocked { reason } = &result.outcome else {
        panic!("unexpected action")
    };
    reason
}
#[tokio::test]
async fn jev_self_selection_is_one_call_with_independent_route_and_action_confidence() {
    let mut script = Script::new(vec![Reply::Json(jev("jev"))]);
    let (result, audit) = run(
        &mut script,
        Some("fake-router-secret"),
        2,
        Duration::from_secs(1),
    )
    .await;
    assert_eq!(resolved(&result).choice, "wait");
    assert_eq!(resolved(&result).confidence, Some(0.7));
    assert_eq!(result.route.unwrap().confidence, Some(0.8));
    assert_eq!(result.api_calls, 1);
    assert_eq!(script.calls.len(), 1);
    assert!(script.calls[0].0.starts_with("https://api.typesafe.ai/"));
    assert_eq!(script.calls[0].2["state"]["catalogue"][1]["id"], "gather");
    assert_eq!(
        script.calls[0].2["state"]["handlers"]["jev_router"]["model"],
        routing::JEV_ROUTER_MODEL
    );
    assert!(
        script.calls[0].2["state"]["handlers"]
            .get("astra")
            .is_none()
    );
    assert_eq!(
        audit.iter().map(|s| s.status.as_str()).collect::<Vec<_>>(),
        ["started", "completed"]
    );
}
#[tokio::test]
async fn jev_router_uses_same_catalogue_and_discards_even_invalid_speculative_jev_action() {
    let mut first = jev("jev_router");
    first["answers"]["action"] = json!({"choice":"destroy_everything"});
    let mut script = Script::new(vec![
        Reply::Json(first),
        Reply::Json(jev_router_completion("{\"choice\":\"gather\"}")),
    ]);
    let (result, audit) = run(
        &mut script,
        Some("fake-router-secret"),
        2,
        Duration::from_secs(1),
    )
    .await;
    let action = resolved(&result);
    assert_eq!(action.choice, "gather");
    assert_eq!(action.model, routing::JEV_ROUTER_MODEL);
    assert!(action.confidence.is_none());
    assert!(action.probabilities.is_empty());
    assert_eq!(result.api_calls, 2);
    assert_eq!(script.calls[1].0, routing::JEV_ROUTER_ENDPOINT);
    assert_eq!(script.calls[1].1, "fake-router-secret");
    assert_eq!(script.calls[1].2["model"], routing::JEV_ROUTER_MODEL);
    assert_eq!(script.calls[1].2["tool_choice"], "none");
    assert_eq!(
        script.calls[1].2["response_format"]["json_schema"]["schema"]["properties"]["choice"]["enum"],
        json!(["gather", "wait"])
    );
    assert_eq!(
        audit.iter().map(|s| s.status.as_str()).collect::<Vec<_>>(),
        ["started", "completed", "started", "completed"]
    );
    assert_eq!(audit[1].input_tokens, Some(80));
    assert_eq!(audit[3].input_tokens, Some(42));
    assert_eq!(audit[2].served_model, None);
    assert_eq!(audit[3].requested_model, routing::JEV_ROUTER_MODEL);
    assert_eq!(
        audit[3].served_model.as_deref(),
        Some("anthropic/claude-sonnet-4.5")
    );
    assert_eq!(result.stages[1].served_model, audit[3].served_model);
    assert_eq!(
        serde_json::to_value(&result).unwrap()["stages"][1]["served_model"],
        "anthropic/claude-sonnet-4.5"
    );
}
#[tokio::test]
async fn invalid_served_model_is_rejected_and_never_recorded() {
    for invalid_model in [json!("bad\nmodel"), json!(""), json!(42)] {
        let mut completion = jev_router_completion("{\"choice\":\"gather\"}");
        completion["model"] = invalid_model;
        let mut script = Script::new(vec![
            Reply::Json(jev("jev_router")),
            Reply::Json(completion),
        ]);
        let (result, audit) = run(
            &mut script,
            Some("fake-router-secret"),
            2,
            Duration::from_secs(1),
        )
        .await;
        assert!(blocked(&result).contains("Invalid OpenRouter model metadata"));
        assert_eq!(audit[3].served_model, None);
        assert_eq!(result.stages[1].served_model, None);
    }
}
#[tokio::test]
async fn served_model_cannot_echo_either_supplied_key_into_audit_or_recording() {
    for key in ["fake-jev-secret", "fake-router-secret"] {
        let mut completion = jev_router_completion("{\"choice\":\"gather\"}");
        completion["model"] = json!(format!("provider/{key}:latest"));
        let mut script = Script::new(vec![
            Reply::Json(jev("jev_router")),
            Reply::Json(completion),
        ]);
        let (result, audit) = run(
            &mut script,
            Some("fake-router-secret"),
            2,
            Duration::from_secs(1),
        )
        .await;
        assert!(blocked(&result).contains("Invalid OpenRouter model metadata"));
        assert_eq!(audit[3].served_model, None);
        assert_eq!(result.stages[1].served_model, None);
        let recorded = serde_json::to_string(&result).unwrap();
        assert!(!recorded.contains("fake-jev-secret"));
        assert!(!recorded.contains("fake-router-secret"));
    }
}
#[tokio::test]
async fn provider_error_echoing_credentials_is_not_recorded() {
    for replies in [
        vec![Reply::FailWith("fake-jev-secret".into())],
        vec![
            Reply::Json(jev("jev_router")),
            Reply::FailWith("fake-router-secret and fake-jev-secret".into()),
        ],
    ] {
        let mut script = Script::new(replies);
        let (result, _) = run(
            &mut script,
            Some("fake-router-secret"),
            2,
            Duration::from_secs(1),
        )
        .await;
        let recorded = serde_json::to_string(&result).unwrap();
        assert!(!recorded.contains("fake-jev-secret"));
        assert!(!recorded.contains("fake-router-secret"));
    }
}
#[tokio::test]
async fn uncertain_handler_never_dispatches_or_consumes_speculative_action() {
    // A low or missing self-rated confidence is not a mechanical invalidity: the engine can carry
    // the answer out and the choice belongs to Jev, so the answer is used instead of discarded.
    for confidence in [Value::Null, json!(0.49)] {
        let mut answer = jev("jev");
        if confidence.is_null() {
            answer["answers"]["handler"]
                .as_object_mut()
                .unwrap()
                .remove("confidence");
        } else {
            answer["answers"]["handler"]["confidence"] = confidence;
        }
        let mut script = Script::new(vec![Reply::Json(answer)]);
        let (result, _) = run(
            &mut script,
            Some("fake-router-secret"),
            2,
            Duration::from_secs(1),
        )
        .await;
        assert_eq!(
            resolved(&result).choice,
            "wait",
            "a self-rating must not discard an answer the engine can execute"
        );
        assert_eq!(
            script.calls.len(),
            1,
            "the engine must not spend a second provider call to re-ask"
        );
    }
    // A route answer without usable probabilities is still uncertain: nothing may be dispatched or
    // speculatively consumed on the strength of it.
    let mut broken = jev("jev");
    broken["answers"]["handler"]
        .as_object_mut()
        .unwrap()
        .remove("probabilities");
    let mut script = Script::new(vec![Reply::Json(broken)]);
    let (result, _) = run(
        &mut script,
        Some("fake-router-secret"),
        2,
        Duration::from_secs(1),
    )
    .await;
    assert!(
        blocked(&result).contains("probabilities"),
        "{}",
        blocked(&result)
    );
    assert_eq!(script.calls.len(), 1);
}
#[tokio::test]
async fn a_run_sets_its_own_route_choice_probability_floor() {
    // The floor belongs to the run, not to the engine, and it is measured on the route's own chosen
    // probability - the number that says how clearly the route picked its path. The same route is
    // accepted under the default policy and refused once this run raises its own floor above it.
    let mut narrow = jev("jev");
    narrow["answers"]["handler"]["probabilities"] = json!({"jev":0.55,"jev_router":0.45});

    let mut accepted = Script::new(vec![Reply::Json(narrow.clone())]);
    let (result, _) = run(
        &mut accepted,
        Some("fake-router-secret"),
        2,
        Duration::from_secs(1),
    )
    .await;
    assert_eq!(resolved(&result).choice, "wait");
    assert_eq!(result.route.as_ref().unwrap().confidence, Some(0.8));
    assert_eq!(resolved(&result).confidence, Some(0.7));
    assert_eq!(accepted.calls.len(), 1);

    let mut refused = Script::new(vec![Reply::Json(narrow)]);
    let (blocked_result, _) = run_with_floor(
        &mut refused,
        Some("fake-router-secret"),
        2,
        Duration::from_secs(1),
        0.6,
    )
    .await;
    assert!(
        blocked(&blocked_result).contains("uncertain"),
        "{}",
        blocked(&blocked_result)
    );
    assert_eq!(refused.calls.len(), 1);
}

#[tokio::test]
async fn missing_router_key_and_exhausted_budget_are_explicit_single_call_blockers() {
    for (key, budget, reason) in [
        (None, 2, "OPENROUTER_API_KEY"),
        (Some(" "), 2, "OPENROUTER_API_KEY"),
        (Some("fake-router-secret"), 1, "budget"),
    ] {
        let mut script = Script::new(vec![Reply::Json(jev("jev_router"))]);
        let (result, _) = run(&mut script, key, budget, Duration::from_secs(1)).await;
        assert!(blocked(&result).contains(reason));
        assert_eq!(result.api_calls, 1);
        assert_eq!(script.calls.len(), 1);
    }
}
#[tokio::test]
async fn bad_handler_and_bad_jev_action_are_rejected_with_available_usage_retained() {
    for value in [json!({"answers":{}}), {
        let mut v = jev("jev");
        v["answers"]["action"]["choice"] = json!("absent");
        v
    }] {
        let mut script = Script::new(vec![Reply::Json(value)]);
        let (result, audit) = run(&mut script, None, 1, Duration::from_secs(1)).await;
        assert!(blocked(&result).starts_with(jev_game_engine::provider::REJECTED_ANSWER));
        assert_eq!(result.api_calls, 1);
        assert_eq!(audit.len(), 2);
    }
}
#[tokio::test]
async fn router_refusal_truncation_unknown_choice_and_extra_fields_never_become_actions() {
    let mut refusal = jev_router_completion("{\"choice\":\"gather\"}");
    refusal["choices"][0]["message"]["refusal"] = json!("no");
    let unknown = jev_router_completion("{\"choice\":\"absent\"}");
    let extra = jev_router_completion("{\"choice\":\"wait\",\"target\":42}");
    let mut truncated = jev_router_completion("{\"choice\":\"gather\"}");
    truncated["choices"][0]["finish_reason"] = json!("length");
    for value in [refusal, unknown, extra, truncated] {
        let mut script = Script::new(vec![Reply::Json(jev("jev_router")), Reply::Json(value)]);
        let (result, audit) = run(
            &mut script,
            Some("fake-router-secret"),
            2,
            Duration::from_secs(1),
        )
        .await;
        assert!(
            blocked(&result).starts_with(jev_game_engine::provider::OPENROUTER_REJECTED_ANSWER)
        );
        assert!(!blocked(&result).contains("TypeSafe"));
        assert_eq!(result.api_calls, 2);
        assert_eq!(audit.last().unwrap().input_tokens, Some(42));
    }
}
#[tokio::test]
async fn transport_failures_keep_attempt_count_and_prior_completed_usage() {
    for replies in [
        vec![Reply::Fail],
        vec![Reply::Json(jev("jev_router")), Reply::Fail],
    ] {
        let expected = replies.len();
        let mut script = Script::new(replies);
        let (result, audit) = run(
            &mut script,
            Some("fake-router-secret"),
            2,
            Duration::from_secs(1),
        )
        .await;
        assert!(blocked(&result).contains("request failed"));
        assert_eq!(result.api_calls as usize, expected);
        assert!(audit.last().unwrap().input_tokens.is_none());
        if expected == 2 {
            assert_eq!(audit[1].input_tokens, Some(80));
        }
    }
}

#[tokio::test]
async fn openrouter_http_and_json_errors_keep_safe_provider_identity() {
    for (provider_error, expected) in [
        (
            "OpenRouter request failed (HTTP 429)",
            "OpenRouter request failed (HTTP 429)",
        ),
        (
            "OpenRouter answer rejected: OpenRouter returned invalid JSON",
            "OpenRouter answer rejected: OpenRouter returned invalid JSON",
        ),
        (
            "OpenRouter request failed (HTTP 429) fake-router-secret",
            "OpenRouter request failed",
        ),
    ] {
        let mut script = Script::new(vec![
            Reply::Json(jev("jev_router")),
            Reply::FailWith(provider_error.into()),
        ]);
        let (result, _) = run(
            &mut script,
            Some("fake-router-secret"),
            2,
            Duration::from_secs(1),
        )
        .await;
        assert_eq!(blocked(&result), expected);
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("fake-router-secret")
        );
    }
}
#[tokio::test]
async fn second_provider_receives_only_remaining_total_deadline() {
    let total = Duration::from_secs(2);
    let mut script = Script::new(vec![
        Reply::Delay(Duration::from_millis(20), jev("jev_router")),
        Reply::Json(jev_router_completion("{\"choice\":\"gather\"}")),
    ]);
    let (result, _) = run(&mut script, Some("fake-router-secret"), 2, total).await;
    assert_eq!(resolved(&result).choice, "gather");
    assert_eq!(script.calls[0].3, total);
    assert!(script.calls[1].3 < total - Duration::from_millis(10));
    assert!(!script.calls[1].3.is_zero());
    let mut expired = Script::new(vec![Reply::Json(jev("jev_router"))]);
    let (result, _) = run(&mut expired, Some("fake-router-secret"), 2, Duration::ZERO).await;
    assert!(blocked(&result).contains("deadline"));
    assert_eq!(expired.calls.len(), 1);
}
#[tokio::test]
async fn cancellation_during_router_preserves_completed_jev_audit_without_final_action() {
    let observation = Observation::default();
    let candidates = candidates();
    let mut audit = vec![];
    let mut script = Script::new(vec![Reply::Json(jev("jev_router")), Reply::Pending]);
    let future = routing::decide_with_transport(
        ("fake-jev-secret", Some("fake-router-secret")),
        RoutingRequest {
            observation: &observation,
            candidates: &candidates,
            objective: "Gather",
            timeout: Duration::from_secs(1),
            remaining_requests: 2,
            min_confidence: 0.5,
        },
        |s| audit.push(s),
        &mut script,
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(10), future)
            .await
            .is_err()
    );
    assert_eq!(script.calls.len(), 2);
    assert_eq!(audit.len(), 3);
    assert_eq!(audit[1].status, "completed");
    assert_eq!(audit[1].input_tokens, Some(80));
    assert_eq!(audit[2].status, "started");
}
#[tokio::test]
async fn invalid_request_does_not_spend_a_provider_call() {
    let observation = Observation::default();
    let candidates = candidates();
    let mut script = Script::default();
    assert!(
        routing::decide_with_transport(
            ("fake-jev-secret", None),
            RoutingRequest {
                observation: &observation,
                candidates: &candidates,
                objective: "Gather",
                timeout: Duration::from_secs(1),
                remaining_requests: 0,
                min_confidence: 0.5
            },
            |_| panic!("unexpected audit"),
            &mut script
        )
        .await
        .is_err()
    );
    assert!(script.calls.is_empty());
}
