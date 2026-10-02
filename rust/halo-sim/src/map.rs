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

impl From<HaloMap> for MapData {
    fn from(map: HaloMap) -> MapData {
        MapData { collision: map.collision, world_bounds: map.world_bounds }
    }
}
