use std::{collections::BTreeMap, fs, path::PathBuf};

use jev_game_engine::{
    model::{Candidate, Decision, Event, Observation, Position, Recording, Settings},
    recording,
};
use tempfile::tempdir;

fn recording() -> Recording {
    Recording {
        schema_version: 1,
        id: "offline-contract-run".into(),
        settings: Settings::default(),
        events: vec![Event {
            sequence: 1,
            elapsed_ms: 10,
            kind: "decision".into(),
            message: "Validated choice".into(),
            observation: Some(Observation {
                world_epoch: 1,
                dimension: Some("fixture:overworld".into()),
                sequence: 1,
                connected: true,
                position: Position {
                    x: 1.5,
                    y: 64.0,
                    z: 2.5,
                },
                health: 20.0,
                food: 20.0,
                inventory: vec!["stone x2".into()],
                blocks: vec![],
                entities: vec![],
                note: "Offline test observation".into(),
            }),
            candidates: vec![Candidate {
                id: "wait".into(),
                description: "Wait".into(),
                target: None,
                duration_ms: 2_000,
            }],
            decision: Some(Decision {
                choice: "wait".into(),
                probabilities: BTreeMap::from([("wait".into(), 1.0)]),
                confidence: Some(1.0),
                model: "jev-contract-fixture".into(),
                input_tokens: Some(10),
                output_tokens: Some(2),
                latency_ms: 300,
            }),
        }],
    }
}

#[test]
fn recorded_facts_survive_loading_without_reinterpretation() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("session.json");
    let original = recording();
    fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
    let loaded = recording::load(path.to_str().unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(&loaded).unwrap(),
        serde_json::to_value(&original).unwrap()
    );
}

#[test]
fn rejects_unsupported_schema_and_invalid_session_ids() {
    for version in [0, 2, u32::MAX] {
        let mut data = recording();
        data.schema_version = version;
        assert!(recording::validate(&data).is_err());
    }
    for id in [String::new(), "x".repeat(129)] {
        let mut data = recording();
        data.id = id;
        assert!(recording::validate(&data).is_err());
    }
}

#[test]
fn events_require_increasing_sequence_and_nondecreasing_time() {
    for (sequence, elapsed) in [(1, 10), (0, 11), (2, 9)] {
        let mut data = recording();
        let mut next = data.events[0].clone();
        next.sequence = sequence;
        next.elapsed_ms = elapsed;
        data.events.push(next);
        assert!(recording::validate(&data).is_err());
    }
    let mut data = recording();
    let mut simultaneous = data.events[0].clone();
    simultaneous.sequence = 2;
    data.events.push(simultaneous);
    assert!(
        recording::validate(&data).is_ok(),
        "separate events can share a millisecond"
    );
}

#[test]
fn load_rejects_corrupt_order_instead_of_sorting_or_repairing_it() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("corrupt.json");
    let mut data = recording();
    let mut next = data.events[0].clone();
    next.sequence = 2;
    next.elapsed_ms = 0;
    data.events.push(next);
    fs::write(&path, serde_json::to_vec(&data).unwrap()).unwrap();
    assert!(recording::load(path.to_str().unwrap()).is_err());
}

#[test]
fn bounds_event_count_and_display_text() {
    let mut data = recording();
    data.events = (0..5_000)
        .map(|i| {
            let mut event = data.events[0].clone();
            event.sequence = i;
            event.elapsed_ms = i;
            event
        })
        .collect();
    assert!(recording::validate(&data).is_ok());
    let mut extra = data.events[0].clone();
    extra.sequence = 5_000;
    extra.elapsed_ms = 5_000;
    data.events.push(extra);
    assert!(recording::validate(&data).is_err());
    let mut data = recording();
    data.events[0].kind = "k".repeat(128);
    data.events[0].message = "m".repeat(8192);
    assert!(recording::validate(&data).is_ok());
    data.events[0].kind.push('k');
    assert!(recording::validate(&data).is_err());
    data.events[0].kind = "decision".into();
    data.events[0].message.push('m');
    assert!(recording::validate(&data).is_err());
}

#[test]
fn nonfinite_recorded_positions_are_rejected_before_serialization() {
    for coordinate in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        for axis in 0..3 {
            let mut data = recording();
            let position = &mut data.events[0].observation.as_mut().unwrap().position;
            match axis {
                0 => position.x = coordinate,
                1 => position.y = coordinate,
                _ => position.z = coordinate,
            }
            assert!(recording::validate(&data).is_err());
        }
    }
}

#[test]
fn replay_choice_must_exist_among_recorded_candidates() {
    let mut data = recording();
    data.events[0].decision.as_mut().unwrap().choice = "unrecorded_action".into();
    assert!(recording::validate(&data).is_err());
}

#[test]
fn replay_probability_map_must_match_candidates_and_form_distribution() {
    for probabilities in [
        BTreeMap::new(),
        BTreeMap::from([("other".into(), 1.0)]),
        BTreeMap::from([("wait".into(), 0.2)]),
        BTreeMap::from([("wait".into(), 1.0), ("extra".into(), 0.0)]),
    ] {
        let mut data = recording();
        data.events[0].decision.as_mut().unwrap().probabilities = probabilities;
        assert!(
            recording::validate(&data).is_err(),
            "replay must not display an incoherent probability distribution"
        );
    }
}

#[test]
fn replay_rejects_invalid_probability_and_confidence_numbers() {
    for invalid in [f64::NAN, f64::INFINITY, -0.1, 1.1] {
        let mut data = recording();
        data.events[0]
            .decision
            .as_mut()
            .unwrap()
            .probabilities
            .insert("wait".into(), invalid);
        assert!(recording::validate(&data).is_err());
        let mut data = recording();
        data.events[0].decision.as_mut().unwrap().confidence = Some(invalid);
        assert!(
            recording::validate(&data).is_err(),
            "recorded confidence must satisfy the live contract"
        );
    }
}

#[test]
fn replay_rejects_duplicate_candidate_identity() {
    let mut data = recording();
    let duplicate = data.events[0].candidates[0].clone();
    data.events[0].candidates.push(duplicate);
    assert!(
        recording::validate(&data).is_err(),
        "an action must have one unambiguous recorded candidate"
    );
}

#[test]
fn load_rejects_wrong_extension_missing_invalid_and_oversized_files() {
    let directory = tempdir().unwrap();
    let wrong = directory.path().join("recording.txt");
    fs::write(&wrong, serde_json::to_vec(&recording()).unwrap()).unwrap();
    assert!(recording::load(wrong.to_str().unwrap()).is_err());
    assert!(recording::load(directory.path().join("missing.json").to_str().unwrap()).is_err());
    let malformed = directory.path().join("malformed.json");
    fs::write(&malformed, b"{not-json").unwrap();
    assert!(recording::load(malformed.to_str().unwrap()).is_err());
    let oversized = directory.path().join("oversized.json");
    fs::File::create(&oversized)
        .unwrap()
        .set_len(32 * 1024 * 1024 + 1)
        .unwrap();
    assert!(recording::load(oversized.to_str().unwrap()).is_err());
}

#[test]
fn legacy_recording_without_dimension_loads_as_unknown() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("legacy.json");
    let mut legacy = serde_json::to_value(recording()).unwrap();
    let observation = legacy["events"][0]["observation"].as_object_mut().unwrap();
    assert!(observation.remove("dimension").is_some());
    fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();

    let loaded = recording::load(path.to_str().unwrap()).unwrap();

    assert!(
        loaded.events[0]
            .observation
            .as_ref()
            .unwrap()
            .dimension
            .is_none()
    );
    assert_eq!(loaded.schema_version, 1);
}

struct SavedFile(PathBuf);
impl Drop for SavedFile {
    fn drop(&mut self) {
        // Only remove the exact file returned by save(), never an entire run directory.
        let _ = fs::remove_file(&self.0);
    }
}

#[test]
fn saving_twice_preserves_previous_run_and_roundtrips() {
    let original = recording();
    let first = SavedFile(PathBuf::from(recording::save(&original).unwrap()));
    let first_bytes = fs::read(&first.0).unwrap();
    let mut later = original.clone();
    later.id = "second-offline-run".into();
    let second = SavedFile(PathBuf::from(recording::save(&later).unwrap()));
    assert_ne!(first.0, second.0);
    assert_eq!(fs::read(&first.0).unwrap(), first_bytes);
    assert_eq!(
        recording::load(first.0.to_str().unwrap()).unwrap().id,
        original.id
    );
    assert_eq!(
        recording::load(second.0.to_str().unwrap()).unwrap().id,
        later.id
    );
}
