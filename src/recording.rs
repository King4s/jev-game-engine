use crate::model::{Candidate, Decision, Mode, Recording};
use anyhow::{Context, Result, ensure};
use serde_json::json;
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_BYTES: u64 = 32 * 1024 * 1024;

/// Writes a new recording without overwriting an existing session.
pub fn save(recording: &Recording) -> Result<String> {
    validate(recording)?;
    let bytes = serde_json::to_vec_pretty(recording).context("Could not serialize recording")?;
    ensure!(bytes.len() as u64 <= MAX_BYTES, "Recording is too large");
    fs::create_dir_all("runs").context("Could not create runs directory")?;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let path = format!("runs/session-{stamp}.json");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .context("Could not create recording file")?;
    file.write_all(&bytes)
        .context("Could not write recording")?;
    file.sync_all()
        .context("Could not save recording to disk")?;
    Ok(path)
}

/// Replay data has no executable instructions and never initiates connections.
pub fn load(path: &str) -> Result<Recording> {
    ensure!(
        Path::new(path).extension().is_some_and(|ext| ext == "json"),
        "Select a JSON recording"
    );
    let file = fs::File::open(path).context("Could not open recording")?;
    ensure!(
        file.metadata()?.len() <= MAX_BYTES,
        "Recording is too large"
    );
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= MAX_BYTES, "Recording is too large");
    let recording = serde_json::from_slice(&bytes).context("Invalid recording JSON")?;
    validate(&recording)?;
    Ok(recording)
}

pub fn validate(recording: &Recording) -> Result<()> {
    ensure!(
        recording.schema_version == 1,
        "Recording version is not supported"
    );
    ensure!(
        recording.events.len() <= 5_000,
        "Recording contains too many events"
    );
    ensure!(
        !recording.id.is_empty() && recording.id.len() <= 128,
        "Invalid session ID"
    );
    let mut previous = None;
    for event in &recording.events {
        if let Some((sequence, elapsed)) = previous {
            ensure!(
                event.sequence > sequence && event.elapsed_ms >= elapsed,
                "Invalid recording event order"
            );
        }
        previous = Some((event.sequence, event.elapsed_ms));
        ensure!(
            event.kind.len() <= 128 && event.message.len() <= 8192,
            "Event text is too long"
        );
        let mut candidate_ids = BTreeSet::new();
        ensure!(
            event.candidates.len() <= 255,
            "Too many recorded candidates"
        );
        for candidate in &event.candidates {
            ensure!(
                !candidate.id.trim().is_empty() && candidate_ids.insert(candidate.id.as_str()),
                "Invalid or duplicate recorded candidate ID"
            );
        }
        if let Some(observation) = &event.observation {
            ensure!(
                observation.position.x.is_finite()
                    && observation.position.y.is_finite()
                    && observation.position.z.is_finite(),
                "Invalid position"
            );
        }
        if let Some(decision) = &event.decision {
            validate_decision(decision, &event.candidates, &recording.settings.mode)?;
        }
    }
    Ok(())
}

fn validate_decision(decision: &Decision, candidates: &[Candidate], mode: &Mode) -> Result<()> {
    if decision.model == "offline-fixture" {
        ensure!(
            *mode == Mode::Demo,
            "Offline fixture decision in a live recording"
        );
        ensure!(
            decision.probabilities.is_empty()
                && decision.confidence.is_none()
                && decision.input_tokens.is_none()
                && decision.output_tokens.is_none(),
            "Offline fixture must not contain model probabilities, confidence or usage"
        );
        ensure!(
            candidates
                .iter()
                .any(|candidate| candidate.id == decision.choice),
            "Selected choice is missing from candidates"
        );
        return Ok(());
    }

    // Keep recorded model answers subject to the same semantic contract as live
    // answers. Optional absent metrics stay absent rather than becoming null/zero.
    // This is pure validation; the provider helper performs no network request.
    let mut answer = json!({
        "type": "choice",
        "choice": decision.choice,
        "probabilities": decision.probabilities,
    });
    if let Some(confidence) = decision.confidence {
        answer["confidence"] = json!(confidence);
    }
    let mut usage = json!({});
    if let Some(tokens) = decision.input_tokens {
        usage["input_tokens"] = json!(tokens);
    }
    if let Some(tokens) = decision.output_tokens {
        usage["output_tokens"] = json!(tokens);
    }
    crate::provider::validate_response(
        json!({"model": decision.model, "answers": {"action": answer}, "usage": usage}),
        candidates,
        decision.latency_ms,
    )
    .context("Invalid recorded model decision")?;
    Ok(())
}
