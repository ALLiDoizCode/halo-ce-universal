//! Where a match's map comes from: the operator's own `.map` files.

use std::path::PathBuf;

use halo_sim::{MapData, PlayerInput};

/// A map ready for a match: what the module keeps, and where players appear.
#[derive(Debug, Clone)]
pub struct LoadedMap {
    pub data: MapData,
    /// One per spawn point (the `player` field is the point's index): at
    /// the map's player starting locations, standing on the ground.
    pub spawns: Vec<PlayerInput>,
}

pub trait MapSource: Send + Sync {
    fn load(&self, name: &str) -> Result<LoadedMap, String>;
}

/// The `.map` files of a folder (the operator's own copy of the game's data).
pub struct MapFiles {
    pub dir: PathBuf,
}

impl MapSource for MapFiles {
    fn load(&self, name: &str) -> Result<LoadedMap, String> {
        let path = self.dir.join(format!("{name}.map"));
        let map = halo_map::HaloMap::from_path(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let starts: Vec<[f32; 3]> = map.player_starts.iter().map(|s| s.position).collect();
        let yaws: Vec<f32> = map.player_starts.iter().map(|s| s.facing).collect();
        let data = MapData::from(map);
        if starts.is_empty() {
            return Err(format!("{}: the map has no player starting locations", path.display()));
        }
        let spawns = starts
            .iter()
            .zip(&yaws)
            .enumerate()
            .map(|(index, (start, yaw))| {
                // feet on the ground below the starting location (which the map puts a little above it)
                let z = data
                    .collision
                    .ray_down([start[0], start[1], start[2] + 1.0], 3.0)
                    .map_or(start[2], |hit| hit.z + 0.01);
                PlayerInput { player: index as u16, position: [start[0], start[1], z], yaw: *yaw, pitch: 0.0 }
            })
            .collect();
        Ok(LoadedMap { data, spawns })
    }
}
