# Architecture

Jev Game Engine separates goal selection from local execution. It is an early desktop prototype, with Minecraft Java 26.2 as the first live integration and a synthetic fixture for local engine tests.

## Components

| Component | Responsibility |
|---|---|
| `src/ui.rs` | Native egui presentation, controls, map, decision inspection and replay selection. |
| `src/engine.rs` | Session lifecycle, one pending model request, freshness checks, budgets and events. |
| `src/provider.rs` | TypeSafe HTTP requests and response validation. |
| `src/latency.rs` | Bounded rolling latency policy, independent of model judgments. |
| `src/adapter.rs` | Action requests, world identity, acknowledgement and channels. |
| `src/minecraft.rs` | Azalea connection, world observation, endpoint guards and local movement. |
| `src/fixture.rs` | Synthetic observations and movement without Minecraft or Jev. |
| `src/model.rs` | Shared types: settings, observations, candidates, events and the arrival verdict with its tolerance. |
| `src/origin.rs` | Who chose an action — model-selected, fixture or manual — derived from fields a recorded event carries, so the timeline and the offline report label the same events the same way. |
| `src/recording.rs` | Versioned JSON persistence, validation and import. |

The UI communicates with a background Rust session through commands and snapshots. Minecraft has a dedicated local Tokio/Azalea execution context. Inference does not run on the rendering thread.

## Decision lifecycle

1. Obtain an observation with position, health, dimension and world epoch.
2. Construct bounded candidates from supported local capabilities: observed waypoints within reach, up to three `flee_*` goals away from the nearest hostile mob when one is within 12 blocks, and `wait`.
3. Request a typed Jev choice, retaining the observation and timing limits used. An operator-stated session objective, when present, is carried as its own `objective` field in the request state and named in the instructions; the engine never derives an action from it. Continuous mode may be paced by a minimum interval between requests.
4. Validate the response against session generation, age and current prerequisites.
5. Dispatch an action carrying its expected dimension, world epoch and absolute acceptance deadline.
6. The adapter checks the current world and local guards, then acknowledges acceptance with its timestamp or reports rejection. Failed acknowledgement delivery cancels queued execution.
7. Record acceptance separately from dispatch; observe progress until completion, expiry or cancellation.
8. A bounded navigation action that ends writes an `ArrivalVerdict` onto its `executor` event: the target it was bound to, the distance measured from that event's own observation, the tolerance, the duration and the elapsed time. A finished attempt is therefore a machine-checkable record instead of an inference from "the action was accepted". Recordings written before the verdict existed still load, because the field is optional.

An accepted command is not proven movement or goal completion. Goal duration starts at adapter acceptance, independently of when the engine polls the acknowledgement. Manual controls show refreshed candidates bound to their observed world; they use the same adapter guards and retain their manual origin.

## Cancellation and world changes

Pause, stop, reset and controller changes invalidate pending model work. Local stop never waits for another model answer. The adapter cancels pathfinding/movement on stop, expiry, stale local ticks and relevant health/world transitions. With the safety reflex enabled, a flight goal keeps moving under damage, and a model answer overtaken by the hit (health dropped, or the bot was knocked more than 1.5 blocks, since the observation it answered) is dropped as `rejected` and the session asks again without stopping the run; so is an action the adapter refused because health dropped before it was accepted.

World identity includes an epoch: respawning or returning to the same dimension can invalidate a goal even when the dimension name is unchanged. The adapter validates identity at execution time. Deferred actions allow a local stop update before a new pathfinding command; acknowledgement prevents queue delivery from being reported as execution.

Stop disconnects; pause leaves the world running. The opt-in safety reflex is the one exception to "only the model or the operator chooses": when a hostile mob is within 6 blocks, or within 16 blocks for shooters (skeleton, stray, bogged, pillager), and no navigation goal is in flight, the engine stops in-flight work (the local stop a pause performs, without ending the run mode), records a `reflex` event, and dispatches one bounded flight goal built from the current observation with the `SAFETY-REFLEX` origin, spending no model request. The flight prefers cells that run across the line of fire, at least 3 blocks away from a shooter. When the reflex is active, a hit during flight does not stop movement; the adapter keeps moving and the session records a `hurt` event. A model answer overtaken while the bot waits for it (a hit lowered health, or knockback moved the bot more than 1.5 blocks since the observation it answered) is dropped with a `rejected` event and the session requests again. It never preempts a navigation goal, is rate-limited to one action per 1.5 s, and is off unless the session enables it. Reset and replay do not change saves. The adapter targets an authorized loopback connection and a separate offline bot identity. Microsoft authentication and arbitrary remote-server configuration are outside its current scope.

## Latency

The policy retains 64 request-latency samples. Goal duration is 4 × p95 clamped to 2–10 seconds; maximum response age is 2 × p95 clamped to 1.5–5 seconds. Warmup permits a five-second first response. Limits captured for an issued request are not retroactively relaxed by new samples.

This is deterministic Rust logic. Measurements describe request latency, not end-to-end task performance. Controlled low/high-latency tests establish bounds; live trials must establish benefits.

## Observation and replay

The map samples observed blocks, entities and waypoints. Unknown terrain remains unknown; this is neither a complete world reconstruction nor a 3D renderer. Unavailable values must not be fabricated.

Recordings contain settings and event-associated observations, candidates and decisions. The timeline exposes the complete bounded recording through pagination. Replay reads records without opening an adapter or requesting inference. It cannot restore Minecraft state, deterministically resimulate the server or evaluate alternative actions.

## Additional games

Reusable control across games is the goal, but only Minecraft and the offline fixture exist today. The engine still contains adapter-specific candidate construction; fully separating game policies remains work for the second live adapter. Shared command types alone do not establish arbitrary game support.

A new adapter needs explicit observations, goals, action bounds, cancellation semantics and outcomes. World pause, deterministic stepping and restore cannot be assumed. Farming Simulator 25 requires a separately tested mod bridge; Minecraft results do not establish compatibility.

Discovery is separate from control. Installation, ownership, subscription entitlement, launchability and adapter readiness are independent facts.

## Offline tools

Four small binaries read, prepare or drive a session outside the desktop UI:

| Tool | Responsibility |
|---|---|
| `recording-report` | Reads one exported recording through the same validator the app uses and prints event, request, answer, decision-bearing-event, choice and arrival-verdict counts. `--chain` adds the request, decision, dispatch and accepted action behind each arrival, correlated by order; `--require-arrival` exits 3 when no arrival was recorded; `--json` prints the summary for a script. It opens no connection and writes nothing. |
| `recording-sanitize` | Validates a recording, replaces exactly the identifiers named on the command line in every string value, validates the result again and refuses to write anything the engine would no longer accept. It never rewrites numbers, so measured distances and deadlines survive publication unchanged. |
| `navigation-demo` | Operator harness for one model-selected bounded navigation goal on a loopback server: one observation, one decision, one dispatch, the engine's own arrival verdict and the local Stop, printed as the correlated chain. It never chooses a goal itself. |
| `session-run` | Headless multi-goal session through the real engine (`harness::run_session`): Connect, Start, wait for the engine to end the session on its own budget, Export, summarise. Takes the objective, pacing, safety-reflex and budget settings, moves the exported recording to `--export`, and exits 0 only for a budget end with the bot still connected; each failure has its own code. `--fixture` runs the identical loop against the offline fixture, which is how it is tested. It never chooses a goal itself. |

## Evidence boundaries

Unit/integration tests cover local contracts and failure cases. Headless egui tests prove layout generation, not GPU rendering. Native screenshots prove a captured UI state, not autonomous game success. API probes, live connections and measured movement each prove only their specific path. Report them separately. A recorded arrival verdict proves that the engine measured the bot inside its tolerance at the end of that one bounded goal; it does not prove the route was sensible, that the model had a good reason to choose it, or that the same goal would be reached twice. The offline fixture arriving proves the engine's own arithmetic and stopping, not Minecraft pathfinding. The session objective, the pacing gate, the flight candidates and the safety reflex are covered by offline unit and integration tests against hand-written observations; none of them has been exercised against a live mob, so no claim about survival follows from their existence.
