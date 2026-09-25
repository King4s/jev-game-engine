# Credits and acknowledgements

Jev Game Engine is a Rust project by [King4s](https://github.com/King4s) and contributors. Its original code is provided under the [MIT license](LICENSE). Third-party dependencies, tools, games and models retain their own licenses and terms. Acknowledgement does not imply endorsement or affiliation.

## Runtime and development foundations

Direct dependencies were identified with offline Cargo metadata; license expressions below were checked against the locally resolved package manifests on 2026-09-25. `Cargo.lock` records exact dependency versions. This list credits direct foundations, not every transitive dependency or a complete binary redistribution notice bundle.

| Project | Role | Declared license |
|---|---|---|
| [Azalea](https://github.com/azalea-rs/azalea) | Native Minecraft protocol, observations, physics and pathfinding; pinned revision `b65fa8cf1bb957976cefa926b9b500d44767d806` | MIT |
| [egui / eframe](https://github.com/emilk/egui) | Desktop UI and application framework | MIT OR Apache-2.0 |
| [Tokio](https://github.com/tokio-rs/tokio) | Async runtime, channels and timers | MIT |
| [reqwest](https://github.com/seanmonstar/reqwest) | HTTPS transport and connection pooling | MIT OR Apache-2.0 |
| [Serde](https://github.com/serde-rs/serde) and [serde_json](https://github.com/serde-rs/json) | Typed serialization and JSON | MIT OR Apache-2.0 |
| [anyhow](https://github.com/dtolnay/anyhow) | Error handling | MIT OR Apache-2.0 |
| [image](https://github.com/image-rs/image) | Image support | MIT OR Apache-2.0 |
| [tempfile](https://github.com/Stebalien/tempfile) | Test-only temporary files | MIT OR Apache-2.0 |

Thanks also to the [Rust project](https://www.rust-lang.org/) and its toolchain contributors. [TypeSafe and Jev](https://typesafe.ai/) provide the model service; the integration follows the [official TypeSafe documentation](https://docs.typesafe.ai/). Access to that service is governed by its own terms, not this repository's MIT license.

## All original inspiration sources

The following nineteen sources were supplied during planning and reviewed as references. Their inclusion does not mean their code, games or models are bundled here. The Rust harness was independently implemented around its dependencies; no source code from these inspiration projects was copied during the recorded assessment.

| Source | Ideas considered |
|---|---|
| [phyous/tsai-sc](https://github.com/phyous/tsai-sc) | Structured observations, bounded candidates, generation identity, execution evidence and honest probability reporting. Its MIT-licensed controller was a particularly useful reference for separating dispatch, acceptance and observed outcomes. |
| [OmniJev/PlayJev](https://github.com/OmniJev/PlayJev) | Small shared game interfaces and explicit episode limits. Its Apache-2.0 browser harness and separate pixel model were assessed, not adopted as the Minecraft runtime. Bundled games have individual licenses. |
| [Minecraft Jev discussion](https://reddit.com/r/accelerate/comments/1whk9oy/new_typesafe_ai_jev_model_playing_minecraft_wip/) | Bounded Minecraft decisions and pathfinding; community demonstration, not reproduced benchmark evidence. |
| [fhshaik/typesafe-mario](https://github.com/fhshaik/typesafe-mario) | Structured game telemetry, primitive composition and model inspection. |
| [ArturSkowronski/kNES](https://github.com/ArturSkowronski/kNES) | Explicit stepping, snapshots and replay capabilities. |
| [milanboers/jev-plays-pokemon](https://github.com/milanboers/jev-plays-pokemon) | Separation of goal selection, memory and local execution. |
| [valentynkit/jev-plays-pokemon-red](https://github.com/valentynkit/jev-plays-pokemon-red) | Decision-point calls, fixtures and measured evaluation. |
| [RomanSlack/jev-drone](https://github.com/RomanSlack/jev-drone) | Fast local control and safety reflexes alongside slower tactical judgments. |
| [kxzk/typesafe-jev-drone-demo](https://github.com/kxzk/typesafe-jev-drone-demo) | Authoritative state, model inspection and action revalidation. |
| [khordoo/jev-reflex-autonomy-lab](https://github.com/khordoo/jev-reflex-autonomy-lab) | Explicit provenance and expiry of asynchronous advice. |
| [lykycy123/RoboJEV](https://github.com/lykycy123/RoboJEV) | Distinct intent, execution and objective outcome checks. |
| [ARCJ137442/jev-2048](https://github.com/ARCJ137442/jev-2048) | Inspectable distributions, prompts, usage and run archives; 2048 is not part of this project's selected game scope. |
| [ARCJ137442/jev-life](https://github.com/ARCJ137442/jev-life) | Versioned rules and explicit provider identity. |
| [Jev Chess](https://jevchess.com) | Initial visual reference; no architecture or performance conclusions were drawn from its unreadable page text. |
| [PromptEngineer48/laya-vs-jev-arena](https://github.com/PromptEngineer48/laya-vs-jev-arena) | Controller boundaries and timing comparisons. |
| [hegargarcia/jev-playground](https://github.com/hegargarcia/jev-playground) | Game-owned legal actions and unavailable metrics represented honestly. No repository license was found during focused inspection; no implementation was copied. |
| [kavehmz/typesafe-playground](https://github.com/kavehmz/typesafe-playground) | Separate human-facing views and structured model observations. |
| [spoonnotfound/soupbase](https://github.com/spoonnotfound/soupbase) | Structured judgments combined with deterministic response construction. |
| [hectorlcastro09/jev-torneo-animales](https://github.com/hectorlcastro09/jev-torneo-animales) | Event-based execution, connection reuse and explicit selection policy. The connection-reuse pattern was applied through reqwest's existing pool. |

See [the focused reuse assessment](tasks/reuse-assessment.md) for concrete use/reject decisions and pinned source revisions. General ideas are distinguished from dependency code and direct source reuse.

## Operational tools and additional research

- [Chunker](https://github.com/HiveGamesOSS/Chunker), MIT: used locally as a separate operational tool to convert the user's existing Bedrock world to Java. It is not an application dependency; no game-world archive is licensed or distributed by this repository.
- [Paper](https://papermc.io/) and [Multiverse-Core](https://github.com/Multiverse/Multiverse-Core): server software and documentation used for compatibility research and the user's world import. They are not bundled in this application.
- **OptiScaler-GUI**: a separately supplied local source project reviewed for library discovery and platform metadata. It was not adopted as a runtime dependency and no source was copied. An upstream URL and license were not verified, so no ownership or licensing claim is made here.
- **Minecraft / Mojang / Microsoft**: the game and its trademarks belong to their respective owners. This project connects a separate bot to an authorized server and does not distribute Minecraft binaries or grant game access.

The live Minecraft screenshot was supplied by the project owner. Minecraft's visual assets and trademarks retain their owners' rights; the MIT license applies to the project's original code, not to Minecraft assets.
