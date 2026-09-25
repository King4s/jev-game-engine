# Decisions with 300 ms network latency

The user reported roughly 300 ms to the Jev server. This is not a measurement of complete observation-to-execution latency: inference, queuing and transport can add time. The design must work without per-frame model decisions.

## Responsibilities

- Jev selects a bounded goal or macro action from current candidates.
- Rust executes it locally over several seconds. Movement, deadlines and cancellation never wait for Jev.
- Request a new decision after goal completion/failure or a relevant change, with at most one request active per session.
- Local cancellation is not a Jev choice. UI and logs distinguish model decisions, local execution and local interrupts.
- Offline fixtures test the engine, but do not replace an owned game or prove live capability.

## Macro contract

Each candidate has an action ID, target ID, preconditions, maximum duration, completion predicate and permitted side effects. Recheck the chosen target before execution. Expiry, disconnect, stop or generation changes cancel execution. Discard responses made irrelevant while waiting and observe again.

Do not use a universal model-confidence threshold as a physical safety check. Local code checks concrete conditions and records why it vetoes or cancels an action. Any safe retreat must be an explicitly named local policy, not hidden model behavior.

The first Minecraft goal can be navigation to an observed nearby waypoint with automatic mining disabled. Resource collection, building and combat are separate future skills. The local client/pathfinder handles continuous player movement.

## Measurements and UI

Display API latency and observation age separately from goal duration. Show the active goal, progress, executing component, cancellation reason and next decision point. Log calls per completed goal, changes during inference and rejected answers. Use measured end-to-end p50/p95, not ping alone, to select the decision horizon.

## Selected game and initial policy

The user chose Minecraft with goals and local movement. Other users with low or high response times must also be able to use the product. The initial policy uses the latest 64 measured requests: goal duration = 4 × p95, bounded to 2–10 seconds; response age limit = 2 × p95, bounded to 1.5–5 seconds. Before measurements exist, use a 2-second goal duration and a **5-second initial response age limit**. An internal estimate must never be displayed as measured latency. Generation changes and local stops always override time horizons. Evaluate these initial limits using live data.

## Historical Rust Minecraft feasibility study

A subagent verified a possible native integration through [Azalea](https://github.com/azalea-rs/azalea), commit `b65fa8cf1bb957976cefa926b9b500d44767d806` (Minecraft 26.2). Its [manifest](https://github.com/azalea-rs/azalea/blob/b65fa8cf1bb957976cefa926b9b500d44767d806/Cargo.toml) and toolchain required a compatibility build before pinning a nightly date. Local 26.2 JAR files were found; no server was started during that inspection. The implementation now pins nightly-2026-09-07; this paragraph records the earlier research stage.

API candidates were ClientBuilder/start, Event::Tick, position/health/inventory/world, start_goto_with_opts with allow_mining(false), force_stop_pathfinding and walk(None). [Current API documentation](https://azalea.rs/azalea/struct.Client.html). These were source-verified findings, not a running adapter at the time. Azalea is a separate client and does not render the original game's view.
