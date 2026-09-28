# Opt-in wood gathering and crafting



Level 1 now has opt-in native gathering, inventory-grid crafting and conservative crafting-table placement. Offline checks include native placement phases and server-inventory evidence. The complete Level 1 sequence, native crafting and gathering lifecycle tests, and live Survival outcome remain unverified.

Wood inspection reads one current bounded look-around frame, the same frame the rest of the observation uses: eight look directions (four bearings at the bot's own pitch, then the same four tilted 45 degrees down), a 120-degree cone and 16m limit per direction, and 100,000 ray-cell reads per direction, with the direction count, read count and truncation state reported in the observation note. Block targets are sampled at the centre of the exposed face that points most toward the eye, so ground and trunk surfaces are observed rather than the interior of a block. The frame therefore covers what the bot could see by turning its head once, and it alone decides what may be a **target**: a harvestable cell outside the bot's own contact column is reported only when that frame perceived it, so no candidate can name a break of a block the engine has not seen. The terrain the **route** is planned through is read from the loaded world instead, because that is exactly the source the mover's own route validator reads before every step (`BotNav::verify_route`: standable cell plus the headroom cells at `max(y) + 2`), and because the headroom cell of a one-block step sits 62 to 90 degrees above the eye while every look sector is a 120-degree cone pointing horizontally or tilted down (highest reach 60 degrees above the horizon): on a frame-only map no route with a height change could ever be verified. Planning through loaded cells does not authorize walking through them — each step is still authorized separately by fresh sight of its own swept volume, and a cell whose state the server never sent stays unknown. Lighting, fluid vision context, unresolved block shapes and any perception beyond sight remain open questions. A live Survival wood-to-planks-to-table sequence is still unproven until an exported recording plus independent server readback exists; a reported candidate, an accepted action or an offline check is not that proof.



In the native UI enable **Wood skills (gather logs and craft planks/tables; opt-in)**, set **Permitted wood dimension**, then reconnect with those settings. The default is disabled; the default permitted dimension is `minecraft:overworld`.



The Observed world panel shows Wood status from the latest observation, labeled live, offline fixture, or recorded replay. Each bounded tree entry shows reported log and verified harvest option counts, its current status, and aggregate rejection reasons: unknown visibility, no verified stance or route, support or upper-log reservation, memory veto, and tree-memory capacity loss. These counts do not reveal hidden cells or rejected routes. A recoverable item appears only while the adapter reports a visible drop with a verified return route; collection still needs inventory confirmation. The last skill attempt shows the candidate, world epoch, server-confirmation state, and outcome message. A missing tree or drop means none was reported in that observation, not that surrounding terrain is clear. The offline fixture does not inspect Minecraft trees or drops; replay shows recorded telemetry without a live world read.


For the headless runner add `--wood-skills --allowed-dimension minecraft:overworld --objective "Gather wood"` to the normal live `session-run` command. This explicitly permits breaking the chosen usable log and crafting planks/tables. Existing navigation does not permit mining.



The engine now inspects connected usable wood in a loaded 25 by 25 by 17 cell box. It exposes up to eight observed components with up to 128 logs each, an incomplete-boundary flag and a blocked/progress description. These are observed wood components, not a guarantee of an entire biological tree. Plant stems are excluded.



A bounded reversible route search considers stable standing positions and cardinal one-block climbs with extra headroom for the return jump. Candidates prefer upper wood, preserve standing/route support, and retain lower column logs while higher wood remains. Mining requires a clear sampled sight line and three-block eye reach. The native adapter independently reconstructs the candidate and checks route support/headroom during execution. Geometry outside these bounds remains unknown; scaffolding and transient airborne mining are unsupported.



This is a partial implementation, awaiting full native scenario tests and live verification. Collection follows observed item entities over a reversible route and returns to the original stance before success. The next movement cell and mining line now require current bounded sight. The native adapter turns toward each non-contact step cell before taking a short world read for its sight sample, so no world read guard spans an ECS look update. Unknown or occluded cells still pause movement. Whole-tree identity across occlusion and orphan-drop retry recovery remain pending.



Gather results appear as skill events and carry a unique dispatch attempt, candidate and world epoch in observations. Success requires both an adapter-confirmed server observation and an increase in the matching log inventory total. Merely arriving, sending a mining packet or receiving acceptance does not count. Expiry and rejection remain recorded. Stop, pause, disconnect, death, world lifecycle change and stale observations cancel execution. A new model decision is required for another attempt.



Old serialized settings, candidates and observations retain their defaults and navigation behavior. Native UI smoke evidence, full gather lifecycle tests and live verification remain separate acceptance work.



## Bounded inventory crafting



Crafting candidates use only packet-confirmed idle player storage/hotbar slots. An empty cursor, empty 2�2 grid/result, no open container, and one reserved empty storage slot are required. Full inventories are conservatively blocked even when merging might be possible. Each attempt consumes exactly one supported log/stem for four matching planks, or four supported planks (including mixed families) for one table. Supported wood families are oak, spruce, birch, jungle, acacia, dark oak, mangrove, cherry, pale oak, crimson and warped. Bamboo planks may contribute to tables; bamboo-to-planks, stripped logs and wood/hyphae recipes are outside this slice. The server preview must validate the expected recipe and count before output pickup.



The native executor sends at most one non-predicting click per game tick and waits for inbound slot/cursor changes after each click (two-second per-click bound, ten-second attempt bound). It deposits one ingredient at a time, returns surplus immediately, takes one result and deposits it in the reserved slot. Engine accounting checks exact ingredient consumption and output gain, separately from the attempt-correlated server result. Ordinary Azalea local predictions are never accepted as crafting proof.



Cancellation releases the retained inventory handle and sends no further recipe clicks. The server may return staged ingredients when the inventory closes; a failed attempt reports partial state, and another attempt requires a confirmed empty grid/cursor. Movement beyond 0.6 blocks, damage, world changes and changed inventory preconditions interrupt crafting. Recipe rejection or absent packets expires honestly. Custom servers that suppress the required slot/cursor updates may block progress; native/live verification remains pending.



Implementation sources are the pinned [container handle](https://github.com/azalea-rs/azalea/blob/b65fa8cf1bb957976cefa926b9b500d44767d806/azalea/src/container.rs), [click packet](https://github.com/azalea-rs/azalea/blob/b65fa8cf1bb957976cefa926b9b500d44767d806/azalea-protocol/src/packets/game/s_container_click.rs), [prediction handler](https://github.com/azalea-rs/azalea/blob/b65fa8cf1bb957976cefa926b9b500d44767d806/azalea-client/src/plugins/inventory/mod.rs), and [incoming packet handlers](https://github.com/azalea-rs/azalea/blob/b65fa8cf1bb957976cefa926b9b500d44767d806/azalea-client/src/plugins/packet/game/mod.rs). The adapter fills the pinned client's missing cursor and full-content metadata updates only from server packets. Recipe support is bounded by the whitelist and runtime server preview; this document does not claim independent 26.2 recipe-data or live verification.


Native tree observations combine the current visibility-filtered tree scan with bounded TreeMemory. The memory retains identity and unresolved wood/access dependencies through occlusion, and a currently visible changed cell can remove remembered wood. Remembered cells remain internal history and can only veto a harvest; the public log list contains current visible wood, and every offered target and route comes from the current scan. If the eight-entry memory limit skips a tree, or a merged/updated entry loses wood at its 256-cell limit, all wood harvest actions are blocked with an explicit capacity status until the spawn/dimension epoch resets. Ordinary partial or currently unobserved wood alone does not trigger this capacity block. Native regression and live verification of this connection remain pending; currently hidden wood is unknown as present terrain.

Visible block facts are capped at 16 per frame and include exact 26.2 state properties, position-dependent collision/interaction boxes, fluid state and native behavior values. Conditional rules remain explicitly unresolved. These facts are included in provider state and recording serialization; legacy observations default to an empty list. See [perception status](block-perception.md).

## Crafting-table placement

The native adapter emits only nearby sites that the current view shows with clear body space, a second adjacent exit and one of four conservatively supported floor blocks: stone, cobblestone, dirt or grass block. A packet-confirmed table in the hotbar can be selected directly; a table in main storage first requires an empty hotbar slot and a server-confirmed swap. The executor requires a real upward hit on the bound support, sends one use-item packet and waits for an exact server block update plus the expected inventory decrement. A brief observation may bridge the look-down action for at most 250 ms and is invalidated by block updates. Other materials and unsupported conditions remain blocked. See [placement details](table-placement.md).

The native placement phase and cancellation tests run in an offline Azalea world; they do not establish success on a live server. A full gather�craft�place fixture and authorized Survival run still need verification.

## Block-agnostic harvest (one harvest path for every block)

Wood work is no longer a special case: it is the same path the engine uses for stone,
dirt and anything else the world shows.

- **One whitelist.** `wood::is_harvestable` is the only list that decides what may be
  harvested: every `*_log`, `stone`, `cobblestone`, `deepslate`, its variants, `dirt`,
  `grass_block`, `sand`, `gravel`. Adding a material means adding a name here, not writing
  a new skill.
- **One verification.** `wood_tree::inspect` builds components of harvestable cells and
  verifies each candidate stance with the unchanged rules: the stance must be walkable
  (floor at the player's own 0.3 half-width within the cell), the ray from the eye to the
  exact target block must be clear, the block below the stance must stay (standing support
  preserved), and a remembered access may still veto a cell.
- **One id scheme.** Candidates are still `gather_<x>_<y>_<z>`, so recordings and tests
  written against spruce keep loading and keep meaning the same thing.
- **The payload says what the block is and what it gives.** Each candidate carries the
  block name and `wood::drop_for_block(block)`: stone → cobblestone, grass_block → dirt,
  a log → that same log. The executor confirms the inventory against the drop, never
  against the block name.
- **Risk is a fact, not a refusal.** Where the engine used to refuse wood work with a
  nearby threat, thin food or a truncated entity scan, the candidate now states the risk
  (`survival::work_risk`) in its description and the model decides. The only remaining
  engine-side vetoes are about the world being unfit for any work at all: not connected,
  mode not live, wrong dimension, full inventory.

## Repair: a perceived block could never become a gather candidate

**Root cause (file:line).** The executable gather options have exactly one source,
`TreeView.next`: `wood::gather_candidates` (`src/wood.rs:265-283`) maps `observation.trees[].next`
into `gather_<x>_<y>_<z>` candidates and `catalog::tactical` (`src/catalog.rs:663`) extends the
menu with them; the catalogue's own view uses the same source (`catalog::harvest_options`,
`src/catalog.rs:106-136`, and `possible(GoalKind::Wood)` refuses the wood goal while
`verified == 0`, `src/catalog.rs:301`). `next` is filled only by the stance search in
`wood_tree::inspect` (`src/wood_tree.rs:260-347`). That search walks `parents`, the
walk-reachable stances built by `reachable()` (`src/wood_tree.rs:667-713`), and every step with
a height change needs the headroom cell at `max(y) + 2` above both columns
(`src/wood_tree.rs:700-708`). The adapter's terrain lookup that fed that map (then at
`src/minecraft.rs:1851-1869`) answered every non-contact cell from the current look-around frame
only. A one-block step up needs the headroom cell above the bot's own column, 1.88 blocks above
the eye and therefore 75 to 90 degrees of elevation, while every look sector is a 120-degree cone
pointing horizontally or tilted down and reaches at most 60 degrees above the horizon
(`src/perception.rs:42-54`); unless the bot happens to look up by more than fifteen degrees — it does
not while it faces the block it will break — that cell is never in the frame, so a frame-only map
could verify **no route with a height change at all**. The planner was also
stricter than its own validator — `BotNav::verify_route` (`src/minecraft.rs:1357-1381`) reads
`standable` and the same `max(y) + 2` headroom cells from the loaded world before every step.

**Live evidence.** `runs/level1-live/live-known-block.json` (controlled Survival trial; server
truth in `runs/level1-live/known-block-setup.log`: `Changed the block at -1446, 72, -1549` and
`-1446, 73, -1549`, `Test passed` for `if block -1446 72 -1549 minecraft:oak_log`): the bot stood
at (-1447.5, 71.0, -1548.5); the observation perceived the trunk (`resources`: `minecraft:oak_log`
at (-1445.5, 72.5, -1548.5) and (-1445.5, 73.5, -1548.5)) and the loaded world said the cell beside
it was standable (`blocks`: `waypoint:-1447:72:-1549`, from `standable(&world, …)`); the tree views
numbered 15+ and **every one reported `next: []`**, including Tree 15 (`wood: minecraft:oak_log`)
whose `logs` held both perceived cells, with `stance/route 2` in its rejection summary. The request
menu held 17 waypoints and `wait` and **not one `gather_*` candidate**, so the break was never even
requested.

**Change.** One source, no new rule: in the adapter's scan lookup (`src/minecraft.rs:1875-1891`
after the change; the comment above it at `src/minecraft.rs:1851-1874` records the reasoning)
the terrain (`air`, `support`) now comes from the loaded world, which is what the mover already
reads for route cells and headroom; a cell may still be a **target** only when this observation's
frame perceived it (`is_harvestable(name) && (contact || perceived)`), so the scan can never offer
a break of a block the engine has not seen, and the bot's own contact column keeps the exemption it
always had. Nothing else changed: `wood_tree::inspect`, `can_mine`, the eye-reach and clear-ray
rules, the support/upper-log reservation, the memory veto and the `TreeMemory` reconciliation (which
still uses the frame, `src/minecraft.rs:1892-1910`) are untouched, and each step is still authorized
separately by fresh sight of its own swept volume (`observed_wood_step`, `src/minecraft.rs:1503-1545`).
No change was needed in `src/catalog.rs` or `src/wood.rs`: they were already correct consumers of
`next`.

**Still honest by construction.** A perceived block whose break is unlawful or unsupported still
gets no candidate: no walk-reachable stance within eye reach, a blocked ray (real leaves remain
solid), a stance that would destroy its own support, a column whose lower log is withheld while an
upper one is reachable, or a memory veto all leave `next` empty. `wait` and the knowledge-only
goals are untouched, and an offered candidate is still re-verified at dispatch
(`Gather::prepare`, `src/minecraft.rs:925-1010`) and every tick (`Gather::advance`).

**Known, untouched asymmetry.** The collect-phase pickup route still plans from the frame only
(`src/minecraft.rs:1263-1292`); a drop behind occlusion can therefore still be uncollectible even
though the mover could walk to it. Out of scope for this change.

**Unverified.** The repair has not been re-run against the live server: whether the operator-placed
trunk now yields a `gather_*` candidate depends on the loaded world containing the headroom cells a
step to the stance needs (the trial bot stood inside a leaf canopy, which is why no stance was
verified). A live Survival break whose drop is confirmed in the authoritative inventory is still
required before this link counts as proven.

