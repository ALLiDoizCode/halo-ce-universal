//! The step against the developer's own Xbox map files, found through the
//! `HALO_MAP_DIR` environment variable (a folder holding `bloodgulch.map` and
//! the rest). Without it these tests pass without testing.

use halo_map::HaloMap;
use halo_sim::{step, Event, MapData, MemoryStore, Player, PlayerInput, RejectReason, Rng, Store};

const MAPS: [&str; 13] = [
    "beavercreek",
    "bloodgulch",
    "boardingaction",
    "carousel",
    "chillout",
    "damnation",
    "hangemhigh",
    "longest",
    "prisoner",
    "putput",
    "ratrace",
    "sidewinder",
    "wizard",
];

#[test]
fn on_every_map_a_player_standing_at_a_start_stays_valid_and_cannot_move_into_the_ground() {
    let Some(dir) = std::env::var_os("HALO_MAP_DIR") else {
        eprintln!("HALO_MAP_DIR is not set: skipping, this test needs the game's own map files");
        return;
    };
    let (mut checked, mut inside_a_wall) = (0, 0);
    for name in MAPS {
        let path = std::path::Path::new(&dir).join(format!("{name}.map"));
        let halo_map = HaloMap::from_path(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let starts = halo_map.player_starts.clone();
        let map = MapData::from(halo_map);

        for (i, start) in starts.iter().enumerate() {
            let above = [start.position[0], start.position[1], start.position[2] + 1.0];
            let ground =
                map.collision.ray_down(above, 3.0).unwrap_or_else(|| panic!("{name}: start {i} has no ground"));
            let feet = [start.position[0], start.position[1], ground.z + 0.01];
            // (a few authored starts are closer to a wall than a player is wide:
            // a player put there is a little inside it, which spawning is to mend)
            if halo_sim::walk::footing(&map, feet).penetration > halo_sim::PENETRATION_TOLERANCE {
                inside_a_wall += 1;
                continue;
            }
            checked += 1;

            let mut store = MemoryStore::new();
            store.set_player(Player { id: 0, position: feet, yaw: 0.0, pitch: 0.0 });
            let mut input = PlayerInput { player: 0, position: feet, yaw: 0.0, pitch: 0.0 };
            let events = step(&mut store, &[input], &map, &mut Rng::seeded(0));
            assert_eq!(events, [Event::MoveAccepted { player: 0 }], "{name}: start {i}");

            input.position[2] -= 0.1;
            let events = step(&mut store, &[input], &map, &mut Rng::seeded(0));
            let reason = RejectReason::ThroughSurface;
            assert_eq!(events, [Event::MoveRejected { player: 0, reason }], "{name}: start {i}");
            assert_eq!(store.player(0).unwrap().position, feet);
        }
    }
    println!("{checked} starts checked, {inside_a_wall} start a little inside a wall");
    assert!(checked > 10 * inside_a_wall, "{inside_a_wall} of {} starts are inside a wall", checked + inside_a_wall);
}

/// Every on-foot player a map starts, walking in eight directions at full
/// throttle for ten seconds each, reporting each tick to the server's step:
/// every report from a player on the ground is accepted. (A player who walks
/// off a ledge is in the air: validating a fall is the jumping and falling
/// work's, and the run ends there.)
#[test]
fn on_every_map_a_player_walking_from_every_start_is_accepted_by_the_server_while_on_the_ground() {
    use halo_sim::walk::{walk, Body, Controls};

    let Some(dir) = std::env::var_os("HALO_MAP_DIR") else {
        eprintln!("HALO_MAP_DIR is not set: skipping, this test needs the game's own map files");
        return;
    };
    let (mut ticks_on_ground, mut falls) = (0u64, 0u32);
    for name in MAPS {
        let path = std::path::Path::new(&dir).join(format!("{name}.map"));
        let halo_map = HaloMap::from_path(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let starts = halo_map.player_starts.clone();
        let map = MapData::from(halo_map);

        for (i, start) in starts.iter().enumerate() {
            for direction in 0..8 {
                let yaw = direction as f32 * core::f32::consts::FRAC_PI_4;
                let mut body = Body::at([start.position[0], start.position[1], start.position[2] + 0.1]);
                // let the player settle first, and report from where they are
                for _ in 0..30 {
                    walk(&map, &mut body, &Controls::standing(yaw));
                }
                if body.airborne {
                    continue;
                }
                let mut store = MemoryStore::new();
                store.set_player(Player { id: 0, position: body.position, yaw, pitch: 0.0 });
                for tick in 0..300 {
                    walk(&map, &mut body, &Controls { forward: 1.0, strafe: 0.0, yaw, pitch: 0.0 });
                    if body.airborne {
                        falls += 1;
                        break;
                    }
                    let input = PlayerInput { player: 0, position: body.position, yaw, pitch: 0.0 };
                    let events = step(&mut store, &[input], &map, &mut Rng::seeded(0));
                    assert_eq!(
                        events,
                        [Event::MoveAccepted { player: 0 }],
                        "{name}: start {i}, heading {direction}, tick {tick}, at {:?}",
                        body.position
                    );
                    ticks_on_ground += 1;
                }
            }
        }
    }
    println!("{ticks_on_ground} ticks of walking on the ground accepted, {falls} runs ended by walking off an edge");
    assert!(ticks_on_ground > 100_000, "only {ticks_on_ground} ticks were walked");
}

#[test]
fn every_maps_tags_give_the_same_movement_values_and_the_stored_bytes_keep_them() {
    let Some(dir) = std::env::var_os("HALO_MAP_DIR") else {
        eprintln!("HALO_MAP_DIR is not set: skipping, this test needs the game's own map files");
        return;
    };
    let mut first = None;
    for name in MAPS {
        let path = std::path::Path::new(&dir).join(format!("{name}.map"));
        let map = MapData::from(HaloMap::from_path(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())));
        assert!(map.movement.is_sane(), "{name}: {:?}", map.movement);
        // the multiplayer maps share one globals tag's values
        assert_eq!(*first.get_or_insert(map.movement), map.movement, "{name}");
        assert_eq!(MapData::from_bytes(&map.to_bytes()).unwrap().movement, map.movement, "{name}");
        // the speed the server holds a player to is above the running speed, and not by much
        let run = map.movement.run_forward_speed;
        assert!(map.max_move_speed() > run && map.max_move_speed() < 3.0 * run, "{name}: {}", map.max_move_speed());
    }
}

#[test]
fn on_every_map_the_stored_bytes_rebuild_the_same_collision() {
    let Some(dir) = std::env::var_os("HALO_MAP_DIR") else {
        eprintln!("HALO_MAP_DIR is not set: skipping, this test needs the game's own map files");
        return;
    };
    for name in MAPS {
        let path = std::path::Path::new(&dir).join(format!("{name}.map"));
        let map = MapData::from(HaloMap::from_path(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())));
        let back = MapData::from_bytes(&map.to_bytes()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(back.world_bounds, map.world_bounds, "{name}");
        assert_eq!(back.collision, map.collision, "{name}");
    }
}
