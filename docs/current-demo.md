# Current demonstrable behavior

The desktop fixture demo is available now. From the repository root, run:

```powershell
cargo run -- --fixture-preview --fixture-reachable-waypoint --screenshot-after-goal runs/current-demo.png
```

The app captures an offline arrival verdict and saves the screenshot; close its window when finished. The fixture makes no Minecraft connection and no TypeSafe or OpenRouter call. Its recorded `FIXTURE` choice, dispatched waypoint and measured arrival demonstrate the engine's action and recording flow, not autonomous gameplay. Run `cargo run -- --fixture-preview` to explore the UI interactively.

Live engine decisions now use Jev-first typed routing. Jev can select its own bounded choice from the current evidence or escalate that same evidence and candidate catalogue to `typesafe/jev-router` through OpenRouter Chat Completions. Escalation requires both the TypeSafe credential and `OPENROUTER_API_KEY`. Live TypeSafe Jev handler calls are recorded from the test-server run (`runs/level1-resume/live-turn23.json`: requests for `jev-latest`, 4,935 input and 107 output tokens at 299ms, blocked with reason "Jev handler judgment is uncertain", no action executed), but no live `typesafe/jev-router` escalation call has been verified. Historical Astra recordings remain supported as historical data; Astra is not part of the current live route.

The current branch also contains offline checks for connected visible wood, bounded access and jump reasoning, inventory recipes, routing validation and native table placement with Azalea components and server-packet evidence. These checks demonstrate code paths; no complete autonomous Minecraft Level 1 sequence (gather wood, craft planks and a table, then place it) has been independently observed in Survival. A live demonstration must start without those resources in the authorized Java test world, let the model choose, and retain independent inventory and block reads alongside the recording. An accepted action, screenshot or fixture recording is not proof of task completion.

The most recent bounded live run is written up in [runs/level1-lookaround/READBACK-live-frozen.md](../runs/level1-lookaround/READBACK-live-frozen.md): in that run the model chose a gather action that the adapter accepted, the wood skill then reported `Wood movement left verified transition`, a second model choice moved the bot with a verified arrival (`Local executor: target reached`), and the session ended when a third handler answer fell below the run's own confidence floor. Inventory stayed empty: no log, planks or table, so the Level 1 ladder is still unproven.

The controls for optional wood skills are disabled by default. A live run requires Minecraft Java 26.2, the permitted dimension and the TypeSafe key; a Jev Router escalation additionally requires `OPENROUTER_API_KEY`. See [model routing](model-routing.md) for request budgets, confidence, freshness and failure behavior.
