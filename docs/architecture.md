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
| `src/recording.rs` | Versioned JSON persistence, validation and import. |

The UI communicates with a background Rust session through commands and snapshots. Minecraft has a dedicated local Tokio/Azalea execution context. Inference does not run on the rendering thread.

## Decision lifecycle

1. Obtain an observation with position, health, dimension and world epoch.
2. Construct bounded candidates from supported local capabilities.
3. Request a typed Jev choice, retaining the observation and timing limits used.
4. Validate the response against session generation, age and current prerequisites.
5. Dispatch an action carrying its expected dimension, world epoch and absolute acceptance deadline.
6. The adapter checks the current world and local guards, then acknowledges acceptance with its timestamp or reports rejection. Failed acknowledgement delivery cancels queued execution.
7. Record acceptance separately from dispatch; observe progress until completion, expiry or cancellation.

An accepted command is not proven movement or goal completion. Goal duration starts at adapter acceptance, independently of when the engine polls the acknowledgement. Manual controls show refreshed candidates bound to their observed world; they use the same adapter guards and retain their manual origin.

## Cancellation and world changes

Pause, stop, reset and controller changes invalidate pending model work. Local stop never waits for another model answer. The adapter cancels pathfinding/movement on stop, expiry, stale local ticks and relevant health/world transitions.

World identity includes an epoch: respawning or returning to the same dimension can invalidate a goal even when the dimension name is unchanged. The adapter validates identity at execution time. Deferred actions allow a local stop update before a new pathfinding command; acknowledgement prevents queue delivery from being reported as execution.

Stop disconnects; pause leaves the world running. Reset and replay do not change saves. The adapter targets an authorized loopback connection and a separate offline bot identity. Microsoft authentication and arbitrary remote-server configuration are outside its current scope.

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

## Evidence boundaries

Unit/integration tests cover local contracts and failure cases. Headless egui tests prove layout generation, not GPU rendering. Native screenshots prove a captured UI state, not autonomous game success. API probes, live connections and measured movement each prove only their specific path. Report them separately.
