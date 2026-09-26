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

### Live model-selected navigation

One later recorded session is the first run in which Jev selected navigation goals and the engine measured arrival. It ran the desktop app in live mode against an operator-authorized loopback test server, with the bot in Creative in one converted test world while a separate player watched in a normal client. Every value below was read from the exported recording and then cross-checked against server-side position reads taken while the session ran.

| Quantity | Recorded value |
|---|---|
| Session length / model requests | 131.5 s / 30 (request budget reached, no failure) |
| Model answers | 30 (one per request). Each answer is recorded on two events — its `decision` and the `dispatched` event carrying the same decision — so the recording holds 60 decision-bearing events: `wait` 27 answers, waypoint candidates 3. |
| Probability of the chosen waypoint | 0.41–0.43 |
| Adapter-accepted navigation actions | 3 |
| Arrival verdicts | 30 (target-less `wait` goals also record one); 3 with `arrived` true |
| Measured arrival distances | 0.1451 m, 0.0281 m, 0.0945 m against tolerance 0.6 m |
| Elapsed vs allowed duration | 613–723 ms of 3460 ms |
| Straight-line displacement | 6.2887 blocks (6.4600 blocks summed over consecutive observations) |
| Health | 18.0, unchanged across all 153 observations |
| Manual takeover records | 0 |
| Answer latency | 266–860 ms over the 30 answers as recorded (`latency_ms`); p50 291 ms and p95 341 ms by the engine's own percentile rule over those answers. The live latency panel shows a rolling 64-sample window of request ages, which measures something narrower than the whole session and is not what the recording stores. |
| Model / tokens | `jev-1.13.0`; 422136 input and 7230 output tokens over the 30 answers (60 decision-bearing events) |

Two limits are part of this evidence rather than separate from it:

- Jev chose `wait` in 27 of the 30 answers (54 of the 60 decision-bearing events) and picked navigation only where a waypoint candidate narrowly overtook it (0.41–0.43). The app offers the operator no way to express a goal, so what gets attempted comes from the model's own reading of the request text and the candidate list. One session shows that a model-selected arrival is reachable; it does not show that navigation is dependable.
- An earlier live session ended safely *without acting*: the provider returned a distribution that failed the probabilities-sum-to-one check (±0.001) and the engine stopped the session with a visible error instead of normalising a doubtful answer. Whether a near-normalised distribution should end the session, be refused and retried inside the budget, or produce an error that leaves the session usable is an open question, not a settled one.

## Attribution of recorded actions

A recording states who chose each action, and the same rule is applied wherever a recording is read. `origin::event_origin` in the library classifies an event from fields it already carries — the recorded takeover message for manual input, `model == "offline-fixture"` for the offline fixture, a model decision on a decision, dispatch, action or executor event for a model-selected goal — and `ActionOrigin::label` gives the `MANUAL`, `FIXTURE` and `JEV-SELECTED` labels the timeline shows. The offline `recording-report --chain` view prints those labels for the events it correlates with each arrival, so the live session above can be read as model-selected navigation without opening the GUI.

What it proves: the separation is one implementation shared by the timeline and the offline report, `tests/action_origin.rs` asserts all three origins and their exact labels against it (including that nothing in a fixture session is labelled as operator input and nothing in a takeover session is labelled as a model or fixture choice), and the regenerable screenshot shows the `FIXTURE` label with the arrival line intact.

What it does not prove: the rule reads the recorded message for manual input, so a future change that rewords a takeover message would move those rows out of `MANUAL`; the test in `tests/action_origin.rs` pins the current wording. Nothing here classifies an action the engine never records at all, and a recording made before the helper existed carries the same fields, so it is labelled by the same rule.

## Sanitizing a recording for publication

The raw live recording stays private because it names the operator's bot and world. `cargo run --bin recording-sanitize -- <input.json> <output.json> --redact from=to …` produces a copy that can be reviewed for publication: it validates the input through the engine's own loader, replaces each named identifier in every string value, validates the result again and refuses to write anything the engine would no longer accept. It does not guess identifiers, and it never rewrites numbers.

Verified on the session above: replacing the bot name and the world name (1 and 153 occurrences) produced a 1.44 MB copy of the 3.51 MB source that still validates and still reports 154 events, 30 requests, 30 model answers recorded on 60 decision-bearing events, 30 accepted actions and the same three arrived verdicts at 0.1451 m, 0.0281 m and 0.0945 m against the 0.60 m tolerance. The refusal paths were exercised too: no `--redact` pair is refused, an input that is not a recording is refused, and a pair that would turn a live decision into the offline fixture's (`--redact jev-1.13.0=offline-fixture`) is refused with *Offline fixture decision in a live recording* and writes no output file.

What it proves: the numbers in this report survive a transformation that removes the operator's identifiers, so the evidence can be published without editing the evidence.

What it does not prove: the sanitized copy is only as private as the pair list the operator supplies — anything a pair did not name stays in the file, which is why the tool prints one line per replacement and tells the reader to review the output. Publishing it remains a separate operator decision.

## Limits

Reliable model-selected navigation remains unproven even though three measured arrivals were recorded once; the session above is the whole of that evidence. Mining, building, combat/escape behavior, game discovery and a second game adapter are not implemented. Runtime facts, offline fixtures and planned features must be reported separately. Private account inventories, world backups and machine-specific operational notes are excluded from publication.

An independent read-only review examined the source and acceptance evidence. It identified manual-target binding, acceptance timing, acknowledgement rollback and replay-history gaps; fixes received regression coverage. The final re-review returned no remaining findings. This review does not establish reliable autonomous gameplay or replace broader field testing.

The engine does not automatically change server configuration, player accounts or existing world files. Separate operator-authorized world conversion work was outside the application.

### Reach of one goal, and what a distant position offers (measured 2026-09-26)

`candidates()` in `src/engine.rs` builds the legal action set from observed blocks whose name starts with `waypoint:`, within 12 blocks of the bot, at most 8 of them, always plus `wait`. Every candidate it builds carries `duration_ms` equal to the latency-derived goal horizon (`goal_ms`, clamped to 1–10 s), so one bounded navigation goal closes roughly four blocks at the fixture's 2 blocks per second, and an arrival is only reachable for a target near the bot.

Measured live: with the bot standing far outside the waypoint grid in the test world, the observation still held 196 observed blocks but **no `waypoint:` landmark within 12 blocks**, so the request carried exactly one candidate, `wait`, and the engine recorded that itself in the request event: `TypeSafe request 1 for observation 210; no navigate candidate: 0 observed waypoints`. Recording: kept privately, not published. The operator's position was read at the time without its world, so this entry claims no distance between the operator and the bot; what is claimed is the bot's own observation and the request the engine built from it.

The consequence is structural rather than a matter of tuning. The engine cannot currently be asked for a distant target: no field, flag or adapter path states one (`navigation-demo` accepts `--port`, `--bot`, `--connect-seconds`, `--goal-seconds`, `--session-seconds`, `--keep-connected` and `--legacy-forwarding` only), and a distant candidate would inherit the same short duration. A long journey would therefore have to be a chain of roughly four-block goals, each behind a model request in which the model chose navigation — measured at 3 of 30 answers in the recorded session.

What it does not prove: the measurement does not show the model refusing a distant goal, only that it was never offered one, and it measures no pathfinding quality because no navigation goal ran at that location. The same run is also where the safety paths were exercised: the teleport placed the bot in water in a world that enforces survival, so the fake player drowned and respawned at the server's join world, and the engine local-stopped nine times on decreasing health without spending a model request.

### Autonomous run without an objective (measured 2026-09-26)

A live session ran with no way to state a goal: the engine offered only the waypoints it observed nearby plus `wait`. Across 9 TypeSafe requests (53 events; 18 decision-bearing events, since each answer is recorded on both its `decision` and its `dispatched` event) the model chose `wait` all 9 times. `wait`'s probability ranged 0.50–0.79; the strongest competing candidate, always the same observed waypoint, never exceeded 0.26. All 9 actions were accepted, 8 produced an expiry verdict and none arrived, because none had a target; the session's time budget ended the run before the last one resolved. The bot's position was identical at every one of the 9 decisions; the only displacement in the recording happened during a respawn before the `Start` control, and health stayed at 20.0 throughout. The operator's `Pause` and `Stop` appear as control events, not as actions. Model `jev-1.13.0`; answer latency 263–428 ms, p50 290 ms. Recording kept privately, not published; re-read with `recording-report --json` and a direct read of the JSON.

What it proves: with only observed waypoints and `wait` on offer, and nothing that names a purpose, the model's own preference decides what is attempted, and in this session that preference was to wait every time.

What it does not prove: it does not show that a stated goal would change the answer — this recording's settings block carries only `mode`, `max_requests` and `max_seconds` and predates `objective`, `request_interval_ms` and `safety_reflex`. Those three settings were added afterwards because of this measurement and have offline tests only; see the next section.

### Session objective and safety reflex (added 2026-09-26, offline evidence only)

Three session settings were added after the autonomous run above showed that the engine had no way to be given a goal: `Settings::objective` (`--objective`), `Settings::request_interval_ms` (`--request-interval-seconds`) and `Settings::safety_reflex` (`--safety-reflex`), plus threat-aware `flee_*` candidates from `src/survival.rs`. All default to empty or off, and `provider::request_body` is tested to produce the earlier request body byte-for-byte in that case.

What is tested offline: an objective travels as its own `objective` field beside `observation` in the request state and is named in the instructions, and an empty one leaves both untouched (`src/provider.rs` tests); the CLI and the engine reject an objective over 400 characters or containing control characters; a hostile entity within 12 blocks adds only the observed waypoints that put at least 1.5 blocks more between bot and threat, best first and at most three, and passive neighbours add none (`tests/survival_candidates.rs`, `src/engine.rs` tests); the 38 hostile kinds match both the adapter's `ZombifiedPiglin` spelling and `zombified_piglin`; the reflex dispatches a flight goal with no model request, sends `Stop` before `Execute`, records a `reflex` event naming the threat, never preempts a navigation goal, stays off until enabled, and the pacing gate holds the next request until the interval has passed (`src/engine.rs` tests). `origin::event_origin` labels the `reflex` event and the reflex action's dispatch and acceptance rows `SAFETY-REFLEX`, distinct from `JEV-SELECTED`, `FIXTURE` and `MANUAL` (`tests/action_origin.rs`), and `recording-report` counts reflex actions separately from model answers. The headless night-run harness (`harness::run_session`, `session-run`) is tested in `tests/session_harness.rs`: a fixture session with an objective and the reflex enabled ends on its request budget with exit 0, exports a recording that loads through `recording::load` and carries both settings, and produces a summary whose counts and displacement are derived from that recording's events; nine invalid option cases and seven invalid command lines are refused with one line before anything connects, live mode without a key is refused the same way, and the failure exit codes are distinct.

What it does not prove: no live session has run with an objective, with pacing or with the reflex enabled, and no live mob has been observed by the hostile filter. Whether a stated objective changes what Jev picks, whether a flight goal outruns a real mob, and whether the 6-block reflex radius is early enough are unmeasured. The reflex is also the only path where an action's goal comes from neither the model nor the operator, which is why it is opt-in and labelled.

### Arrival verdict scope

Every bounded navigation action that ends writes an `ArrivalVerdict` onto its `executor` event, so a finished local navigation attempt is a machine-checkable record instead of only a log sentence. The verdict carries the bound target, the measured distance in blocks when a connected observation was available, the tolerance in effect (`ARRIVAL_TOLERANCE_M = 0.6`), the allowed duration, the observed elapsed time, and `arrived`.

What it proves offline: the fields are derived by the engine from observed world state, and a recorded verdict can be inspected in the exported JSON and in offline replay without any provider or Minecraft call. Arrival is therefore decidable after the fact from the recording. A verdict with `arrived` false and a measured distance above the tolerance documents that the bounded goal duration expired first; a verdict without `measured_distance_m` documents that no connected observation was available to measure, and is not a distance measurement.

The fixture's geometry is explicit so that both outcomes are constructible offline. `fixture::spawn()` uses `fixture::DISTANT_WAYPOINT` (5.657 blocks), which the fixture's 2 blocks per second telemetry cannot close inside a 2000 ms demo goal, so a default fixture run demonstrates the expiry path. `fixture::spawn_with_waypoint` with `fixture::REACHABLE_WAYPOINT` (1.5 blocks) records a genuine arrival inside the same bound, and `Settings::fixture_waypoint` selects between them for a session, so an operator, a test or a recording states which geometry produced a verdict. Geometry therefore decides which outcome a fixture run shows, and neither geometry measures Minecraft pathfinding.

What it does not prove: a verdict states only that the observed distance to the bound target entered, or failed to enter, the tolerance within the bounded duration. It does not show that Jev chose a sensible target, that the route was safe or permitted, or that the in-game task succeeded, and it does not replace the separately reported measured physical displacement.

`src/bin/navigation-demo.rs` remains a way to produce such a record, not evidence by itself: it drives the same engine path headlessly for one model-selected goal and prints the correlated chain. The live record reported above was produced through the desktop app's continuous mode instead, which is why it contains several goals and thirty verdicts rather than one.

An exported recording can be checked without the GUI: `cargo run --bin recording-report -- <path> [--chain] [--require-arrival]` re-validates it with the same loader the app uses and reports the counts and every arrived verdict, opening no connection and writing nothing. `--chain` prints the request, decision, dispatch and accepted action that led to each arrival, correlated by order, which is how the live session above was re-read: each arriving goal shows a TypeSafe request, its decision, the dispatch and the adapter's acceptance before the verdict, and every navigation action in that session is labelled `Jev selected goal` by the engine itself.

## Acceptance map

| MVP requirement | Evidence |
|---|---|
| Native English Rust UI and session controls | Native rendered screenshots, including a regenerable capture of the arrival verdict with its measured numbers; UI and engine regression tests; documented Cargo launch command. |
| Validated live Jev integration | Production API responses on actual Minecraft observations; malformed-response, timeout and missing-key tests. |
| Real bounded Minecraft adapter | Pinned Azalea connection, real observations, acknowledged wait, separately measured local navigation/stop, and model-selected navigation with recorded arrival verdicts. |
| Cancellation, stale-answer rejection and replay | Generation/world/deadline regressions, cancelled acknowledgement rollback, immutable replay and full-history tests. |
| Automated checks and runtime evidence | 115 tests in the working tree (35 unit inside `src`, 80 integration under `tests/`) against 69 at the reviewed revision, including the navigation, fixture-geometry, recording-compatibility, action-origin, survival-candidate and session-harness tests added since; compilation and warning-free Clippy passed; fixture and live runtime evidence are distinguished above. |
| Independent review and handoff | Independent final verdict: no remaining findings; README, architecture, screenshots, credits and public source published. |
| Adaptive latency and reusable configuration | Bounded low/high-latency tests, current manual-choice refresh and settings without personal paths. |

The original planning checklist in `todo.md` is retained as planning history; this report is the current implementation evidence. Future adapters and reliable autonomous task completion remain outside this MVP's demonstrated capabilities.
