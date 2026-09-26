//! Backwards compatibility of `Settings::fixture_waypoint`.
//!
//! The offline fixture is built with one of two waypoint geometries, and `Settings::fixture_waypoint`
//! selects between them for a session. The published claim is two-part: a recording written before
//! the field existed still loads, and a recording, an operator or a test states which geometry
//! produced a verdict. These tests pin that claim down without a provider and without a Minecraft
//! connection: an absent key defaults to `Distant`, both variants round-trip through serde and
//! `recording::load` unchanged, and a value that names no known geometry is rejected instead of
//! being quietly rewritten.
//!
//! Scope: `tests/recording_contract.rs` already covers the same legacy-loading contract for the
//! session `dimension` and the general recording validator, and `tests/navigation_arrival.rs`
//! covers `Event::arrival`. This file only adds `Settings::fixture_waypoint`, so it asserts the
//! field itself rather than re-checking their work.
//!
//! Limit: this file proves how the field is written, read and defaulted, not what the engine does
//! with it. That `FixtureWaypoint::Reachable` makes the fixture actually arrive is asserted through
//! the engine in `tests/fixture_geometry_arrival.rs`, and is not repeated here.
//!
//! Everything here is deterministic and offline: fixed ids instead of timestamps, no sleeps, no
//! network, no game.

use std::{fs, path::Path};

use jev_game_engine::{
    model::{Event, FixtureWaypoint, Recording, Settings},
    recording,
};
use serde_json::{Value, json};
use tempfile::tempdir;

/// One recording that `recording::validate` accepts: a fixed id, the default settings and a single
/// control event. Nothing time-shaped is written, so the same bytes are produced on every run.
fn valid_recording() -> Recording {
    Recording {
        schema_version: 1,
        id: "offline-waypoint-compat".into(),
        settings: Settings::default(),
        events: vec![Event {
            sequence: 1,
            elapsed_ms: 0,
            kind: "control".into(),
            message: "Offline fixture session started".into(),
            observation: None,
            candidates: vec![],
            decision: None,
            arrival: None,
        }],
    }
}

fn write_json(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}

#[test]
fn the_default_session_keeps_the_distant_fixture_waypoint() {
    // Documented default: the offline demo keeps showing the honest expiry path unless a session
    // explicitly asks for `Reachable`.
    assert_eq!(
        Settings::default().fixture_waypoint,
        FixtureWaypoint::Distant
    );
    // The serde fallback for an absent key is exactly this documented default.
    assert_eq!(FixtureWaypoint::default(), FixtureWaypoint::Distant);
}

#[test]
fn a_recording_without_the_fixture_waypoint_key_loads_as_distant() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("legacy.json");

    // Reproduce exactly what a build without `Settings::fixture_waypoint` wrote: the same valid v1
    // recording with that single key removed from `settings`. Everything else stays as the current
    // build writes it, so the missing key is provably the only difference.
    let mut legacy = serde_json::to_value(valid_recording()).unwrap();
    assert!(
        legacy["settings"]
            .as_object_mut()
            .expect("recorded settings object")
            .remove("fixture_waypoint")
            .is_some()
    );
    write_json(&path, &legacy);

    let loaded = recording::load(path.to_str().unwrap()).expect("a legacy recording must load");
    recording::validate(&loaded).expect("the legacy recording is still a valid v1 recording");

    assert_eq!(loaded.settings.fixture_waypoint, FixtureWaypoint::Distant);
    assert_eq!(loaded.schema_version, 1);

    // The absent key is the only difference: the loaded recording re-serialises to the same JSON a
    // build that carried the field would have written, whose value is the documented default.
    let current = serde_json::to_value(valid_recording()).unwrap();
    assert_eq!(current["settings"]["fixture_waypoint"], json!("Distant"));
    assert_eq!(serde_json::to_value(&loaded).unwrap(), current);
}

#[test]
fn both_fixture_waypoint_variants_survive_serde_and_load() {
    // Pin the wire format the compatibility tests above and below read and write.
    assert_eq!(
        serde_json::to_value(FixtureWaypoint::Distant).unwrap(),
        json!("Distant")
    );
    assert_eq!(
        serde_json::to_value(FixtureWaypoint::Reachable).unwrap(),
        json!("Reachable")
    );

    let directory = tempdir().unwrap();
    for (variant, name) in [
        (FixtureWaypoint::Distant, "Distant"),
        (FixtureWaypoint::Reachable, "Reachable"),
    ] {
        let mut original = valid_recording();
        original.settings.fixture_waypoint = variant;
        let serialized = serde_json::to_vec_pretty(&original).unwrap();
        let text = std::str::from_utf8(&serialized).unwrap();
        assert!(
            text.contains("fixture_waypoint"),
            "a recording must state the geometry it used: {text}"
        );
        assert_eq!(
            serde_json::to_value(&original).unwrap()["settings"]["fixture_waypoint"],
            json!(name)
        );

        // Serde round-trip: the variant comes back unchanged.
        let round_tripped: Recording = serde_json::from_slice(&serialized).unwrap();
        assert_eq!(round_tripped.settings.fixture_waypoint, variant);
        assert_eq!(
            serde_json::to_value(&round_tripped).unwrap(),
            serde_json::to_value(&original).unwrap()
        );

        // The loader the app uses returns the same variant.
        let path = directory.path().join(format!("waypoint-{name}.json"));
        fs::write(&path, &serialized).unwrap();
        let loaded = recording::load(path.to_str().unwrap()).expect("a valid recording must load");
        assert_eq!(loaded.settings.fixture_waypoint, variant);
        assert_eq!(
            serde_json::to_value(&loaded).unwrap(),
            serde_json::to_value(&original).unwrap()
        );
    }
}

#[test]
fn an_unknown_fixture_geometry_is_rejected_rather_than_defaulted() {
    // Rejection is the honest behaviour here. The published claim is that a recording states which
    // geometry produced its verdicts. Only the *absence* of the key is honestly defaultable,
    // because a build from before the field existed had exactly one geometry; a value this build
    // does not recognise cannot be resolved to either fixture layout, so reading it as `Distant`
    // would present a guess as a recorded fact. Surfacing it as an error keeps the claim literally
    // true: a typo, or a geometry from a newer build, is never silently rewritten into a geometry
    // the recording never named.
    let directory = tempdir().unwrap();
    for (index, unknown) in ["Sideways", "distant", "Reachable ", ""]
        .into_iter()
        .enumerate()
    {
        let mut value = serde_json::to_value(valid_recording()).unwrap();
        value["settings"]
            .as_object_mut()
            .expect("recorded settings object")
            .insert("fixture_waypoint".into(), json!(unknown));

        // The deserializer itself refuses, so even a caller that never validates cannot receive a
        // quietly defaulted geometry.
        let typed: Result<Recording, _> = serde_json::from_value(value.clone());
        assert!(
            typed.is_err(),
            "serde must reject the unknown fixture geometry {unknown:?} instead of defaulting it"
        );

        // The same rejection reaches the loader the app uses, so a recording cannot smuggle an
        // unrecognised geometry past `recording::load` either. The file is otherwise valid, so the
        // geometry is the only reason the load fails.
        let path = directory.path().join(format!("unknown-{index}.json"));
        write_json(&path, &value);
        assert!(
            recording::load(path.to_str().unwrap()).is_err(),
            "`recording::load` must reject the unknown fixture geometry {unknown:?}"
        );
    }
}
