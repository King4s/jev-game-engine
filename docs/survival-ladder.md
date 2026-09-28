# One engine: the survival ladder

The engine has one perception model, one mover and one harvester. A goal is data on top of those,
not a new code path. A stone, a log, dirt and a crafting-table placement all travel the same route:

```
observe -> world model (every observed cell, its block, its support)
        -> verified target (reachable stance, clear ray, drop item known)
        -> one mover (motion::follow) walking the verified route
        -> one interaction (mine / place / craft) that must confirm the world changed
        -> re-observe and compare; a goal that cannot be confirmed is abandoned honestly
```

Nothing here may grow a second motion path or a second verification rule: if a goal needs a new
kind of step, the step belongs in `motion`, and if it needs a new kind of target, the target
belongs in `harvest`.

## First day, in order

Minecraft's day is 20 real minutes (about 10 day, 7 night). The ladder below is the order a
survival player actually follows, and each rung is the same engine work as the rung before:

| # | Goal | Generic form | Why it is on the ladder |
| --- | --- | --- | --- |
| 1 | Wood | harvest any log | the only tool material reachable with bare hands |
| 2 | Planks and sticks | craft from owned items | every tool needs sticks, every stick needs planks |
| 3 | Crafting table | craft then place on clear ground | stone tools and every later recipe need it |
| 4 | Wooden tools | craft axe/pickaxe/shovel/sword | harvest speed, stone access, defence |
| 5 | Stone | harvest stone/cobblestone | durable tools and a shelter that mobs cannot break |
| 6 | Shelter before dusk | place blocks as walls, roof and a door gap | at night, light 0 spawns mobs and the bot dies |
| 7 | Light | place a torch | keeps spawns out of the shelter |
| 8 | Food and bed | harvest and cook what is found, place a bed | regeneration, and it resets the danger of night |

Rungs 1-6 need nothing but the one engine: harvest a block, craft from owned items, place a block.
Rungs 7-8 add items, not mechanics.

## Survival rules the engine must obey while doing any of it

- Health and food come first. A goal that keeps walking while health falls is a bug, not bravery.
- Night is a deadline, not a timer: if dusk is close and the bot has blocks, it builds; it does not
  start a long trip. Time of day is part of perception for exactly this reason.
- Never trade a verified step for an unverified shortcut. Falling, lava, drowning and mobs all end
  runs that ignore the verified route.

## Where the engine stands against this ladder

Nothing above rung 2 has been completed live. A live run has dispatched and had accepted a
`gather` action, walked a waypoint with a verified arrival, and survived damage by fleeing, but no
log has ever reached the inventory: the gather attempt ends either on its action deadline without a
confirmed inventory gain, or because the bot is taking damage while it works. Inventory reads and
block reads in the recordings are the evidence; anything else is a description, not a result.

## What the code now covers of the ladder

The catalogue turns the ladder into goals that are offered only when the observed world
makes them possible, in ladder order, each with the facts needed to weigh it:

- `goal_wood` (harvest 3 log(s) from observed trees), then `goal_planks`, then
  `goal_table` (each gated on what is actually held and observed);
- `goal_stone` (mine 20 cobblestone from observed stone) and `goal_soil` (dig 12 dirt from
  observed soil) — the same verified stance-and-route machinery, only the block differs;
- `goal_shelter` (build a 3x3 shelter with a roof from the blocks held) offered only when
  blocks are in hand *and* the daylight left before mobs can spawn is short, and stating
  that deadline in ticks;
- `goal_light` (place a torch inside it), gated on a torch actually being held;
- `goal_food` (eat until not hungry), gated on food below full;
- `goal_survive` (survive until dawn), gated on an observed threat and stating the threat
  and the daylight left.

Harvest goals bank a *group* (log / stone / soil), so "mine until there is enough stone"
is one goal the engine carries out over many blocks through the one harvester and the one
mover, re-asking the model only on completion, honest failure, or when survival forces a
new decision.

Not yet covered, and stated as such inside the entry rather than hidden: there is no
attack executor, no torch-placement executor and no eating executor in this build. Those
options are still offered — with their precondition, cost and the honest note that the
engine cannot carry them out yet — because hiding a possibility the world allows is worse
than reporting that the engine cannot yet perform it.
