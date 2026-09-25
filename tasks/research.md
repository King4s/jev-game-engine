# Inspiration analysis — AI Game Engine

Inspected September 25, 2026. This is documentation analysis, not reproduced benchmarking. All 19 links were opened, GitHub README material reviewed and selected policy files inspected. Demos were not run. Numbers from different projects are not directly comparable.

**Historical context:** initial 2048/Snake and JavaScript ideas below predate the user's Rust/Minecraft decision. They remain research provenance, not current requirements; tasks/plan.md takes precedence.

## Useful findings

| Source | Relevant observation | Application |
|---|---|---|
| [tsai-sc](https://github.com/phyous/tsai-sc) | Structured observations, bounded commands, recorded probabilities and game-result verification; inference occurs while paused. | Trace observations to execution; distinguish simulation time from wall time. |
| [PlayJev](https://github.com/OmniJev/PlayJev) | Separate Qwen-based pixel model; several games share a small start/step/frame/score/done interface. | Small adapter contract and consistent game selection. A pixel provider is a future extension, not an assumed TypeSafe Jev feature. |
| [Minecraft post](https://reddit.com/r/accelerate/comments/1whk9oy/new_typesafe_ai_jev_model_playing_minecraft_wip/) | Author describes Mineflayer, bounded decisions and pathfinding. | Goal/macro-based Minecraft adapter. A Reddit description is not verified implementation or benchmark evidence. |
| [typesafe-mario](https://github.com/fhshaik/typesafe-mario) | RAM/telemetry becomes object-oriented state; Choice, Noul and Score are combined; dashboard shows model data. | Compact model observations, explicit timing and separate debug views. Extra questions require concrete value. |
| [kNES](https://github.com/ArturSkowronski/kNES) | Emulator stepping, savestates, replay and API; GPL-3.0. | Explicit pause/snapshot/restore capabilities. Assess integration separately; do not copy emulator code into MVP. |
| [milanboers Pokémon](https://github.com/milanboers/jev-plays-pokemon) | Harness retains memory; Jev chooses goals while code executes actions including A*. | Short explicit history and goal/macros; identify who performs execution. |
| [valentynkit Pokémon](https://github.com/valentynkit/jev-plays-pokemon-red) | Model calls at branches, fixtures and measured calibration with stated limits. | Decision-point calls and separate offline/live tests; do not assume calibration. |
| [RomanSlack drone](https://github.com/RomanSlack/jev-drone) | Separate perception, fast physical control, safety reflexes and slower Jev tactics. | Separate simulation and inference; record which component actually controlled movement. |
| [Jev Flight Lab](https://github.com/kxzk/typesafe-jev-drone-demo) | Authoritative backend, interpolated scene, model inspector and action revalidation. | Backend owns state; UI displays snapshots and interpolates updates. |
| [Reflex Autonomy Lab](https://github.com/khordoo/jev-reflex-autonomy-lab) | Optional asynchronous System 2 advice; records when advice was used. | Possible later strategy layer with expiry; MVP uses Jev as its only live provider. |
| [RoboJEV](https://github.com/lykycy123/RoboJEV) | Intent and movement are separate; objective outcome checks and distinct rule baselines. | Game determines outcomes. Baselines are separate runs, never hidden Jev substitutes. |
| [jev-2048](https://github.com/ARCJ137442/jev-2048) | Instrumented laboratory with probabilities, prompts, token use and archives. | Historical proposal: 2048 vertical slice with full inspector. Current scope excludes 2048 but retains inspection ideas. |
| [jev-life](https://github.com/ARCJ137442/jev-life) | Rule experiments and a compatibility broker for other models. | Version rules/questions; preserve provider identity and original metric meaning. |
| [jevchess.com](https://jevchess.com) | Opened successfully but provided no readable page text. | Possible later visual reference only; infer no architecture or strength claims. |
| [Laya vs Jev Arena](https://github.com/PromptEngineer48/laya-vs-jev-arena) | Multiple controllers, shared arena and exported timing; its Snake rules differ from classic Snake. | Controller boundary and later comparisons. Historical Snake proposal would require explicit rules; Snake is now excluded. |
| [hegargarcia playground](https://github.com/hegargarcia/jev-playground) | Game owns legal actions; decisions are inspectable; missing metrics are unavailable. | Valid action IDs only; null is not zero. Never fabricate confidence. |
| [kavehmz playground](https://github.com/kavehmz/typesafe-playground) | Cameras for humans, structured sensors for Jev and visible memory. | Switch between game display and exact model observations. |
| [soupbase](https://github.com/spoonnotfound/soupbase) | Jev judges text puzzles through structured outputs; code constructs responses. | Potential text-game/judge role. Deterministic games retain their own outcome rules. |
| [Animal Tournament](https://github.com/hectorlcastro09/jev-torneo-animales) | Event-based tournament, reused connection, argmax or sampling. | Event stream and reused client; record selection policy and RNG with the run. |

## Source inspection: inspiration is not a complete engine

- [2048 decision.ts](https://github.com/ARCJ137442/jev-2048/blob/HEAD/src/client/decision.ts) can choose the next legal alternative and returns the first legal direction on some missing-data branches. Do not adopt these fallbacks: invalid model responses become visible errors or rejected decisions.
- [Mario policy.py](https://github.com/fhshaik/typesafe-mario/blob/HEAD/src/typesafe_mario/policy.py) separates policy from snapshots and batches three questions. Keep this boundary while retaining game geometry/action definitions in adapters.
- [Arena agents.js](https://github.com/PromptEngineer48/laya-vs-jev-arena/blob/HEAD/shared/agents.js) uses concurrent workers and sustained control. Our initial scheduler permits one active request per session, with deadlines and generation IDs so old responses cannot become new actions.

## Official foundation

- [JavaScript SDK](https://docs.typesafe.ai/sdk/javascript): `@typesafe-ai/sdk`, `TypeSafeClient`, `systemOne`, typed questions. Historical SDK reference; the selected implementation is Rust over HTTP.
- [Choice](https://docs.typesafe.ai/primitives/choice): named options and a probability distribution.
- [State](https://docs.typesafe.ai/concepts/state): explicit per-request context; use versioned JSON.
- [Confidence](https://docs.typesafe.ai/confidence): response-distribution statistic; evaluate thresholds in the target domain.
- [Function calling](https://docs.typesafe.ai/cookbooks/function_calling): code maps bounded choices to functions; independent questions can be batched.
- [HTTP API](https://docs.typesafe.ai/api): verify the current contract when writing the integration.

## Recommendation

Build a small new engine with game adapters and a common control panel. Reuse design ideas rather than merging 17 different codebases. The common loop is observe → bound options → ask Jev → validate → execute → measure → display. Inspect model input, chosen action and actual execution in one timeline.

Check licenses and assets at the exact revision before reusing code. This planning work imported no source code, ROMs or game graphics.
