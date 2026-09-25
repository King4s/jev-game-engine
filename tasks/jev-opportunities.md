# Where Jev adds value

Researched September 25, 2026. This is product prioritization, not an expansion of MVP acceptance criteria or an implementation. It is based on the current Rust/Minecraft plan and official TypeSafe sources. The proposals, budgets and evaluation requirements below are our design choices; their benefits have not yet been measured in Minecraft.

## Recommendation

Keep Jev as the semantic decision maker and Rust as the executor. Two additional uses are worth evaluating: **translate the user's intent into supported goal types**, and **select a new strategy after a locally detected blockage**. Both can help users with short or long response times without moving local control to the server. Start with recovery inside the existing goal decision; add free-text goals once the goal registry is useful enough.

TypeSafe provides Choice for bounded selections, Score for described degrees, and Noul for the probability of yes. These are typed judgments, not an API for generating arbitrary plans, code or explanatory text. [Choice](https://docs.typesafe.ai/primitives/choice), [Score](https://docs.typesafe.ai/primitives/score), [Noul](https://docs.typesafe.ai/primitives/noul).

## Candidates and boundaries

### 1. Understand the user's goal — possible MVP addition

**Value:** “Return to the start” or “explore the area near the trees” can map to named goals the adapter actually supports. This makes the product easier to use across games and languages.

**Input:** user text, supported goal types, named observed locations, capabilities and explicit restrictions. **Output:** Choice for goal type and Choice for an existing target ID; always include `unsupported`/`unknown`. Rust assembles the typed command. Coordinates and durations are calculated or selected from configuration, never generated as free text by Jev. This follows TypeSafe's [function-calling cookbook](https://docs.typesafe.ai/cookbooks/function_calling).

**Budget/latency:** one request per new user goal, at most four questions, cached only for the same text, goal registry and relevant state. No repeated requests to interpret the same goal. The interface can display “interpreting goal” while the local executor remains stopped.

**Guard:** an unknown goal starts nothing; the goal type must exist, the target must still be observed and every action limit applies. In the current navigation MVP, “build a house” therefore cannot be accepted. A known goal can be broken down by a code-based recipe; arbitrary plan generation is outside this proposal.

**Low-cost evaluation:** 40 Danish/English phrasings with manually labeled goals, including ambiguous and unsupported requests. Compare accuracy, rejection of unknown goals and request count/latency against a dropdown with simple aliases. Keep the dropdown as a direct alternative.

### 2. Strategy selection after blockage — first additional MVP candidate

**Value:** choose a meaningful alternative waypoint or terminate repeated failed attempts based on the goal's meaning and history. This can extend the existing Jev goal selection instead of adding another request.

**Input:** active goal, locally measured lack of progress, pathfinder errors, attempted target IDs, freshly observed terrain and legal recovery candidates. **Output:** Choice among `alternate_waypoint`, `retry_current`, `wait`, `stop_goal` with concrete candidate IDs. The engine generates candidates and excludes known illegal actions.

**Budget/latency:** only after local blockage; at most one request per recovery episode and two episodes per goal in the initial test profile. Afterwards display “new goal required”. No separate inference if the same candidates can enter the next regular goal decision. These limits must be configurable and count toward the total session budget.

**Guard:** local movement stops first. Jev never decides whether stopping must wait. Bind the answer to the generation, goal and fresh observation. An unknown/stale answer leaves the executor stopped. No automatic mining, teleportation or server restart.

**Low-cost evaluation:** 20 saved blockage cases against the rule “try the next valid waypoint, otherwise stop”. Measure recovered progress, repeated errors, elapsed time and additional requests. Offline replay can assess choices but cannot prove the outcome of an alternative action; that requires separate controlled runs.

### 3. Assess progress and goal completion — later

For navigation, distance to target, pathfinder status and timeout are sufficient rules. Later, complex goals such as “find a suitable base location” could receive a Score against explicitly described quality levels or a Noul for a concrete condition. Input is the goal plus observed before/after state; missing terrain data remains unknown. At most one judgment per milestone, preferably alongside other independent questions. Label Jev judgments **estimated**, while exact inventory/position requirements are evaluated in code. Evaluate 30 labeled episodes against rules, especially false “complete” outcomes. Noul 0.5 means an uncertain yes/no, not 50% complete. [Noul](https://docs.typesafe.ai/primitives/noul).

### 4. Relevant history and a readable timeline — later

Input: the latest goal, a bounded set of factual events and their IDs. Output: relevance Score per event or Choice of event category. Rust selects events and fills fixed text templates with observed values. Jev must not invent a narrative or causal explanation. Use at most one batch at session completion or user inspection, never per tick. Always retain critical errors, manual input and rejected answers regardless of judgment. Compare against “latest events plus all errors” on 20 logs; measure whether relevant events are omitted. [Re-ranking cookbook](https://docs.typesafe.ai/cookbooks/rerank_typesafe).

### 5. Matching games and setup — later, advisory

Rust scans manifests, registry entries and versions deterministically, potentially drawing on OptiScaler-GUI after separate code/license review. Jev could later rank already verified game adapters against a free-text request. Input: the request and a small adapter registry with verified capabilities; output: adapter ID or `no_match`. One request per user intent. This cannot prove ownership, installation, version compatibility or access. Credentials, complete personal paths and raw license lists are unnecessary. Evaluate against metadata filters on 20 queries. With Minecraft alone, this provides no MVP benefit.

### 6. Adaptive latency policy — keep in Rust

p50/p95, timeout, jitter, observation age, replanning interval and macro duration are measurable quantities with explicit limits. Having Jev calculate them adds another slow and uncertain stage. Use a deterministic policy and test injected response times. Later, Jev may prioritize different already-safe strategies based on a natural-language request; it must not change freshness requirements, budgets or stopping guarantees.

## Shared decision contract

Independent questions over the same state can share a request, but one question cannot consume another question's answer before it is known. Additional questions still consume tokens, and our own end-to-end measurements determine the benefit. [Speculative fan-out](https://docs.typesafe.ai/patterns/fan-out).

Log purpose, state ID, candidate IDs, actual model, validated answer, usage, response time and whether the judgment was used. Confidence measures distribution concentration, not the probability that the entire run succeeds. Evaluate thresholds on our own cases. [Confidence](https://docs.typesafe.ai/confidence). State must distinguish observed facts, unknown data and judgments. [State](https://docs.typesafe.ai/concepts/state).

**No Jev calls** for movement ticks, emergency stop, collision, exact coordinate calculations, manifest parsing, credential detection, API-key validation, proof of licensing or replay playback. The development Jev loop and product Jev judgments retain separate logs and budgets.

Before implementation, choose one candidate and run it in shadow mode on saved cases. Record the baseline outcome too; do not claim benefits based on Jev's own judgment. A result without measurable improvement is a good reason to keep the rule.
