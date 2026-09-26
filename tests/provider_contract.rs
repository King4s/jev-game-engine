use jev_game_engine::{model::Candidate, provider::validate_response};
use serde_json::{Value, json};

fn candidates() -> Vec<Candidate> {
    ["wait", "waypoint_1"]
        .into_iter()
        .map(|id| Candidate {
            id: id.into(),
            description: id.into(),
            target: None,
            duration_ms: 2_000,
        })
        .collect()
}

fn response() -> Value {
    json!({
        "model": "jev-1.13.0",
        "answers": {"action": {"type": "choice", "choice": "wait",
            "probabilities": {"wait": 0.75, "waypoint_1": 0.25}, "confidence": 0.42}},
        "usage": {"input_tokens": 321, "output_tokens": 17}
    })
}

#[test]
fn preserves_actual_model_metrics_and_measured_latency() {
    let decision = validate_response(response(), &candidates(), 307).unwrap();
    assert_eq!(decision.model, "jev-1.13.0");
    assert_eq!(decision.choice, "wait");
    assert_eq!(decision.probabilities["wait"], 0.75);
    assert_eq!(decision.probabilities["waypoint_1"], 0.25);
    assert_eq!(decision.confidence, Some(0.42));
    assert_eq!(decision.input_tokens, Some(321));
    assert_eq!(decision.output_tokens, Some(17));
    assert_eq!(decision.latency_ms, 307);
}

#[test]
fn absent_optional_metrics_are_unknown_not_fabricated_zeroes() {
    let mut value = response();
    value["answers"]["action"]
        .as_object_mut()
        .unwrap()
        .remove("confidence");
    value.as_object_mut().unwrap().remove("usage");
    let decision = validate_response(value, &candidates(), 1).unwrap();
    assert_eq!(decision.confidence, None);
    assert_eq!(decision.input_tokens, None);
    assert_eq!(decision.output_tokens, None);
    let mut value = response();
    value["usage"] = json!({"input_tokens": 0});
    let decision = validate_response(value, &candidates(), 1).unwrap();
    assert_eq!(decision.input_tokens, Some(0));
    assert_eq!(decision.output_tokens, None);
}

#[test]
fn rejects_wrong_response_shape_or_missing_action_answer() {
    for malformed in [
        Value::Null,
        json!([]),
        json!("text"),
        json!({}),
        json!({"answers": {"other": {"choice": "wait"}}}),
        json!({"answers": {"action": []}}),
    ] {
        assert!(validate_response(malformed, &candidates(), 1).is_err());
    }
    for field in ["type", "choice", "probabilities"] {
        let mut value = response();
        value["answers"]["action"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(
            validate_response(value, &candidates(), 1).is_err(),
            "missing {field}"
        );
    }
    for (field, bad) in [
        ("type", json!("noul")),
        ("choice", json!(5)),
        ("choice", json!("not_available")),
        ("probabilities", json!([])),
    ] {
        let mut value = response();
        value["answers"]["action"][field] = bad;
        assert!(
            validate_response(value, &candidates(), 1).is_err(),
            "invalid {field}"
        );
    }
}

#[test]
fn probabilities_must_cover_exactly_the_offered_choices() {
    for probabilities in [
        json!({"wait": 1.0}),
        json!({"wait": 0.75, "unknown": 0.25}),
        json!({"wait": 0.75, "waypoint_1": 0.25, "unknown": 0.0}),
    ] {
        let mut value = response();
        value["answers"]["action"]["probabilities"] = probabilities;
        assert!(validate_response(value, &candidates(), 1).is_err());
    }
}

#[test]
fn rejects_invalid_probability_numbers_and_incoherent_distributions() {
    for bad in [
        json!(-0.01),
        json!(1.01),
        Value::Null,
        json!("NaN"),
        json!("Infinity"),
        json!(false),
        json!({}),
    ] {
        let mut value = response();
        value["answers"]["action"]["probabilities"]["wait"] = bad;
        assert!(validate_response(value, &candidates(), 1).is_err());
    }
    for probabilities in [
        json!({"wait": 0.0, "waypoint_1": 0.0}),
        json!({"wait": 0.9, "waypoint_1": 0.9}),
        json!({"wait": 0.25, "waypoint_1": 0.75}),
    ] {
        let mut value = response();
        value["answers"]["action"]["probabilities"] = probabilities;
        assert!(validate_response(value, &candidates(), 1).is_err());
    }
}

#[test]
fn accepts_ties_and_small_serialization_rounding_without_renormalizing() {
    for probabilities in [
        json!({"wait": 0.5, "waypoint_1": 0.5}),
        json!({"wait": 0.75, "waypoint_1": 0.2495}),
    ] {
        let mut value = response();
        value["answers"]["action"]["probabilities"] = probabilities.clone();
        let decision = validate_response(value, &candidates(), 1).unwrap();
        assert_eq!(
            decision.probabilities["waypoint_1"],
            probabilities["waypoint_1"].as_f64().unwrap()
        );
    }
}

#[test]
fn rejects_malformed_optional_confidence_and_usage() {
    for bad in [
        Value::Null,
        json!(-0.1),
        json!(1.1),
        json!("0.5"),
        json!(true),
    ] {
        let mut value = response();
        value["answers"]["action"]["confidence"] = bad;
        assert!(validate_response(value, &candidates(), 1).is_err());
    }
    for bad in [Value::Null, json!([]), json!("unknown")] {
        let mut value = response();
        value["usage"] = bad;
        assert!(validate_response(value, &candidates(), 1).is_err());
    }
    for key in ["input_tokens", "output_tokens"] {
        for bad in [Value::Null, json!(-1), json!(0.5), json!("3"), json!(false)] {
            let mut value = response();
            value["usage"][key] = bad;
            assert!(
                validate_response(value, &candidates(), 1).is_err(),
                "invalid {key}"
            );
        }
    }
}

#[test]
fn rejects_missing_or_unusable_model_identity() {
    let mut value = response();
    value.as_object_mut().unwrap().remove("model");
    assert!(validate_response(value, &candidates(), 1).is_err());
    for bad in [
        Value::Null,
        json!(5),
        json!(""),
        json!("model\nsecret"),
        json!("x".repeat(129)),
    ] {
        let mut value = response();
        value["model"] = bad;
        assert!(validate_response(value, &candidates(), 1).is_err());
    }
}

#[test]
fn invalid_candidate_sets_cannot_define_an_action_space() {
    assert!(validate_response(response(), &[], 1).is_err());
    let mut duplicate = candidates();
    duplicate[1].id = duplicate[0].id.clone();
    assert!(validate_response(response(), &duplicate, 1).is_err());
    let mut blank = candidates();
    blank[0].id = " \t".into();
    assert!(validate_response(response(), &blank, 1).is_err());
    let too_many: Vec<_> = (0..256)
        .map(|i| Candidate {
            id: format!("option_{i}"),
            description: String::new(),
            target: None,
            duration_ms: 1,
        })
        .collect();
    assert!(validate_response(response(), &too_many, 1).is_err());
}

#[test]
fn rejected_server_content_is_not_reflected_in_errors() {
    const SECRET: &str = "private-marker-that-must-not-be-logged";
    for path in [
        "/answers/action/choice",
        "/answers/action/confidence",
        "/model",
        "/usage/input_tokens",
    ] {
        let mut value = response();
        *value.pointer_mut(path).unwrap() = json!(format!("{SECRET}\n"));
        let error = validate_response(value, &candidates(), 1).unwrap_err();
        assert!(!format!("{error:?}").contains(SECRET));
    }
}

#[test]
fn a_distribution_that_does_not_sum_to_one_names_the_computed_sum() {
    let mut value = response();
    value["answers"]["action"]["probabilities"] = json!({"wait": 0.6, "waypoint_1": 0.3});
    let error = validate_response(value, &candidates(), 1)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("sum 0.9000 over 2 options"),
        "the sum is computed here, so it can be named: {error}"
    );
}
