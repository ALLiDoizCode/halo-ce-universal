//! Spawning against the developer's own Xbox map files, found through the
//! `HALO_MAP_DIR` environment variable (a folder holding `bloodgulch.map` and
//! the rest). Without it these tests pass without testing.

use halo_map::HaloMap;
use halo_sim::rules::{enter, play, GameEvent, GameStore, MemoryGame, Rules};
use halo_sim::{MapData, MemoryStore, Rng, Store};

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

fn load(dir: &std::ffi::OsStr, name: &str) -> (HaloMap, MapData) {
    let path = std::path::Path::new(dir).join(format!("{name}.map"));
    let halo_map = HaloMap::from_path(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let map = MapData::from(halo_map.clone());
    (halo_map, map)
}

#[test]
fn on_every_map_the_starting_locations_are_settled_and_survive_the_stored_bytes() {
    let Some(dir) = std::env::var_os("HALO_MAP_DIR") else {
        eprintln!("HALO_MAP_DIR is not set: skipping, this test needs the game's own map files");
        return;
    };
    let (mut starts, mut unusable) = (0, 0);
    for name in MAPS {
        let (halo_map, map) = load(&dir, name);
        assert_eq!(map.starts.len(), halo_map.player_starts.len(), "{name}");
        assert_eq!(MapData::from_bytes(&map.to_bytes()).unwrap().starts, map.starts, "{name}");
        assert!(map.starts.iter().any(|s| s.is_for_slayer()), "{name} has no start for Slayer");
        for (i, start) in map.starts.iter().enumerate() {
            starts += 1;
            let footing = halo_sim::walk::footing(&map, start.position);
            // (a start a few millimetres into a wall is where the engine's own players can stand)
            if footing.penetration > 0.02 || !footing.supported {
                unusable += 1;
                println!("{name}: start {i} at {:?} is not somewhere a player can stand: {footing:?}", start.position);
            }
        }
    }
    println!("{starts} starts, {unusable} where a player cannot stand");
    assert_eq!(unusable, 0);
}

/// A crowd three times the starts of the game, joining at once, on every map,
/// in a game with teams and one without: after twelve seconds of waves
/// everyone is somewhere a player can stand and nobody is on top of another.
#[test]
fn on_every_map_a_crowd_of_more_players_than_starts_is_spawned_in_waves_without_overlap() {
    let Some(dir) = std::env::var_os("HALO_MAP_DIR") else {
        eprintln!("HALO_MAP_DIR is not set: skipping, this test needs the game's own map files");
        return;
    };
    let ticks = 12 * halo_sim::TICKS_PER_SECOND as u64;
    for name in MAPS {
        let (_, map) = load(&dir, name);
        let slayer_starts = map.starts.iter().filter(|s| s.is_for_slayer()).count();
        for rules in [Rules::slayer(), Rules::team_slayer()] {
            let players = (3 * slayer_starts) as u16;
            let mut store = MemoryStore::new();
            let mut game = MemoryGame::new(rules);
            let mut rng = Rng::seeded(11);
            for id in 0..players {
                enter(&mut game, id, (id % 2) as u8, 0);
            }
            let (mut at_a_start, mut in_waves, mut waits) = (0, 0, 0);
            for tick in 1..=ticks {
                let outcome = play(&mut store, &mut game, &map, &mut rng, tick, &[], &[]);
                for e in outcome.events {
                    match e {
                        GameEvent::Spawned { wave: false, .. } => at_a_start += 1,
                        GameEvent::Spawned { wave: true, .. } => in_waves += 1,
                        GameEvent::Waiting { .. } => waits += 1,
                        _ => {}
                    }
                }
            }
            let alive: Vec<[f32; 3]> = game
                .contestants()
                .iter()
                .filter(|c| c.is_alive())
                .map(|c| store.player(c.id).unwrap().position)
                .collect();
            println!(
                "{name} teams {}: {players} players, {slayer_starts} starts: {at_a_start} at a free start, {in_waves} in \
                 waves, {waits} told to wait, {} alive after 12 s",
                rules.teams,
                alive.len()
            );
            assert!(alive.len() as u16 > players / 2, "{name}: only {} of {players} spawned in 12 s", alive.len());
            for (i, a) in alive.iter().enumerate() {
                let footing = halo_sim::walk::footing(&map, *a);
                assert!(
                    footing.penetration <= 0.02 && footing.supported,
                    "{name}: a player was spawned at {a:?}: {footing:?}"
                );
                for b in &alive[i + 1..] {
                    let d = ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
                    assert!(d >= 2.0 * map.movement.collision_radius, "{name}: two players {d} apart at {a:?}");
                }
            }
        }
    }
}
