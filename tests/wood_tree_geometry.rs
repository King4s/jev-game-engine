use jev_game_engine::model::Position;
use jev_game_engine::wood_tree::{Cell, Terrain, inspect};
use std::collections::BTreeMap;

const OAK: &str = "minecraft:oak_log";

fn world() -> BTreeMap<Cell, Terrain> {
    let mut cells = BTreeMap::new();
    for x in -12..=12 {
        for z in -12..=12 {
            for y in -8..=8 {
                cells.insert(
                    (x, y, z),
                    Terrain {
                        air: y >= 0,
                        support: y < 0,
                        log: None,
                    },
                );
            }
        }
    }
    cells
}

fn log(cells: &mut BTreeMap<Cell, Terrain>, p: Cell) {
    cells.insert(
        p,
        Terrain {
            air: false,
            support: true,
            log: Some(OAK.into()),
        },
    );
}

fn inspect_world(cells: &BTreeMap<Cell, Terrain>) -> Vec<jev_game_engine::wood_tree::TreeView> {
    inspect(
        &Position {
            x: 0.5,
            y: 0.0,
            z: 0.5,
        },
        |p| cells.get(&p).cloned(),
    )
}

fn cell(p: &Position) -> Cell {
    (p.x.floor() as i32, p.y.floor() as i32, p.z.floor() as i32)
}

#[test]
fn short_tree_exposes_upper_log_before_lower_log() {
    let mut cells = world();
    log(&mut cells, (2, 0, 0));
    log(&mut cells, (2, 1, 0));

    let trees = inspect_world(&cells);
    assert_eq!(trees.len(), 1);
    assert_eq!(trees[0].logs.len(), 2);
    assert!(!trees[0].incomplete);
    assert!(
        trees[0]
            .next
            .iter()
            .any(|step| cell(&step.target) == (2, 1, 0))
    );
    assert!(
        trees[0]
            .next
            .iter()
            .all(|step| cell(&step.target) != (2, 0, 0))
    );
}

#[test]
fn five_log_trunk_keeps_safe_partial_progress_when_top_is_unreachable() {
    let mut cells = world();
    for y in 0..5 {
        log(&mut cells, (2, y, 0));
    }

    let trees = inspect_world(&cells);
    assert_eq!(trees.len(), 1);
    assert_eq!(trees[0].logs.len(), 5);
    assert!(
        trees[0].next.iter().any(|step| cell(&step.target).1 < 4),
        "a lower log that can be mined from ground without removing route support should remain available"
    );
    assert!(trees[0].next.iter().all(|step| cell(&step.target).1 < 4));
}

#[test]
fn elevated_log_step_is_preserved_while_upper_branch_is_harvested() {
    let mut cells = world();
    log(&mut cells, (1, 0, 0)); // reversible one-block step and part of the tree
    for y in 0..4 {
        log(&mut cells, (2, y, 0));
    }
    log(&mut cells, (2, 2, 1));

    let trees = inspect_world(&cells);
    assert_eq!(trees.len(), 1);
    assert_eq!(trees[0].logs.len(), 6);
    assert!(trees[0].next.iter().any(|step| cell(&step.target).1 >= 2));
    assert!(
        trees[0]
            .next
            .iter()
            .all(|step| cell(&step.target) != (1, 0, 0))
    );
    for step in &trees[0].next {
        assert_eq!(cell(step.route.first().unwrap()), (0, 0, 0));
        assert_ne!(cell(&step.target), cell(step.route.last().unwrap()));
    }
}

#[test]
fn obstructed_headroom_removes_the_elevated_harvest_stance() {
    let mut cells = world();
    // A low stone step supplies the elevated stance. The column prevents a
    // ground stance directly beneath the target, but leaves the elevated ray clear.
    for p in [(1, 0, 0), (2, 0, 0), (2, 1, 0), (2, 2, 0)] {
        cells.insert(
            p,
            Terrain {
                air: false,
                support: true,
                log: None,
            },
        );
    }
    log(&mut cells, (2, 4, 0));
    let open = inspect_world(&cells);
    assert!(
        open[0]
            .next
            .iter()
            .any(|step| cell(&step.target) == (2, 4, 0))
    );

    assert!(
        open[0]
            .next
            .iter()
            .filter(|step| cell(&step.target) == (2, 4, 0))
            .all(|step| step.route.last().unwrap().y > 0.0)
    );
    cells.insert(
        (1, 2, 0),
        Terrain {
            air: false,
            support: true,
            log: None,
        },
    );
    let obstructed = inspect_world(&cells);
    assert!(
        obstructed[0]
            .next
            .iter()
            .all(|step| cell(&step.target) != (2, 4, 0)),
        "a one-block step with blocked headroom cannot justify mining a high log"
    );
}

#[test]
fn missing_canopy_cells_leave_the_tree_view_incomplete() {
    let mut cells = world();
    log(&mut cells, (2, 0, 0));
    log(&mut cells, (2, 1, 0));
    cells.remove(&(2, 2, 0));

    let trees = inspect_world(&cells);
    assert_eq!(trees.len(), 1);
    assert!(trees[0].incomplete);
    assert!(trees[0].status.contains("Partial view") || trees[0].status.contains("Blocked"));
}

#[test]
fn mining_requires_ground_support_clearance_and_current_sightline() {
    use jev_game_engine::wood_tree::can_mine;
    let mut cells = world();
    let position = Position {
        x: 0.5,
        y: 0.0,
        z: 0.5,
    };
    let target = (2, 0, 0);
    log(&mut cells, target);
    assert!(can_mine(&position, target, true, |p| cells
        .get(&p)
        .cloned()));
    assert!(!can_mine(&position, target, false, |p| cells
        .get(&p)
        .cloned()));
    let airborne = Position {
        y: 0.4,
        ..position.clone()
    };
    assert!(!can_mine(&airborne, target, true, |p| cells
        .get(&p)
        .cloned()));
    cells.insert(
        (1, 1, 0),
        Terrain {
            air: false,
            support: true,
            log: None,
        },
    );
    assert!(!can_mine(&position, target, true, |p| cells
        .get(&p)
        .cloned()));
    cells.insert(
        (1, 1, 0),
        Terrain {
            air: true,
            support: false,
            log: None,
        },
    );
    cells.remove(&(0, -1, 0));
    assert!(!can_mine(&position, target, true, |p| cells
        .get(&p)
        .cloned()));
}

/// The live stance from `runs/level1-live/live-dt.json` event 20: a one-cell notch in a
/// hillside. The bot stands at (0.5, 0, 0.5) on the floor (0,-1,0); the wall east of it is
/// (1,0,0) plus (1,1,0) with the block it was sent to break on top, (1,2,0); the wall north
/// of it is (0,0,-1) plus (0,1,-1) with the other observed candidate on top, (0,2,-1).
fn notch() -> BTreeMap<Cell, Terrain> {
    let mut cells = world();
    for p in [(1, 0, 0), (1, 1, 0), (0, 0, -1), (0, 1, -1)] {
        cells.insert(
            p,
            Terrain {
                air: false,
                support: true,
                log: None,
            },
        );
    }
    log(&mut cells, (1, 2, 0));
    log(&mut cells, (0, 2, -1));
    cells
}

#[test]
fn the_stance_the_observation_verified_survives_the_drift_the_live_bot_really_had() {
    use jev_game_engine::wood_tree::{can_mine, facing_point};
    let mut cells = notch();
    let stance = Position {
        x: 0.5,
        y: 0.0,
        z: 0.5,
    };
    let target = (1, 2, 0);
    // The candidate was offered and chosen from the stance centre.
    assert!(can_mine(&stance, target, true, |p| cells.get(&p).cloned()));
    // The live bot did not stop dead on that centre: it coasted 0.20 blocks east and 0.07
    // north (12.70, 66.0, 6.57 against a stance of 12.5, 66.0, 6.5). The log is still two
    // blocks up and one east, and `live-dt.json` event 20 records the attempt dying here with
    // "Gather mining stance, support or sightline changed" -- because a ray to the block
    // *centre* samples the wall beside the bot, not because the block was out of view.
    let drifted = Position {
        x: 0.70,
        y: 0.0,
        z: 0.57,
    };
    assert!(can_mine(&drifted, target, true, |p| cells.get(&p).cloned()));
    // The same drift against the other live candidate, the log above the north wall.
    assert!(can_mine(&drifted, (0, 2, -1), true, |p| cells
        .get(&p)
        .cloned()));
    // The executor aims at the face this eye can see, the same point the observation's own
    // sightline check uses, instead of the centre buried inside the block.
    let aim = facing_point(
        &Position {
            x: drifted.x,
            y: 1.62,
            z: drifted.z,
        },
        target,
        |cell| cells.get(&cell).is_some_and(|terrain| terrain.air),
    );
    assert_eq!((aim.x, aim.y, aim.z), (1.05, 2.5, 0.5));
    // A block that really covers that face is still refused, from either position.
    cells.insert(
        (0, 2, 0),
        Terrain {
            air: false,
            support: true,
            log: None,
        },
    );
    assert!(!can_mine(&drifted, target, true, |p| cells
        .get(&p)
        .cloned()));
    assert!(!can_mine(&stance, target, true, |p| cells.get(&p).cloned()));
}

#[test]
fn sequential_harvest_preserves_root_step_and_original_tree_identity_across_splits() {
    use jev_game_engine::wood_tree::TreeMemory;
    let mut cells = world();
    for y in 0..5 {
        log(&mut cells, (2, y, 0));
    }
    let mut memory = TreeMemory::default();
    let mut tree_id = None;
    for remaining in (1..=5).rev() {
        let trees = memory.update(1, inspect_world(&cells), |p, name| {
            cells.get(&p).map(|t| t.log.as_deref() == Some(name))
        });
        assert_eq!(
            trees.len(),
            1,
            "severed canopy remains associated with original trunk"
        );
        assert_eq!(trees[0].logs.len(), remaining);
        assert_eq!(*tree_id.get_or_insert(trees[0].id), trees[0].id);
        let step = trees[0]
            .next
            .first()
            .expect("a safe ordered harvest remains possible");
        let target = cell(&step.target);
        if remaining > 1 {
            assert_ne!(target, (2, 0, 0), "root step retained for upper wood");
        }
        cells.insert(
            target,
            Terrain {
                air: true,
                support: false,
                log: None,
            },
        );
    }
    let final_view = memory.update(1, inspect_world(&cells), |p, name| {
        cells.get(&p).map(|t| t.log.as_deref() == Some(name))
    });
    assert_eq!(final_view[0].id, tree_id.unwrap());
    assert!(final_view[0].logs.is_empty());
    assert!(!final_view[0].incomplete);
}

#[test]
fn remembered_wood_is_unknown_when_unloaded_and_clears_only_on_observed_removal_or_new_world() {
    use jev_game_engine::wood_tree::TreeMemory;
    let mut cells = world();
    log(&mut cells, (2, 0, 0));
    log(&mut cells, (2, 1, 0));
    let mut memory = TreeMemory::default();
    let first = memory.update(1, inspect_world(&cells), |p, name| {
        cells.get(&p).map(|t| t.log.as_deref() == Some(name))
    });
    let unseen = memory.update(1, Vec::new(), |_, _| None);
    assert_eq!(unseen[0].id, first[0].id);
    assert_eq!(unseen[0].logs.len(), 2);
    assert!(unseen[0].incomplete);
    assert!(unseen[0].next.is_empty());
    let removed = memory.update(1, Vec::new(), |_, _| Some(false));
    assert!(removed[0].logs.is_empty());
    assert!(memory.update(2, Vec::new(), |_, _| None).is_empty());
}

#[test]
fn a_remembered_route_protects_another_trees_log_until_its_target_is_visibly_removed() {
    use jev_game_engine::wood_tree::{Access, TreeMemory, TreeView};

    for other_wood in [OAK, "minecraft:birch_log"] {
        let mut memory = TreeMemory::default();
        let upper_target = (2, 2, 0);
        let support_log = (1, 0, 0);
        let upper = TreeView {
            id: 0,
            wood: OAK.into(),
            logs: vec![Position {
                x: 2.5,
                y: 2.5,
                z: 0.5,
            }],
            incomplete: false,
            capacity_loss: false,
            status: String::new(),
            diagnostics: Default::default(),
            next: vec![Access {
                target: Position {
                    x: 2.5,
                    y: 2.5,
                    z: 0.5,
                },
                log: OAK.into(),
                route: vec![
                    Position {
                        x: 0.5,
                        y: 0.0,
                        z: 0.5,
                    },
                    Position {
                        x: 1.5,
                        y: 1.0,
                        z: 0.5,
                    },
                ],
            }],
        };
        let support = TreeView {
            id: 0,
            wood: other_wood.into(),
            logs: vec![Position {
                x: 1.5,
                y: 0.5,
                z: 0.5,
            }],
            incomplete: false,
            capacity_loss: false,
            status: String::new(),
            diagnostics: Default::default(),
            next: vec![Access {
                target: Position {
                    x: 1.5,
                    y: 0.5,
                    z: 0.5,
                },
                log: other_wood.into(),
                route: vec![Position {
                    x: 0.5,
                    y: 0.0,
                    z: 0.5,
                }],
            }],
        };

        let first = memory.update(1, vec![upper, support.clone()], |p, name| {
            Some((p == upper_target && name == OAK) || (p == support_log && name == other_wood))
        });
        let upper_id = first
            .iter()
            .find(|tree| tree.logs.iter().any(|p| cell(p) == upper_target))
            .unwrap()
            .id;
        let support_id = first
            .iter()
            .find(|tree| tree.logs.iter().any(|p| cell(p) == support_log))
            .unwrap()
            .id;
        assert_ne!(
            upper_id, support_id,
            "the logs belong to separate memory entries"
        );
        assert!(
            first
                .iter()
                .find(|tree| tree.id == support_id)
                .unwrap()
                .next
                .is_empty(),
            "a route for another unresolved tree needs this support log"
        );

        let occluded = memory.update(1, vec![support.clone()], |p, _| {
            (p == support_log).then_some(true)
        });
        assert!(
            occluded
                .iter()
                .find(|tree| tree.id == support_id)
                .unwrap()
                .next
                .is_empty(),
            "an unknown cell does not prove the upper target was removed"
        );

        let reconciled = memory.update(1, vec![support], |p, _| {
            if p == upper_target {
                Some(false)
            } else {
                Some(true)
            }
        });
        assert!(
            reconciled
                .iter()
                .find(|tree| tree.id == upper_id)
                .unwrap()
                .logs
                .is_empty()
        );
        assert!(
            reconciled
                .iter()
                .find(|tree| tree.id == support_id)
                .unwrap()
                .next
                .iter()
                .any(|step| cell(&step.target) == support_log),
            "visible removal releases the support dependency"
        );
    }
}

#[test]
fn fully_reconciled_entries_release_capacity_for_newly_observed_trees() {
    use jev_game_engine::wood_tree::{TreeMemory, TreeView};

    let mut memory = TreeMemory::default();
    let observed = |x: i32| TreeView {
        id: 0,
        wood: OAK.into(),
        logs: vec![Position {
            x: f64::from(x) + 0.5,
            y: 0.5,
            z: 0.5,
        }],
        incomplete: false,
        capacity_loss: false,
        status: String::new(),
        diagnostics: Default::default(),
        next: Vec::new(),
    };
    let first = memory.update(1, (0..8).map(|i| observed(i * 2)).collect(), |_, _| {
        Some(true)
    });
    assert_eq!(first.len(), 8);
    let cleared = memory.update(1, Vec::new(), |_, _| Some(false));
    assert!(cleared.iter().all(|tree| tree.logs.is_empty()));

    let ninth = memory.update(1, vec![observed(16)], |p, _| Some(p == (16, 0, 0)));
    assert!(
        ninth
            .iter()
            .any(|tree| tree.logs.iter().any(|p| cell(p) == (16, 0, 0))),
        "reconciled entries must not consume all eight memory slots"
    );
}

#[test]
fn scan_beyond_the_tree_cap_keeps_enumerated_harvests_and_reports_the_drop() {
    use jev_game_engine::wood_tree::{MAX_TREES, TreeMemory};

    // A real forest inside the scan window: thirty isolated trunks. A spruce forest
    // denser than the old eight-tree cap is exactly where the wood goal runs.
    let mut cells = world();
    for x in 0..6 {
        for z in 0..5 {
            log(&mut cells, (2 + x * 2, 0, 2 + z * 2));
        }
    }
    assert_eq!(cells.values().filter(|t| t.log.is_some()).count(), 30);

    let scanned = inspect_world(&cells);
    assert_eq!(
        scanned.len(),
        MAX_TREES,
        "the scan must enumerate its whole cap before dropping anything"
    );
    assert!(
        scanned.iter().all(|tree| !tree.capacity_loss),
        "a tree the scan enumerated in full must not inherit another tree's loss"
    );
    assert!(
        scanned.iter().any(|tree| !tree.next.is_empty()),
        "a forest larger than the cap must still offer its verified harvest"
    );
    assert!(
        scanned
            .iter()
            .all(|tree| tree.status.contains("beyond the 24-tree scan cap")),
        "every enumerated tree must report that components were dropped"
    );
    assert!(
        scanned
            .iter()
            .all(|tree| tree.status.contains("6 further observed tree(s)")),
        "the status must state how many components were not enumerated"
    );

    let mut memory = TreeMemory::default();
    let remembered = memory.update(1, scanned, |p, name| {
        cells.get(&p).map(|t| t.log.as_deref() == Some(name))
    });
    assert!(
        remembered.iter().any(|tree| !tree.next.is_empty()),
        "dropping unenumerated components must not block a verified harvest"
    );
    assert!(
        remembered.iter().all(|tree| !tree.capacity_loss),
        "a cap drop is not this tree's own truncation"
    );
}

#[test]
fn component_cell_limit_reports_capacity_loss_to_memory() {
    use jev_game_engine::wood_tree::TreeMemory;

    let mut cells = world();
    for x in 2..=9 {
        for z in 0..=7 {
            for y in 0..=1 {
                log(&mut cells, (x, y, z));
            }
        }
    }
    log(&mut cells, (10, 0, 0));

    let scanned = inspect_world(&cells);
    assert!(scanned.iter().any(|tree| tree.capacity_loss));
    let mut memory = TreeMemory::default();
    let remembered = memory.update(1, scanned, |p, name| {
        cells.get(&p).map(|t| t.log.as_deref() == Some(name))
    });
    assert!(remembered.iter().all(|tree| tree.next.is_empty()));
    assert!(
        remembered
            .iter()
            .all(|tree| tree.status.contains("capacity"))
    );
}

#[test]
fn collection_route_is_reversible_and_rejects_unreachable_drop() {
    use jev_game_engine::wood_tree::pickup_route;
    let mut cells = world();
    let origin = Position {
        x: 0.5,
        y: 0.0,
        z: 0.5,
    };
    let item = Position {
        x: 4.5,
        y: 0.1,
        z: 0.5,
    };
    let route = pickup_route(&origin, &item, |p| cells.get(&p).cloned()).unwrap();
    assert_eq!(cell(&route[0]), cell(&origin));
    assert!(route.len() > 1);
    for pair in route.windows(2) {
        let a = cell(&pair[0]);
        let b = cell(&pair[1]);
        assert_eq!((a.0 - b.0).abs() + (a.2 - b.2).abs(), 1);
        assert!((a.1 - b.1).abs() <= 1);
    }
    // Remove the floor under every stance within pickup range: no blind descent.
    for x in 3..=5 {
        for z in -1..=1 {
            cells.remove(&(x, -1, z));
        }
    }
    assert!(pickup_route(&origin, &item, |p| cells.get(&p).cloned()).is_none());
    assert!(
        pickup_route(&origin, &Position { y: 7.0, ..item }, |p| cells
            .get(&p)
            .cloned())
        .is_none()
    );
}

#[test]
fn waypoint_requires_actual_landing_and_rejects_sideways_deviation() {
    use jev_game_engine::wood_tree::{inside_transition, landed};
    let from = Position {
        x: 0.5,
        y: 0.0,
        z: 0.5,
    };
    let to = Position {
        x: 1.5,
        y: 1.0,
        z: 0.5,
    };
    assert!(landed(&to, &to, true));
    assert!(!landed(&to, &to, false));
    let airborne = Position {
        y: 1.4,
        ..to.clone()
    };
    assert!(!landed(&airborne, &to, true));
    assert!(inside_transition(&airborne, &from, &to));
    assert!(inside_transition(&airborne, &to, &from));
    assert!(!inside_transition(
        &Position { z: 1.0, ..airborne },
        &from,
        &to
    ));
    assert!(!inside_transition(
        &Position {
            y: -0.5,
            ..from.clone()
        },
        &from,
        &to
    ));
    // Live evidence: after a real walk the bot stood 0.26 blocks past the next cell centre, which
    // is inside its own 0.6-wide body. That step must count as being on the verified edge; the old
    // 0.2 box rejected it and the wood skill aborted with "Wood movement left verified transition".
    let overshoot = Position {
        y: 1.0,
        ..to.clone()
    };
    assert!(inside_transition(
        &Position {
            x: to.x + 0.26,
            ..overshoot.clone()
        },
        &from,
        &to
    ));
    // One centimetre past the player's own half-width is outside the swept volume and stays out.
    assert!(!inside_transition(
        &Position {
            x: to.x + 0.31,
            ..overshoot.clone()
        },
        &from,
        &to
    ));
}
