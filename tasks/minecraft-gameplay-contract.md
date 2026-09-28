# Minecraft gameplay planning contract

This contract applies to every Minecraft goal, skill and subtask, including future levels. The developer derives ordinary gameplay requirements proactively; the user supplies intent and preferences, not a catalogue of obvious edge cases. A protocol operation is an implementation primitive, not a completed gameplay capability.

## Before implementing each capability

Write a scenario specification covering all of the following. An inapplicable item needs a reason, rather than silent omission.

1. **Outcome and scope:** what the player achieves, where the bounded attempt starts and ends, and what remains unknown or unsupported.
2. **Knowledge:** what is observed, inferred, remembered or unknown; observation age, unloaded terrain and invalidation after world changes.
3. **Dependencies and quantities:** tools, durability, ingredients, fuel, inventory space, equipment and earlier goals; retain resources needed to finish and return.
4. **Access:** approach, reach and line of sight, standing support, headroom, jumping or climbing, landing and a verified route back. Preserve blocks needed by later steps.
5. **Execution and collection:** order of operations, interaction state, dropped items and recovery, server confirmation and progress between bounded attempts.
6. **Competing needs:** health, hunger, threats, time remaining, equipment and retreat. Replan when a previously sensible goal becomes unsuitable.
7. **Interruption and recovery:** cancellation in every phase, changed targets, rejected interactions, full inventory, timeout, disconnect and retry. A previous attempt's outcome cannot complete a new attempt.
8. **Proof:** explicit success, partial progress and blocked states; deterministic scenarios plus independent live evidence. Sending input or receiving an ACK is not success.

Review the scenario from the starting player's perspective through completion and recovery. Check interactions with other skills, not only isolated happy paths. Verify version-sensitive game rules against the supported server and authoritative sources before relying on them. These are required design questions, not claims that all resulting behavior is already implemented.

## Apply to the complete ladder

| Goal | Gameplay questions that must become concrete scenarios |
|---|---|
| See | Can the bot distinguish unknown from absent, find relevant resources beyond a nearest-item summary, reconcile inventory/container state and invalidate observations after movement or world changes? |
| Wood | Can it map observed connected trunks/branches, retain steps and support, reach upper wood through safe standing positions/jumps, collect drops, descend, reserve ingredients, craft and place a usable table? |
| Stone Age | Can it reach and use the table, reserve sticks/planks, choose an effective tool, excavate with headroom and an exit, collect the required material, replace exhausted tools and return? |
| Food | Can it reach a suitable animal, control the encounter without losing its retreat, recover drops, reserve furnace fuel, handle cooking progress and eat before hunger prevents the plan? |
| First night | Can it budget travel and construction time, obtain all materials, build from an order that preserves access, validate enclosure/light/door/bed usability and react when sleep is rejected? |
| Iron Age | Can it explore with a return route and supply budget, check tool suitability, recover ore, smelt with reserved fuel, equip the result and safely store/retrieve items at remembered locations? |
| Settled | Can it preserve and replenish seeds/feed, identify readiness, avoid destroying immature resources, retain access to crops/animals and resume a partially built extension from actual world state? |
| Onwards | Before implementation, separately expand diamond acquisition, enchanting and Nether travel with equipment/resource dependencies, travel and return, changed hazards, recovery and server evidence. These remain future scope. |

The ladder is an outcome roadmap. No row is implementation-ready solely because it lists block breaking, clicks or placement. Apply the eight questions to every subtask added beneath a row. Audit completed capabilities when new dependencies expose missing behavior; do not relabel earlier evidence as broader verification.

## Level 1 scenario specification

The current gather-only slice is incomplete. Required scenarios before Level 1 can pass:

- A short tree, a tall trunk and a branched tree: retain a bounded connected wood view beyond the nearest-resource summary; identify incomplete observation and unresolved wood honestly.
- Lower logs provide current support or access to higher logs: choose a harvest order that preserves access until it is no longer needed, then replan from fresh geometry.
- Higher wood requires a jump: validate a stable reachable landing, headroom and retreat; do not treat momentary airborne reach as sufficient for sustained mining. If access is unavailable, report partial/blocked progress.
- Terrain changes during approach or mining: revalidate target, support and reach; stop instead of continuing an obsolete sequence. Never hold a world lock while acquiring ECS state for client actions.
- A log breaks but its drop is distant, obstructed or cannot enter the inventory: collect through a verified route or report the remaining collection problem. Breaking alone does not complete gathering.
- The inventory is empty, nearly full or contains mixed wood families: calculate valid recipe quantities, reserve subsequent ingredients and handle cursor/container state without inventing item gain.
- Craft planks and a table: validate each interaction against current inventory and require server-confirmed expected changes, accounting for local prediction.
- Place the table: select/hold the item, verify support and replaceable destination, reach and usable access; keep the bot's occupied space and return route clear. Require the server-confirmed block at the exact destination.
- Stop, threat, deadline, disconnect or world change in every phase: release active inputs, cancel remaining interactions and preserve truthful partial progress. Correlate outcomes with unique attempts, including a retry at the same coordinates.
- End-to-end: start in Survival without wood/planks/table, gather and collect, craft, place and finish at an accessible location without operator assistance during the run. Record independent inventory and block evidence; fixtures remain labelled simulations.

## Delivery and evidence

### Shared catalogue and Jev dispatch requirement

Keep reusable decision and execution contracts separate from Minecraft adapters and versioned block/recipe data. The catalogue describes available capabilities, prerequisites and supported semantics; current observations determine which entries are applicable. Version data must not turn historical coordinates into current facts. A plugin framework is not required by this direction, and protocol or behavior changes may still require adapter code and tests.

For semantic AI tasks, Jev must judge whether to handle the bounded task itself or dispatch it to an available larger LLM. This is a shared engine requirement, not a separate hand-written model choice in each gameplay feature. Supply the task, relevant catalogue entries, current evidence and actual handler capabilities. Record the route, model judgment, elapsed time and available usage/cost data. Evaluate routing quality and total cost/latency, including the routing call; do not assume savings. Define uncertainty, unavailable-provider and timeout outcomes explicitly, without routing recursively. Both handlers remain subject to the same fresh-state and deterministic execution guards. Exact lookups, geometry, calculations and fixed rules stay in code. Runtime Jev/Astra dispatch is implemented in the shared routing module and live engine; see ../docs/model-routing.md. Dedicated routing regression coverage, live Astra verification and cost/quality evaluation remain pending.

Expand the design and scenario specification before changing behavior. Implement coherent slices under the Jev role, then have the test role turn the scenarios into meaningful executable checks. The loop server runs configured Cargo checks; independent review checks gameplay coverage and evidence, not only green checks. Live verification follows offline readiness in the authorized test world.

Implemented slices: bounded visible tree geometry and access ordering, attempt correlation, inventory-grid crafting, conservative native placement and isolated routing. Pending: continuity across occluded tree parts, orphan-drop recovery, contextual visibility and collision, full native lifecycle and Level 1 sequence tests, routing evaluation, native UI smoke and authorized live Survival evidence. This document defines the complete requirements; implemented slices do not by themselves mark Level 1 complete.

## Inventory crafting slice specification

An attempt begins with a packet-confirmed idle player inventory, empty cursor/grid and one reserved empty storage slot. It ends after exactly one recipe (one whitelisted log into four matching planks, or four whitelisted planks into one table). Storage/hotbar ingredients only; armor/offhand are not recipe supplies. Mixed valid plank families are allowed. No movement or world access is necessary; the standing position and world lifecycle must remain unchanged. An unknown inventory, open container, full inventory or competing inventory update blocks the attempt.

The native executor sends non-predicting clicks one at a time, checks each changed slot and cursor against subsequent inbound packets, and validates the server recipe preview before taking it. Success requires confirmed ingredient consumption, output deposited in storage, and an empty cursor/grid. Health loss, death, disconnect, world change, stop and deadlines prevent further clicks. Cancellation closes the inventory handle without recovery clicks; vanilla may return staged inputs, and partial state is reported rather than claimed as a completed recipe. A new attempt requires a fresh idle packet-confirmed inventory. Tests and independent live evidence remain separate work; packet support alone is not gameplay verification.

## Mandatory current-world and block-property refinement

The user requires continually refreshed, human-like bounded perception and a property audit covering every block/state. See [current-world perception](../docs/block-perception.md) and the versioned [block-property audit](../docs/block-property-audit-26.2.json). Historical block locations are not current facts. Loaded chunk contents are not automatically visible. Separate sight, collision, support, fluid, interaction and hazard queries, and revalidate against current observations before every substep. This applies to every goal in the ladder. Existing tree memory and scan shortcuts require revision; the research catalogue does not mark runtime behavior complete.
