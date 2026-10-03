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
            store.set_player(Player::new(0, feet, 0.0, 0.0));
            let mut input = PlayerInput { player: 0, position: feet, yaw: 0.0, pitch: 0.0, flags: 0 };
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
/// throttle for ten seconds each (`script` says when they jump and crouch),
/// reporting each tick to the server's step: every report is accepted, on the
/// ground, in a jump or in a fall off a ledge. Returns the ticks played and
/// the ticks spent in the air; `None` without the game's maps.
fn every_start_accepted(script: impl Fn(u32) -> (bool, bool)) -> Option<(u64, u64)> {
    use halo_sim::walk::{walk, Body, Controls};

    let dir = std::env::var_os("HALO_MAP_DIR")?;
    let (mut ticks, mut in_the_air) = (0u64, 0u64);
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
                store.set_player(Player::new(0, body.position, yaw, 0.0));
                for tick in 0..300 {
                    let (jump, crouch) = script(tick);
                    walk(&map, &mut body, &Controls { forward: 1.0, strafe: 0.0, yaw, pitch: 0.0, jump, crouch });
                    let flags = if body.crouched() { halo_sim::FLAG_CROUCHED } else { 0 };
                    let input = PlayerInput { player: 0, position: body.position, yaw, pitch: 0.0, flags };
                    let events = step(&mut store, &[input], &map, &mut Rng::seeded(0));
                    assert_eq!(
                        events,
                        [Event::MoveAccepted { player: 0 }],
                        "{name}: start {i}, heading {direction}, tick {tick}, at {:?}, airborne {}",
                        body.position,
                        body.airborne
                    );
                    ticks += 1;
                    in_the_air += body.airborne as u64;
                }
            }
        }
    }
    Some((ticks, in_the_air))
}

#[test]
fn on_every_map_a_player_walking_from_every_start_is_accepted_by_the_server_including_falls_off_ledges() {
    let Some((ticks, in_the_air)) = every_start_accepted(|_| (false, false)) else {
        eprintln!("HALO_MAP_DIR is not set: skipping, this test needs the game's own map files");
        return;
    };
    println!("{ticks} ticks of walking accepted, {in_the_air} of them in the air (falling off ledges)");
    assert!(ticks > 100_000, "only {ticks} ticks were walked");
    assert!(in_the_air > 500, "only {in_the_air} ticks in the air: no ledge was walked off");
}

#[test]
fn on_every_map_a_player_jumping_while_walking_from_every_start_is_accepted_by_the_server() {
    let Some((ticks, in_the_air)) = every_start_accepted(|tick| (tick % 45 == 20, false)) else {
        eprintln!("HALO_MAP_DIR is not set: skipping, this test needs the game's own map files");
        return;
    };
    println!("{ticks} ticks of walking and jumping accepted, {in_the_air} of them in the air");
    assert!(in_the_air as f32 > 0.2 * ticks as f32, "{in_the_air} of {ticks} ticks in the air");
}

#[test]
fn on_every_map_a_player_crouching_and_jumping_from_every_start_is_accepted_by_the_server() {
    let Some((ticks, in_the_air)) = every_start_accepted(|tick| (tick % 55 == 10, tick % 70 > 25)) else {
        eprintln!("HALO_MAP_DIR is not set: skipping, this test needs the game's own map files");
        return;
    };
    println!("{ticks} ticks of crouching, walking and jumping accepted, {in_the_air} of them in the air");
    assert!(in_the_air > 0);
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

/// What the falling damage reads of a fall: the speed the player hits the
/// ground at (`Body::landing_velocity`) against the tags' thresholds. Blood
/// Gulch's open field, dropped from several heights: the damage starts with a
/// fall of the tag's 3 world units and is greatest at its 6, and the hard
/// landing starts a little above where the damage does.
#[test]
fn a_fall_hits_the_ground_at_the_speed_the_falling_damage_of_the_tags_reads() {
    use halo_sim::walk::{walk, Body, Controls};

    let Some(dir) = std::env::var_os("HALO_MAP_DIR") else {
        eprintln!("HALO_MAP_DIR is not set: skipping, this test needs the game's own map files");
        return;
    };
    let path = std::path::Path::new(&dir).join("bloodgulch.map");
    let map = MapData::from(HaloMap::from_path(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())));
    let m = map.movement;
    let ground = map.collision.ray_down([77.9, -166.2, 3.0], 6.0).expect("the open field").z;
    let landing_from = |height: f32| {
        let mut body = Body::at([77.9, -166.2, ground + height]);
        for _ in 0..400 {
            walk(&map, &mut body, &Controls::standing(0.0));
            if body.landing_velocity > 0.0 {
                return body.landing_velocity;
            }
        }
        panic!("a fall from {height} never landed");
    };
    // the tags give the distances: v = sqrt(2 g d)
    let distance = |v: f32| v * v / (2.0 * halo_sim::walk::GRAVITY);
    println!(
        "falling damage from {:.2} to {:.2} world units; killing fall {:.2}; hard landing from {:.2}",
        distance(m.minimum_damage_velocity),
        distance(m.maximum_damage_velocity),
        distance(m.maximum_falling_velocity),
        distance(m.minimum_hard_landing_velocity / 30.0)
    );
    assert!((distance(m.minimum_damage_velocity) - 3.0).abs() < 0.05);
    assert!((distance(m.maximum_damage_velocity) - 6.0).abs() < 0.05);
    for (height, hurts) in [(2.0, false), (2.9, false), (3.2, true), (4.5, true), (6.5, true)] {
        let v = landing_from(height);
        println!("fall of {height}: lands at {v:.4} a tick ({:.3} a second)", v * 30.0);
        assert_eq!(v > m.minimum_damage_velocity, hurts, "a fall of {height} lands at {v}");
    }
    // the damage scale the engine computes: 0 at the first, 1 at the second
    let scale = |v: f32| {
        ((v - m.minimum_damage_velocity) / (m.maximum_damage_velocity - m.minimum_damage_velocity)).clamp(0.0, 1.0)
    };
    assert!(scale(landing_from(3.2)) < 0.15 && scale(landing_from(4.5)) > 0.3 && scale(landing_from(6.5)) == 1.0);
}
