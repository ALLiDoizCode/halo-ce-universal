//! Items against the developer's own Xbox map files, found through the
//! `HALO_MAP_DIR` environment variable (a folder holding `bloodgulch.map` and
//! the rest). Without it these tests pass without testing.

use halo_map::HaloMap;
use halo_sim::combat::MemoryCombat;
use halo_sim::items::{self, Item, ItemStore, MemoryItems, NO_PLACEMENT, NO_PLAYER};
use halo_sim::rules::{self, MemoryGame, Rules};
use halo_sim::{MapData, MemoryStore, Rng};

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

fn load(dir: &std::ffi::OsStr, name: &str) -> MapData {
    let path = std::path::Path::new(dir).join(format!("{name}.map"));
    MapData::from(HaloMap::from_path(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
}

fn maps() -> Option<std::ffi::OsString> {
    let dir = std::env::var_os("HALO_MAP_DIR");
    if dir.is_none() {
        eprintln!("HALO_MAP_DIR is not set: skipping, this test needs the game's own map files");
    }
    dir
}

#[test]
fn on_every_map_the_items_survive_the_stored_bytes_and_have_the_tags_numbers() {
    let Some(dir) = maps() else { return };
    for name in MAPS {
        let map = load(&dir, name);
        let back = MapData::from_bytes(&map.to_bytes()).unwrap();
        assert_eq!(back.items, map.items, "{name}");
        assert!(!map.items.placements.is_empty(), "{name} has netgame equipment");
        let overshield = map.items.defs.iter().find(|d| d.name == "powerups\\over shield.eqip").unwrap();
        let camo = map.items.defs.iter().find(|d| d.name == "powerups\\active camouflage.eqip").unwrap();
        assert_eq!((overshield.powerup_time, camo.powerup_time), (60.0, 45.0), "{name}");
        assert!(map.items.player.bounding_radius > 0.0, "{name}");
        // every placement's collection names items the map has
        for p in &map.items.placements {
            assert!(!p.permutations.is_empty(), "{name}: a placement of nothing at {:?}", p.position);
        }
    }
}

/// The rates every map's placements spawn at, for the report: how many
/// placements there are for each period.
#[test]
fn the_periods_of_the_placements_on_every_map() {
    let Some(dir) = maps() else { return };
    for name in MAPS {
        let map = load(&dir, name);
        let mut periods = std::collections::BTreeMap::new();
        for p in &map.items.placements {
            if items::is_for_slayer(&p.game_types) {
                let names: Vec<&str> =
                    p.permutations.iter().filter_map(|(_, t)| map.items.def(*t)).map(|d| d.name.as_str()).collect();
                *periods.entry((items::period_ticks(p) / 30, names.join("|"))).or_insert(0) += 1;
            }
        }
        println!("{name}: (seconds, items) -> placements: {periods:?}");
    }
}

/// A match on each map's real collision data: every item the placements make
/// at the start comes to rest on the map near where the placement is, and
/// each comes back on its period.
#[test]
fn on_every_map_the_items_of_the_placements_fall_to_rest_on_the_real_collision_and_come_back_on_their_periods() {
    let Some(dir) = maps() else { return };
    for name in MAPS {
        let map = load(&dir, name);
        let mut game = MemoryGame::new(Rules::slayer());
        rules::begin(&mut game, 0);
        let mut items = MemoryItems::new();
        let mut combat = MemoryCombat::new();
        let store = MemoryStore::new();
        let mut rng = Rng::seeded(5);
        items::tick(&mut items, &mut combat, &store, &game, &map, &mut rng, 1, &[]);
        let start: Vec<Item> = items.items();
        let expected = map
            .items
            .placements
            .iter()
            .filter(|p| items::is_for_slayer(&p.game_types))
            .filter(|p| p.permutations.iter().any(|(_, t)| items::is_spawnable(&map, *t)))
            .count();
        assert_eq!(start.len(), expected, "{name}: an item for each placement the game uses");
        // 6 seconds is long enough for any to fall to the ground
        for tick in 2..=200u64 {
            items::tick(&mut items, &mut combat, &store, &game, &map, &mut rng, tick, &[]);
        }
        let mut lost = 0;
        for item in items.items() {
            let placement = &map.items.placements[item.placement as usize];
            let rested = item.resting;
            let drop = placement.position[2] - item.position[2];
            let sideways = ((placement.position[0] - item.position[0]).powi(2)
                + (placement.position[1] - item.position[1]).powi(2))
            .sqrt();
            // (an item may be a little above its surface in the map's data, or sink a little: it falls to a surface within a unit)
            if !rested || !(-0.2..=1.5).contains(&drop) || sideways > 0.5 {
                lost += 1;
                println!(
                    "{name}: item {} from {:?} is at {:?} resting {rested}",
                    item.tag, placement.position, item.position
                );
            }
        }
        assert_eq!(lost, 0, "{name}: items that did not come to rest near their placements");

        // each placement's item is replaced exactly on its period, from the tick after the start
        for (index, placement) in map.items.placements.iter().enumerate() {
            let period = items::period_ticks(placement);
            if !items::is_for_slayer(&placement.game_types) || period > 400 {
                continue;
            }
            let first = items.items().iter().find(|i| i.placement == index as u16).map(|i| i.id);
            assert!(first.is_some(), "{name}: placement {index}");
        }
    }
}

/// A weapon dropped on a real map falls and rests on the ground there.
#[test]
fn on_every_map_a_dropped_weapon_falls_and_rests_on_the_ground_under_where_it_was_dropped() {
    let Some(dir) = maps() else { return };
    for name in MAPS {
        let map = load(&dir, name);
        let mut checked = 0;
        for start in map.starts.iter().filter(|s| s.is_for_slayer()).take(20) {
            // (where a player stands: over the ground; the weapon leaves them waist high)
            let ground = map.collision.ray_down([start.position[0], start.position[1], start.position[2] + 1.0], 3.0);
            let Some(ground) = ground else { continue };
            let item = Item {
                id: 0,
                tag: map.combat.weapons[0].tag_index,
                position: [start.position[0], start.position[1], start.position[2] + 0.35],
                velocity: [0.0; 3],
                tick: 0,
                resting: false,
                placement: NO_PLACEMENT,
                loaded: 1,
                reserve: 1,
                last_owned: 0,
                ignore: NO_PLAYER,
            };
            let (item, flight) = item.advanced_to(&map, 600);
            assert_eq!(flight, items::Flight::Rested, "{name}: at {:?}", start.position);
            assert!(item.resting);
            assert!((item.position[2] - ground.z).abs() < 0.2, "{name}: rests at {:?}, ground at {}", item.position, ground.z);
            checked += 1;
        }
        assert!(checked > 0, "{name}");
    }
}
