# Tasks — Rust AI Game Engine

Current scope: **Rust, Minecraft Java first, Farming Simulator 25 next. No 2048 or Snake.** The UI is native Rust with egui/eframe. The user approved Minecraft goals with local movement; approximately 300 ms model latency is handled through macro selection, a local executor and event-driven replanning. See [latency-design.md](latency-design.md) and [plan.md](plan.md). These checkboxes are planned requirements, not evidence of completed code.

The Jev loop selects the next focused task; subagents have separate file ownership and independent review. Keep each task to roughly 3–5 handwritten files. Proposed paths below are planning references, not a claim that the implementation uses that layout. Standard verification includes relevant focused tests, `cargo check`, `cargo fmt --check` and build; UI tests follow the selected framework. Commands become applicable after scaffolding.

Report fixtures, real game connections and real Jev inference separately. Missing external prerequisites remain open; mocks do not replace live evidence.

## Task 1: Define the Rust contract for goals and local execution

**Description:** Turn research and the approved macro architecture into concrete contracts and version choices.
**Acceptance:**

- [ ] Justify the Minecraft client/protocol or bounded bridge using current official sources, supported server versions and control model.
- [ ] Retain native egui/eframe and a Rust engine; distinguish goals, progress, cancellation and executor provenance.
- [ ] Document TypeSafe integration and authentication/server requirements; reusable settings profiles avoid hardcoded personal paths.

**Verification:** Source and compatibility review; no unsupported live-connection claims.
**Dependencies:** None. **Size:** S–M.
**Files:** `docs/architecture.md`, `docs/minecraft-setup.md`, `tasks/plan.md`.

## Task 2: Runnable Rust shell and adapter contract

**Description:** Establish a minimal workspace with a launchable UI shell and shared observation/action/event contracts.
**Acceptance:**

- [ ] A documented command starts locally on Windows; build/check work.
- [ ] Declare worldPause/reset/restore/replay capabilities instead of assuming them.
- [ ] Define validated commands and stable observation/action IDs.

**Verification:** Cargo check/build, contract tests and opening the UI.
**Dependencies:** 1. **Size:** M.
**Files:** `Cargo.toml`, `src/main.rs`, `src/contracts.rs`, selected UI entry point, `README.md`.

## Task 3: Minimal Minecraft connection

**Description:** Connect through the chosen integration and read position/connection status from a separate local test world.
**Acceptance:**

- [ ] Validate version/address/authentication; explain that the bot is a separate player if that control model is used.
- [ ] Disconnect cleans up resources without changing existing saves or player accounts.
- [ ] Record fixture tests and real server smoke tests separately, or document the concrete missing live prerequisite.

**Verification:** Connection/disconnection fixtures and a real connection probe when a compatible server and access exist.
**Dependencies:** 2. **Size:** M.
**Files:** `src/minecraft/connection.rs`, `src/minecraft/config.rs`, `tests/minecraft_connection.rs`, `docs/minecraft-setup.md`.

### Checkpoint A

- [ ] Rust/UI direction is fixed and buildable.
- [ ] Minecraft compatibility is tested or has a concrete unresolved prerequisite.
- [ ] Independent review distinguishes research, fixtures and live evidence.

## Task 4: Display the observed world

**Description:** Normalize Minecraft data and display telemetry with a simple local map.
**Acceptance:**

- [ ] Position, available health/inventory and local observations have timestamps/age.
- [ ] Unknown areas and missing data remain visibly unknown; fixtures are labelled.
- [ ] The UI promises neither integrated 3D streaming nor complete world state.

**Verification:** Observation fixtures and visual inspection with fixtures and real connections where possible.
**Dependencies:** 3. **Size:** M.
**Files:** `src/minecraft/observation.rs`, `tests/observation.rs`, selected UI view, UI state binding.

## Task 5: Local navigation-goal executor

**Description:** Execute navigation goals through local movement, progress checks and immediate cancellation.
**Acceptance:**

- [ ] Candidates have stable IDs, durations and preconditions; combat/digging/placement are not required.
- [ ] Actions finish and release input on duration limits and stop.
- [ ] Results distinguish requested actions from actually observed movement.

**Verification:** Action/cleanup tests and a bounded live probe when server access exists.
**Dependencies:** 4. **Size:** M.
**Files:** `src/minecraft/adapter.rs`, `tests/minecraft_actions.rs`, selected UI controls, `src/main.rs`.

## Task 6: One visible Jev decision

**Description:** Implement a Rust-compatible TypeSafe provider and show observation, candidates, response and executed action.
**Acceptance:**

- [ ] The key stays in the Rust process; Jev chooses a current validated goal/macro ID lasting several seconds; local sub-movements belong to the executor.
- [ ] Show confidence/probabilities/usage only when returned; invent no explanations.
- [ ] Invalid responses and missing keys produce concrete errors without hidden test fallback.

**Verification:** Provider contract tests and one budget-limited real Jev call when credentials are available.
**Dependencies:** 5. **Size:** M.
**Files:** `src/providers/jev.rs`, `tests/jev_contract.rs`, selected decision view, `src/main.rs`.

### Checkpoint B

- [ ] Demonstrate observation → candidate → Jev → validated action → UI, or leave the live prerequisite explicitly open.
- [ ] Manual input is bounded and cleaned up on stop.
- [ ] Review provider/adapter contracts separately from gameplay quality.

## Task 7: Shared session controls and budgets

**Description:** Combine start/pause/one-decision/stop in the Rust engine with generation IDs and command IDs.
**Acceptance:**

- [ ] Pause/stop/controller changes reject late responses; duplicate command IDs cannot execute twice.
- [ ] At most one provider request is active; request/duration budgets include retries.
- [ ] Agent pause does not freeze the world; one decision does not promise a single world tick.

**Verification:** Controlled futures/clocks for race and budget tests; UI stop while a request is pending.
**Dependencies:** 6. **Size:** M.
**Files:** `src/engine/session.rs`, `tests/session.rs`, `src/contracts.rs`, selected UI controls.

## Task 8: Event-driven replanning and manual takeover

**Description:** Replan on goal results, blockage or meaningful changes; bind responses to age/expiry and handle disconnect without stuck input.
**Acceptance:**

- [ ] Reject stale responses; adapt goal/replan horizons to measured latency within profile bounds without bypassing freshness limits or local stop.
- [ ] Pause, stop, timeout and disconnect release input and cancel active work.
- [ ] Manual bot control invalidates Jev requests and marks mixed-control.

**Verification:** Inject low/high latency, disconnects and controller changes; check bounded adaptation and live stop during movement where available.
**Dependencies:** 7. **Size:** M.
**Files:** `src/engine/scheduler.rs`, `tests/scheduler.rs`, `src/engine/session.rs`, `src/minecraft/adapter.rs`.

## Task 9: Traceable event log and recording

**Description:** Save a manifest and sequential observation/request/response/applied/rejected/result events.
**Acceptance:**

- [ ] IDs link Jev goal choices to executor sub-actions and observations; retain errors/manual input.
- [ ] Save versions and known timing without credentials; local sequences are not world ticks.
- [ ] Disk failures stop agent control with recording_error; imports validate schema, sizes and paths.

**Verification:** Recording fixtures covering corruption, write failures and credential redaction.
**Dependencies:** 8. **Size:** M.
**Files:** `src/engine/events.rs`, `src/engine/recording.rs`, `tests/recording.rs`, `src/engine/session.rs`.

### Checkpoint C

- [ ] Late responses and failures cannot leave inputs active.
- [ ] Record the entire decision sequence and every outcome.
- [ ] Subagent review checks real-time races and the strength of evidence.

## Task 10: Telemetry replay and timeline

**Description:** Add Runs/Replay and a synchronized decision inspector.
**Acceptance:**

- [ ] Replay makes zero model calls, Minecraft connections or game actions.
- [ ] Replay displays recorded telemetry without promising world restoration or deterministic replay.
- [ ] Distinguish live/history and selected/executed/rejected actions; derive requests, known usage and end-to-end/API p50/p95 from events; display active profile limits and executor provenance.

**Verification:** Replay fixtures and record → restart → import → replay flow; verify zero external calls.
**Dependencies:** 9. **Size:** M.
**Files:** `src/engine/replay.rs`, `tests/replay.rs`, selected replay view, selected inspector view.

## Task 11: Robust local UI flow

**Description:** Verify the full operator flow and connection/error boundaries in the chosen UI framework.
**Acceptance:**

- [ ] Start, pause during requests, takeover, stop and replay work; fixture/live modes are clearly labelled.
- [ ] Timeout, 429 and disconnect produce useful consistent statuses; credentials do not leak through UI/logs/exports.
- [ ] Keyboard operation and suitable window sizes work; any web transport uses local binding and origin/payload validation.

**Verification:** Relevant UI integration tests, visual/keyboard inspection and focused boundary tests.
**Dependencies:** 10. **Size:** M.
**Files:** selected UI integration test, UI styles/layout, UI transport, `src/main.rs`.

## Task 12: Live evidence and handoff

**Description:** Archive a bounded real Minecraft run with Jev, document Windows startup and define the FS25 phase.
**Acceptance:**

- [ ] Real Jev inference controls Minecraft on a compatible separate server; missing key/server/authentication leaves the criterion open.
- [ ] README explains reusable setup/profiles, control model, pause/step and telemetry replay without determinism guarantees; local desktop first, without SaaS/accounts.
- [ ] Verification report and independent review link actual run IDs and identify the FS25 mod bridge as the next experiment.

**Verification:** Budget-limited live smoke test, relevant combined tests/build/check and comparison of actual events with the report.
**Dependencies:** 11. **Size:** S–M.
**Files:** `README.md`, `docs/minecraft-setup.md`, `tasks/verification.md`, `tasks/todo.md`.

### Checkpoint D

- [ ] Criteria have concrete evidence; mocks are not described as live verification.
- [ ] Document a real Minecraft/Jev run before calling the MVP live-verified.
- [ ] The next phase is FS25 in a separate test save, with its own integration experiment.
