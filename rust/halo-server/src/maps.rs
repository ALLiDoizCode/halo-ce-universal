//! Where a match's map comes from: the operator's own `.map` files.

use std::path::PathBuf;

use halo_sim::MapData;

/// A map ready for a match: what the module keeps (its collision data and its
/// player starting locations, which is where the game's rules spawn players)
/// and how many players it holds unless the operator says otherwise.
#[derive(Debug, Clone)]
pub struct LoadedMap {
    pub data: MapData,
    pub default_capacity: u16,
}

pub trait MapSource: Send + Sync {
    fn load(&self, name: &str) -> Result<LoadedMap, String>;
}

/// How many players a map holds unless the operator's configuration says
/// otherwise. The large maps hold about what the mode is for (Blood Gulch 500,
/// Sidewinder 300, about in proportion to their ground: Sidewinder's is two
/// thirds of Blood Gulch's); the others are small, and are not overfilled: 16
/// to 64 players is a crowd there. A name that is not one of the original maps
/// holds 16.
pub fn default_capacity(map: &str) -> u16 {
    match map {
        "bloodgulch" => 500,
        "sidewinder" => 300,
        "boardingaction" => 64,
        "putput" => 48,
        "hangemhigh" => 32,
        "beavercreek" | "carousel" | "damnation" | "ratrace" => 24,
        _ => 16,
    }
}

/// The `.map` files of a folder (the operator's own copy of the game's data).
pub struct MapFiles {
    pub dir: PathBuf,
}

impl MapSource for MapFiles {
    fn load(&self, name: &str) -> Result<LoadedMap, String> {
        let path = self.dir.join(format!("{name}.map"));
        let map = halo_map::HaloMap::from_path(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if map.player_starts.is_empty() {
            return Err(format!("{}: the map has no player starting locations", path.display()));
        }
        // (the starting locations are put on the ground, and clear of walls, as the map is built)
        Ok(LoadedMap { data: MapData::from(map), default_capacity: default_capacity(name) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_large_maps_hold_the_most_and_every_original_map_has_a_cap() {
        let maps = [
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
        for map in maps {
            let cap = default_capacity(map);
            assert!((16..=500).contains(&cap), "{map}: {cap}");
            if !matches!(map, "bloodgulch" | "sidewinder") {
                assert!(cap <= 64, "{map} is not a large map: {cap}");
            }
        }
        assert!(default_capacity("bloodgulch") > default_capacity("sidewinder"));
        assert_eq!(default_capacity("not_a_map"), 16);
    }
}
