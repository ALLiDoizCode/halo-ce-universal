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
