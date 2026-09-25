# Implementation plan: AI Game Engine with Jev

Date: September 25, 2026. Current user requirements supersede the earlier TypeScript/2048 scope. **Build the engine in Rust. Minecraft Java is the first game; Farming Simulator 25 is the next phase. Neither 2048 nor Snake is a product target.** The user authorized subagents and the Jev loop.

## Purpose

A local AI game engine lets Jev select actions while the UI displays observations, candidates, model responses, executed actions and observed outcomes. The first Minecraft scenario is bounded navigation in a separate local test world. One active session is sufficient initially.

The development Jev loop selects implementation work; in-game Jev selects gameplay actions. Keep their documentation, logs and budgets separate. See [research.md](research.md), [game-library.md](game-library.md) and [todo.md](todo.md). Older research supplies context, not the current language choice.

## Decisions

| Area | Current direction |
|---|---|
| Engine | Rust owns sessions, provider requests, deadlines, validation and recordings. |
| First game | Minecraft Java from the user's library. |
| Next phase | Farming Simulator 25, following a separate mod-bridge investigation. |
| UI | Native Rust with egui/eframe. |
| Minecraft integration | Rust/Azalea pinned as described in latency-design.md; test server compatibility early. |
| Jev | Documented TypeSafe HTTP integration suitable for Rust; verify the current endpoint and contract before implementation. |
| Storage | Versioned manifest and sequential events; no MVP database required. |

Pin compatible crate/runtime versions against official sources. A separate Java client supplies the original 3D game view; the engine UI shows bot telemetry and a local map without promising integrated 3D streaming.

## Engine and adapter

The engine owns session status, generation, provider requests, budgets, deadlines and the event log. The adapter owns observations, candidates, commands, preconditions and observed outcomes. Declare capabilities such as worldPause, reset, restore and deterministicReplay instead of assuming an external world supports them.

Observations carry runId, generation, observationId, timestamp, goal, game facts and concise history. Candidates have stable action IDs and bounded duration. Validate model responses against their observation and current preconditions before execution. Keep Minecraft-specific branches out of the engine/provider.

The first adapter exposes position, health, inventory, connection status and relevant nearby blocks/entities where supported. Unknown areas and missing data remain unknown. Initial actions are navigation and waiting; digging, placement and combat are outside MVP requirements.

A connected bot is a **separate player**, not takeover of the user's existing player. Explain alternative control models before selecting them. Server version, protocol and authentication must match; local Java files alone prove neither a compatible server nor access.

Do not modify, move or delete existing saves. A test server uses a separate directory and world. Handle downloads, EULA acceptance and authentication explicitly according to concrete requirements. Missing server access blocks live evidence, not development with clearly labelled fixtures.

## Timing, stopping and Jev

Minecraft continues during inference and agent pause. Pause stops new requests and releases active inputs; it does not freeze the server. One decision means one bounded agent action, not a deterministic world tick. Stop cancels active work, releases inputs and ends the session.

Allow at most one provider request per session. Generation, observation age and expiry reject responses after pause/stop/controller changes or from stale observations. Action duration limits release inputs even when inference is slow. Local event sequence numbers are not authoritative world ticks.

Jev selects current goal/macro candidates. Label the executor's movements with their own provenance rather than presenting each as a model choice. Show only confidence, probabilities and usage actually returned by the API; missing values are unknown. Never invent a model explanation or probability of winning. Keep credentials in the Rust process and out of logs, exports and errors.

Timeouts, provider failures, disconnects and exhausted budgets produce concrete statuses and release inputs. Manual bot control invalidates pending responses and marks the run mixed-control. A test controller is explicit, never a hidden fallback.

## UI and replay

Live, Runs and Replay identify game, controller, connection and data source: live, fixture or replay. Live prioritizes observed world state and the latest decision. Correlate observation → request → response → applied/rejected action → result. Clearly distinguish historical selections from current bot state.

Minecraft replay means **playback of recorded telemetry and decisions**. It makes no server connections, executes no actions, restores no world and promises neither deterministic simulation nor complete world state. The same seed does not guarantee an identical live run. Preserve recorded errors, manual input and losses.

Display requests, known usage, response latency, observation age, rejected responses and goal status. Cost requires a documented pricing basis. Require keyboard operation and clear status text regardless of UI framework.

## Success criteria

- A Rust engine controls a real Minecraft session with Jev through a verified integration.
- Actions are traceable to observations, controllers and actual application.
- Stop, pause and errors release input; late responses cannot control later sessions.
- Recorded telemetry can be inspected without Minecraft connections or model calls.
- Report offline fixtures, real Minecraft connections and real Jev inference separately.

Missing keys, compatible servers, licenses or authentication are explicit unresolved prerequisites. Passing mocks does not make the MVP live-verified. The goal is a functioning visible control loop, not guaranteed victory.

## Build order and delegation

The 12 tasks in todo.md begin with integration/UI decisions and an early connection probe. Then deliver the shared engine, visible Jev decisions, controlled real-time execution and recording. Finish with replay, operator flow and documented live evidence.

Subagents may research Minecraft, UI and the provider in parallel. Coordinate shared contracts and assign bounded file ownership. The Jev loop selects the next task; independent review checks changes and acceptance. Checkpoints require evidence rather than routine repeated user confirmation.

After MVP, investigate FS25's documented Lua mod bridge and Rust transport in a separate test save. Minecraft results do not prove FS25 compatibility. ETS2 and shapez remain later candidates from the user's library.

## Approved approach to 300 ms latency

The user chose Minecraft with goals and local movement. Jev selects goals/macros lasting several seconds; Rust handles movement, progress and immediate cancellation without waiting for inference. Replan on completion, blockage, meaningful changes or expiry. Distinguish Jev's goal selection from executor sub-actions throughout the UI and logs. Jev does not provide reflex control. See [latency-design.md](latency-design.md).

## Reusable local product and adaptive latency

Other people must be able to use the product, with Minecraft as the first demanding integration. Settings, profiles and exports must avoid hardcoded personal paths. Deliver a local desktop app first; SaaS, user accounts and deployment are outside MVP.

Measure observation-to-action and API p50/p95. Adapt goal horizons and replan intervals within explicit configured limits using observed latency, supporting both short and long response times. Freshness limits and immediate local stopping remain mandatory. Profiles make limits reproducible; logs/UI show active bounds, changes and executor provenance. Verify with injected low/high latency. The reported 300 ms is a user observation, not a universal constant.

## Additional product research

- [OptiScaler-GUI scanner research](optiscaler-research.md): prefer independent Rust discovery. Keep installed, owned, subscription-accessible and adapter-supported statuses separate. Graphics DLL installation is not part of an AI adapter.
- [Additional Jev roles](jev-opportunities.md): prioritize recovery strategy selection and goal interpretation for evaluation against rules. These are not automatically added to MVP acceptance. Local stopping, scanning and latency calculations remain Rust responsibilities.

## Reuse boundary refinement (2026-09-25)

The user reaffirmed that this is an engine for many games. Keep model decisions, latency policy, budgets, cancellation, recording and timeline reusable. Game-specific observation collection, legal candidate generation, movement and lifecycle checks belong to adapters. Minecraft is the first proving adapter, not the permanent product boundary.

The current MVP still has Minecraft-specific candidate generation and presentation choices. Treat these as explicit prototype debt; do not claim a finished plugin API. Extract a stable capability/adapter factory contract when a second real game supplies concrete requirements, rather than inventing a large framework now. Reuse maintained game libraries and verified patterns when integration and testing cost less than rebuilding them.
