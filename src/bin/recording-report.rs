//! Offline report for an exported session recording: counts, choices, arrival verdicts and the
//! recording's own validation result. Reads one JSON file; makes no provider call, opens no
//! Minecraft connection and never mutates the recording.
use anyhow::{Context, Result};
use jev_game_engine::{
    model::Event,
    origin::{ActionOrigin, event_origin},
    recording,
};
use std::collections::BTreeMap;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let require_arrival = args.iter().any(|argument| argument == "--require-arrival");
    let as_json = args.iter().any(|argument| argument == "--json");
    let chain = args.iter().any(|argument| argument == "--chain");
    let path = args
        .iter()
        .find(|argument| !argument.starts_with("--"))
        .context("Usage: recording-report <recording.json> [--chain] [--require-arrival] [--json]")?
        .clone();

    // `load` validates schema version, event order, recorded decisions and arrival verdicts.
    let recording = recording::load(&path)?;

    let mut choices: BTreeMap<String, u32> = BTreeMap::new();
    // Each model answer is recorded twice: on its `decision` event and on the `dispatched` event
    // carrying the same decision. Counting only the `decision` events is what makes these counts
    // answer-level, so a reader does not see one choice counted twice.
    let mut answers: BTreeMap<String, u32> = BTreeMap::new();
    let mut requests = 0u32;
    let mut accepted_actions = 0u32;
    // Engine-initiated flight goals carry no model decision, so they never enter `answers`.
    // They are counted from their `dispatched` row (one per reflex action) through the same
    // attribution rule the timeline uses, so the report cannot fold them into model answers.
    let mut reflex_actions = 0u32;
    let mut reflex_events = 0u32;
    let mut arrived: Vec<&Event> = Vec::new();
    let mut expired_with_target = 0u32;
    let mut target_less = 0u32;
    for event in &recording.events {
        match event.kind.as_str() {
            "request" => requests += 1,
            "action" => accepted_actions += 1,
            "reflex" => reflex_events += 1,
            _ => {}
        }
        if event.kind == "dispatched" && event_origin(event) == Some(ActionOrigin::SafetyReflex) {
            reflex_actions += 1;
        }
        if let Some(decision) = &event.decision {
            *choices.entry(decision.choice.clone()).or_default() += 1;
            if event.kind == "decision" {
                *answers.entry(decision.choice.clone()).or_default() += 1;
            }
        }
        match &event.arrival {
            Some(verdict) if verdict.arrived => arrived.push(event),
            Some(verdict) if verdict.target.is_some() => expired_with_target += 1,
            Some(_) => target_less += 1,
            None => {}
        }
    }
    let events = recording.events.len();
    let verdicts = arrived.len() as u32 + expired_with_target + target_less;
    let arrived_count = arrived.len();
    let decisions: u32 = choices.values().sum();
    let answer_count: u32 = answers.values().sum();
    let id = &recording.id;
    let schema = recording.schema_version;
    let mode = &recording.settings.mode;
    let geometry = recording.settings.fixture_waypoint;

    let describe = |event: &Event| -> String {
        let verdict = event.arrival.as_ref().expect("collected from a verdict");
        let sequence = event.sequence;
        let at_ms = event.elapsed_ms;
        let tolerance = verdict.tolerance_m;
        let elapsed = verdict.elapsed_ms;
        let duration = verdict.duration_ms;
        let measured = match verdict.measured_distance_m {
            Some(distance) => format!("{distance:.4} m"),
            None => "no connected observation".to_string(),
        };
        let target = match &verdict.target {
            Some(position) => {
                let (x, y, z) = (position.x, position.y, position.z);
                format!("({x:.2}, {y:.2}, {z:.2})")
            }
            None => "none".to_string(),
        };
        format!(
            "#{sequence} at {at_ms} ms: measured {measured} within tolerance {tolerance:.2} m, \
             target {target}, elapsed {elapsed} ms of {duration} ms"
        )
    };

    if as_json {
        let arrived_details: Vec<String> = arrived.iter().map(|event| describe(event)).collect();
        println!(
            "{}",
            serde_json::json!({
                "recording": path,
                "session": id,
                "schema_version": schema,
                "mode": format!("{mode:?}"),
                "fixture_waypoint": format!("{geometry:?}"),
                "events": events,
                "requests": requests,
                "answers": answer_count,
                "answer_choices": answers,
                "decision_bearing_events": decisions,
                "choices": choices,
                "accepted_actions": accepted_actions,
                "reflex_actions": reflex_actions,
                "reflex_events": reflex_events,
                "arrival_verdicts": verdicts,
                "arrived": arrived_count,
                "expired_with_target": expired_with_target,
                "target_less": target_less,
                "arrived_details": arrived_details,
                "validation": "passed",
            })
        );
    } else {
        println!(
            "Recording {id} · schema version {schema} · mode {mode:?} · fixture geometry {geometry:?}"
        );
        println!(
            "Events {events} · requests {requests} · answers {answer_count} · decision-bearing events {decisions} · accepted actions {accepted_actions}"
        );
        println!(
            "Engine safety-reflex actions {reflex_actions} (dispatched without a model request, \
             labelled SAFETY-REFLEX; {reflex_events} reflex events) - not counted as answers"
        );
        let listed: Vec<String> = answers
            .iter()
            .map(|(choice, count)| format!("{choice} {count}"))
            .collect();
        println!(
            "Answers by choice: {} (per answer; each answer is also recorded on its dispatch, so \
             {decisions} decision-bearing events carry the same choices)",
            listed.join(", ")
        );
        println!(
            "Arrival verdicts {verdicts} · arrived {arrived_count} · expired with a bound target \
             {expired_with_target} · target-less goals (wait) {target_less}"
        );
        if arrived.is_empty() {
            println!("Arrived verdicts: none");
        } else {
            println!("Arrived verdicts:");
            for event in &arrived {
                println!("  {}", describe(event));
                if chain {
                    // Correlate by order rather than by message text: the request, decision,
                    // dispatch and accepted action immediately before a verdict belong to the same
                    // bounded goal. The verdict event itself carries the local Stop.
                    for step in correlated_chain(&recording.events, event.sequence) {
                        let kind = &step.kind;
                        let sequence = step.sequence;
                        let at_ms = step.elapsed_ms;
                        let origin = match event_origin(step) {
                            Some(origin) => format!("{:<12} ", origin.label()),
                            None => String::new(),
                        };
                        let message = step.message.replace('\n', " ");
                        let message = message.chars().take(160).collect::<String>();
                        println!("    #{sequence} {at_ms} ms {origin}{kind}: {message}");
                    }
                }
            }
        }
        println!(
            "Validation: passed. No provider call and no Minecraft connection was made, and the \
             recording was not modified."
        );
    }

    if require_arrival && arrived.is_empty() {
        eprintln!("No arrived verdict in this recording; --require-arrival was requested.");
        std::process::exit(3);
    }
    Ok(())
}

/// The events that lead to one verdict, correlated by order rather than by message text: the
/// request, decision, dispatch and accepted action immediately preceding it in the recording.
fn correlated_chain(events: &[Event], verdict_sequence: u64) -> Vec<&Event> {
    let wanted = ["request", "decision", "dispatched", "action"];
    let Some(index) = events
        .iter()
        .position(|event| event.sequence == verdict_sequence)
    else {
        return Vec::new();
    };
    let mut found: Vec<&Event> = Vec::new();
    for event in events[..index].iter().rev() {
        if wanted.contains(&event.kind.as_str()) {
            found.push(event);
            if found.len() == wanted.len() {
                break;
            }
        }
    }
    found.reverse();
    found
}
