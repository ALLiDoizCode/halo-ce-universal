use alloc::vec::Vec;

use halo_map::collision::CollisionBsp;
use halo_map::HaloMap;

/// What the simulation reads from a map: its collision BSP and bounds. The
/// server keeps one per match and passes it by reference to every step.
#[derive(Debug, Clone)]
pub struct MapData {
    pub collision: CollisionBsp,
    /// `x0, x1, y0, y1, z0, z1` in world units.
    pub world_bounds: [f32; 6],
}

impl MapData {
    /// The map as bytes, for a server to keep in one row: the six world
    /// bounds (little-endian `f32`) followed by
    /// [`CollisionBsp::to_bytes`](halo_map::collision::CollisionBsp::to_bytes).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for v in self.world_bounds {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&self.collision.to_bytes());
        out
    }

    /// Rebuild a map from [`MapData::to_bytes`], checking every index.
    pub fn from_bytes(bytes: &[u8]) -> halo_map::Result<MapData> {
        const BOUNDS: usize = 6 * 4;
        let Some((bounds, collision)) = bytes.split_at_checked(BOUNDS) else {
            return Err(halo_map::MapError::Malformed("map data is shorter than its bounds".into()));
        };
        let mut world_bounds = [0.0; 6];
        for (v, b) in world_bounds.iter_mut().zip(bounds.as_chunks::<4>().0) {
            *v = f32::from_le_bytes(*b);
        }
        Ok(MapData { collision: CollisionBsp::from_bytes(collision)?, world_bounds })
    }
}

impl From<HaloMap> for MapData {
    fn from(map: HaloMap) -> MapData {
        MapData { collision: map.collision, world_bounds: map.world_bounds }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_map_survives_its_bytes() {
        let map = crate::fixtures::flat_floor_map();
        let back = MapData::from_bytes(&map.to_bytes()).unwrap();
        assert_eq!(back.world_bounds, map.world_bounds);
        assert_eq!(back.collision, map.collision);
    }

    #[test]
    fn map_bytes_too_short_for_the_bounds_are_refused() {
        assert!(MapData::from_bytes(&[0; 23]).is_err());
    }
}
