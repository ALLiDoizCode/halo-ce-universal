//! The netgame flags of every game type against the developer's own Xbox map files, found through the
//! `HALO_MAP_DIR` environment variable (a folder holding `bloodgulch.map` and
//! the rest). Without it these tests pass without testing.

use halo_map::{flag_type, HaloMap};
use halo_sim::MapData;

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
fn on_every_map_the_flags_of_each_game_type_are_read_and_survive_the_stored_bytes() {
    let Some(dir) = maps() else { return };
    let mut seen = std::collections::BTreeMap::<i16, usize>::new();
    for name in MAPS {
        let map = load(&dir, name);
        let back = MapData::from_bytes(&map.to_bytes()).unwrap();
        assert_eq!(back.netgame_flags, map.netgame_flags, "{name}");
        for f in &map.netgame_flags {
            *seen.entry(f.flag_type).or_default() += 1;
        }
        let of = |t: i16| map.netgame_flags.iter().filter(|f| f.flag_type == t).count();
        eprintln!(
            "{name}: ctf {} oddball {} race track {} hill {}",
            of(flag_type::CTF_FLAG),
            of(flag_type::ODDBALL_BALL_SPAWN),
            of(flag_type::RACE_TRACK),
            of(flag_type::HILL)
        );
    }
    for (kind, what) in [
        (flag_type::CTF_FLAG, "CTF flags"),
        (flag_type::ODDBALL_BALL_SPAWN, "Oddball ball spawns"),
        (flag_type::RACE_TRACK, "Race checkpoints"),
        (flag_type::HILL, "King hills"),
    ] {
        assert!(seen.get(&kind).is_some_and(|&n| n > 0), "none of the 13 maps has {what}");
    }
}
