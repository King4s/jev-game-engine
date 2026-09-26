# Survival goals: what the bot must be able to do, and what we must build

Survival Minecraft is not about outrunning monsters. It is about using the day to gather,
craft and build, so that when night falls you are inside a lit, closed house of your own
and sleep until morning; then you get better every day: better tools, steady food, a
bigger and better base. A bot that only waits or flees is not playing the game.

This document fixes the goals as a ladder. Each level says what the player can do in the
game, how we verify it without trusting the bot's own report, and what the engine
(programmer work) needs before the bot can do it. Levels are cumulative: a level counts
only when the levels below it still work in the same run.

Sources: minecraft.wiki pages *Survival*, *Tutorial:Beginner's guide*, *Tutorial:Shelters*,
*Daylight cycle* and *Advancement* (read 2026-09-26).

## Facts from the game that shape the goals

- A day is 24,000 ticks (20 minutes). Daylight is ticks 0–12,000 (10 minutes), dusk
  12,000–13,000, night 13,000–23,000. Hostile mobs start spawning outdoors at tick 13,188
  (12,958 in rain) and stop at 22,812. Undead burn in daylight.
- Sleeping in a bed skips the night to dawn. A bed needs 3 wool (sheep) and 3 planks.
- Mobs spawn in darkness, so a shelter must be lit inside. It needs walls, a roof and a
  door; walls of 4 or more blocks keep spiders out.
- Progression is by tool tier: wood → stone → iron → diamond. Each tier unlocks the next
  (a stone pickaxe mines iron ore; iron is smelted in a furnace; an iron pickaxe mines
  diamonds).
- The game has its own milestones, **advancements**. A server can test them for an online
  player: `execute if entity @a[name=<bot>,advancements={minecraft:story/mine_stone=true}]`
  (verified on the test server 2026-09-26). They are our main objective evidence.

## The ladder

| Level | In the game the bot can… | Verified by | Engine work needed (programmer) |
|---|---|---|---|
| **0 See** | Know where it is, the time of day, its health and hunger, what is in its inventory, and which useful blocks and animals are nearby (logs, stone, ores, sheep, cows, pigs). | Recorded observation matches RCON `data get entity` (Inventory, foodLevel, Health) and `time query minecraft:day`. | Observe inventory, hotbar, food level, time of day and nearby block/entity types, not only waypoints and threats. |
| **1 Wood** | Chop a tree, pick up the logs, turn them into planks and a crafting table, place the table. | `story/root` (crafting table in inventory); table block exists at the reported position. | Break a block and collect its drops; craft in the 2×2 inventory grid; place a block; choose and hold the right item. |
| **2 Stone Age** | Craft a wooden pickaxe at the table, dig down to stone, collect ~14 cobblestone, craft a stone pickaxe, axe and sword and a furnace. | `story/mine_stone`, `story/upgrade_tools`; furnace in inventory or placed. | Craft in a 3×3 crafting-table container; use the best tool for each block; dig down and back up safely (stairs, never straight down into the unknown, avoid lava and falls). |
| **3 Food** | Kill an animal, cook the meat in the furnace, eat when hungry. Keep hunger high enough to sprint and heal. | `husbandry/root` (ate something); recorded food level never drops to starving during the run. | Attack an entity; use a furnace (fuel, input, output); eat an item; a hunger rule. |
| **4 First night** | Before dusk, be inside its own shelter: closed walls and roof, a door it closes, torches inside, a bed; sleep and wake up alive in the morning. | `adventure/sleep_in_bed`; the bot is alive at the next tick 0 and has not left the shelter at night; RCON block checks of the shelter (door closed, light level ≥ 1 on the floor, roof present). | Place blocks from a small blueprint; place and close a door; make charcoal and torches and place them; craft and place a bed and lie in it; plan against the clock ("head home and finish the shelter by tick ~11,000"). |
| **5 Iron Age** | Find and mine iron ore, smelt ingots, make an iron pickaxe and iron armor and wear it; keep items in a chest at home. | `story/smelt_iron`, `story/iron_tools`, `story/obtain_armor`; armor equipped in the recorded inventory. | Search for ore (caves or branch mining) and come back; equip armor; place and use chests; remember where home and its chests are across the session. |
| **6 Settled** | Plant and harvest crops, breed animals, and extend the house (storage room, lit surroundings, more space). | `husbandry/plant_seed`, `husbandry/breed_an_animal`; recorded builds match their blueprints. | Farming (hoe, plant, harvest, replant); feeding and breeding; multi-day build projects that resume where they stopped. |
| **7 Onwards** | Diamonds, enchanting, the Nether. | `story/mine_diamond`, `story/enchant_item`, `story/enter_the_nether`. | Later; not planned until level 6 works. |

## The first milestone: one full Survival day

Levels 0–4 together, as one run in the test world:

1. The bot starts at dawn (tick 0) with an empty inventory near spawn, in Survival.
2. Without operator help it gets wood, a crafting table and wooden, then stone tools,
   a furnace, food and wool, and builds a small closed, lit shelter with a door and a bed.
3. It is inside and in bed before mobs spawn (tick 13,188), sleeps, and is alive and
   inside at the next dawn.
4. Evidence: the advancements above, RCON checks of the shelter and the bot's health,
   and the engine's recording. A run that needed an operator command, or that survived
   the night outside, does not count.

## Jev's role

The engine executes skills; Jev decides what matters now. At each decision Jev sees the
observation (time of day, inventory, health, hunger, what is built, what is nearby) and
chooses the next goal from the skills that are currently possible, for example
*gather wood*, *mine stone*, *craft stone tools*, *get food*, *build shelter*, *go home
and sleep*. Code keeps the hard rules: a skill is offered only when its preconditions
hold, and at dusk without a shelter the offered goals are only the ones that end inside.
Useful yes/no judgments for Jev: *is the shelter safe enough to spend the night?*,
*is there enough time left today for this goal?*

The flee reflex stays as a last-resort safety net, not as a way of playing. The
`wait`/`flee` selection policy work (Task 14) is parked until there are real goals to
choose between.

## Platform and test setup

The engine stays Rust on Azalea, as the project's own contract requires (no Node or
TypeScript runtime). Azalea already provides pathfinding, block breaking, inventory,
containers and attacking; crafting (container clicks on the 2×2 and 3×3 grids) and block
placement ("use item on block") are built on top of it as part of levels 1, 2 and 4.

The operator is only an observer in the test world and must never be what makes a level
pass or fail. Where the server's multiplayer rules would let the observer block the bot,
the test world is configured for the bot, or the observer is moved out of it. The known
case: skipping the night needs `players_sleeping_percentage` low enough for the bot alone
(the server default 100 would require the observer to sleep too); set it for the test
world before level 4 runs and verify it did not change other worlds.
