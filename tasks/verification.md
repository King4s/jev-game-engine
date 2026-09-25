# Verification evidence

2026-09-25. Development prototype, not a stable release.

## Automated and UI checks

Jev-loop run `20260925-200554`, turn 30 passed all 69 offline tests, `cargo check`, and `cargo clippy --all-targets -- -D warnings`. Tests cover provider validation, latency bounds, cancellation and stale answers, request/time budgets, action acknowledgements and rejection, same-dimension respawns, adapter bounds, recording validation, immutable replay and headless UI rendering.

Rust nightly-2026-09-07 and Azalea revision `b65fa8cf1bb957976cefa926b9b500d44767d806` compile together for Minecraft Java 26.2. Newer nightly-2026-09-24 removed a macro used by this Azalea revision.

Native English fixture and live UI screenshots were visually inspected. The public [screenshot](../docs/images/native-ui.png) contains synthetic fixture data, clearly labelled. Keyboard/accessibility usability has not received a full manual audit.

## Live observations and execution

The native Rust adapter joined an authorized Paper 26.2 test backend through a loopback SSH tunnel, observed actual position, health, inventory, nearby blocks/entities and validated waypoints, then disconnected cleanly. A separate player used the normal Minecraft client to watch it; the engine itself provides telemetry, not a 3D video feed.

After fixing Azalea Stop/Goto update ordering, a local manual diagnostic navigated to a verified nearby waypoint. The pathfinder reported arrival; measured displacement was 1.1180 blocks over a bounded 2-second window, followed by 0.0000 additional displacement after Stop, with unchanged health 20. This verifies local navigation, not model-selected navigation.

Production TypeSafe requests returned validated `jev-1.13.0` decisions on actual observations. A sheltered Creative-mode test selected wait (probability 0.66 versus navigate 0.34), latency 654 ms. The adapter acknowledged the action; position and health remained unchanged and disconnect completed. A previous survival test took damage while waiting; its movement is explicitly not counted as commanded navigation. External-world physics continue after local Stop.

## Limits

A completed model-selected navigation target remains unproven. Mining, building, combat/escape behavior, game discovery and a second game adapter are not implemented. Runtime facts, offline fixtures and planned features must be reported separately. Private account inventories, world backups and machine-specific operational notes are excluded from publication.

An independent read-only review examined the source and acceptance evidence. It identified manual-target binding, acceptance timing, acknowledgement rollback and replay-history gaps; fixes received regression coverage. The final re-review returned no remaining findings. This review does not establish reliable autonomous gameplay or replace broader field testing.

The engine does not automatically change server configuration, player accounts or existing world files. Separate operator-authorized world conversion work was outside the application.
