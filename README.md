# Jev Game Engine

**See what an AI game agent observes, chooses and actually does.**

An early Rust desktop prototype for controlling games through bounded goals. [TypeSafe Jev](https://docs.typesafe.ai/) selects a goal; a local executor handles movement, validation and stopping. The aim is a reusable engine for multiple games. **Minecraft Java 26.2 is the first implemented integration.**

![Native desktop UI with explicitly labeled offline fixture data](docs/images/native-ui.png)

*The screenshot uses synthetic offline data. It is not evidence of live Minecraft or Jev control.*

![The dedicated bot inside a building, viewed from the normal Minecraft client](docs/images/minecraft-bot.png)

*User-provided screenshot from a real Minecraft session. This is the normal game's view of the separate bot, not a 3D feed rendered by the engine and not evidence that the bot built the structure.*

## Current capabilities

- Native egui/eframe controls, observed telemetry and a local X/Z map.
- Explicit offline fixture and separate live Minecraft bot connection.
- TypeSafe goal selection, visible candidates, returned probabilities and request timing.
- Bounded navigation, pause, stop, manual takeover and world-lifecycle guards.
- Decision trace, JSON export and offline telemetry replay.

This is a prototype, not a general autonomous player. Live connectivity and approximately **1.118 blocks of local manual movement** have been observed. That verifies a limited executor path, not reliable Jev navigation or arbitrary goal completion. Automated tests, fixture demos, API calls and game outcomes provide different evidence.

## Quick start

Install [Rust through rustup](https://rustup.rs/). Windows requires MSVC C++ build tools and the Windows SDK. `rust-toolchain.toml` pins `nightly-2026-09-07`; the first build downloads the toolchain and dependencies. No Node runtime is required.

```powershell
cargo run -- --fixture-preview
```

This opens the desktop app and connects the offline fixture without starting the agent. Select **One decision** or **Start agent** to exercise the synthetic controller. Fixture mode makes no TypeSafe requests and does not connect to Minecraft. Use `cargo run` to open without automatically connecting.

## Live Minecraft

The [pinned Azalea client](https://github.com/azalea-rs/azalea/tree/b65fa8cf1bb957976cefa926b9b500d44767d806) targets **Minecraft Java 26.2**. Bedrock and other protocol versions are unsupported.

Prepare an authorized compatible server and a separate test world. The adapter connects to `127.0.0.1` using a configured offline bot identity. Keep an offline-login server bound to loopback. The app does not install servers, accept their EULA, modify saves/accounts or perform Microsoft account login.

```powershell
cargo run -- --live-preview --port 25565 --bot JevBot
```

Inspect the connection and observations before starting the agent. The bot is a **separate player**. To watch it in 3D, join the same server/world using a normal Minecraft client and your own account. The engine map is sampled telemetry, not a video feed or complete world map.

An independently managed SSH tunnel can expose an authorized remote backend on loopback. Use `--legacy-forwarding` only when that backend explicitly requires it and authorizes the bot. The forwarded UUID derives from the configured bot name; arbitrary identity override is not supported. Close previous previews before reconnecting the same identity.

### TypeSafe key

Configure a key outside the repository before launching:

```powershell
$env:TYPESAFE_API_KEY_FILE = 'C:\private-config\typesafe-key'
cargo run -- --live-preview --port 25565 --bot JevBot
```

The file contains the API key. `TYPESAFE_API_KEY` is also supported and takes precedence. Do not commit keys or include them in shared artifacts. Live agent requests count toward configured request/time budgets. Missing keys, rejected responses and failed connections produce visible errors, without hidden fixture fallback. See the [TypeSafe HTTP API](https://docs.typesafe.ai/api).

## Controls

| Control | Behavior |
|---|---|
| Connect | Applies settings and starts a new session. |
| Start agent | Repeatedly requests bounded goals within budgets. |
| One decision | Requests one goal, not one Minecraft tick. |
| Pause agent | Cancels pending work and stops movement; the world continues. |
| Stop | Stops local work and disconnects. |
| Reset session | Clears session state, not the Minecraft world. |
| Manual takeover | Revalidates a candidate and records manual control. |
| Export / Open replay | Saves or inspects telemetry without re-executing actions. |

The trace distinguishes **dispatched**, **accepted** and **rejected** actions. Acceptance does not prove arrival. Actions carry an observed world epoch, including same-dimension respawns, to invalidate obsolete work.

## Latency and recordings

Jev chooses goals over seconds; Rust handles local movement and stopping. Goal duration is 4 × rolling request-latency p95, bounded to 2–10 seconds. Response freshness is 2 × p95, bounded to 1.5–5 seconds. Warmup uses a two-second goal and five-second response limit. These are request measurements, not network ping or a benchmark of task success. Adaptation benefits still need controlled evaluation.

Export writes versioned JSON under `runs/` relative to the working directory. Replay makes no model requests or game connections. It does not restore worlds, guarantee deterministic simulation or reveal alternative action outcomes. Inspect recordings for private names and telemetry before sharing.

## Development

```powershell
cargo fmt --check
cargo test
cargo check
cargo clippy --all-targets -- -D warnings
```

On Windows, `.\scripts\start-preview.ps1` accepts app arguments and launches a separate executable copy so an open preview does not lock Cargo's build output.

Capture the rendered app without closing it:

```powershell
cargo run -- --fixture-preview --screenshot runs/ui.png
```

Do not combine this with `EFRAME_SCREENSHOT_TO`. Live capture waits for an observation, error or timeout; inspect the image before treating it as evidence.

`cargo run --bin connection-probe -- --port 25565 --bot JevBot` observes and disconnects. Optional `--jev` makes one real request; `--jev --move` offers a bounded navigation target; `--local-move` tests the executor without Jev. `--action-ms` sets 250–3000 ms (default 500), and `--settle-seconds` allows time to leave a lobby. `cargo run --bin provider-probe` sends a synthetic disconnected-state API probe, not a Minecraft run.

See [architecture](docs/architecture.md) and [contributing](CONTRIBUTING.md).

## Limits and direction

Automatic combat, mining, building, inventory manipulation and general task planning are not implemented. Endpoint/pathfinding guards do not guarantee harmless routes or server permission to run bots. Use an authorized test environment.

Farming Simulator 25 is the next proposed adapter, pending a separate mod-bridge investigation. Library scanning and additional Jev roles remain proposals: [scanner assessment](tasks/optiscaler-research.md), [Jev opportunities](tasks/jev-opportunities.md). Installation discovery does not prove ownership, subscription entitlement or AI support.

## Credits and license

Built with TypeSafe/Jev, Azalea, egui/eframe and other open-source dependencies, informed by community game-agent projects. See [CREDITS.md](CREDITS.md) and [LICENSE](LICENSE). Third-party projects, game assets and services retain their own terms. This project is not affiliated with Mojang or Microsoft.
