use alloc::vec::Vec;

use halo_map::collision::CollisionBsp;
use halo_map::{HaloMap, Movement};

use crate::math::sqrt;

/// What the simulation reads from a map: its collision BSP and bounds, and
/// the tags' movement values. The server keeps one per match and passes it by
/// reference to every step.
#[derive(Debug, Clone)]
pub struct MapData {
    pub collision: CollisionBsp,
    /// `x0, x1, y0, y1, z0, z1` in world units.
    pub world_bounds: [f32; 6],
    /// How a player moves on foot, as the map's tags say.
    pub movement: Movement,
}

impl MapData {
    /// The fastest an on-foot player can go over the ground, in world units a
    /// second, from the tags: the run speeds on both axes at once (the
    /// throttle's two axes are not limited to a circle), at the faster of the
    /// slopes' speed scales, with a tenth over for the rounding of a client's
    /// float arithmetic and network timing. What the server checks a move
    /// against (see [`crate::step`]).
    pub fn max_move_speed(&self) -> f32 {
        let m = &self.movement;
        let forward = m.run_forward_speed.max(m.run_backward_speed);
        let sideways = m.run_sideways_speed;
        let slope = m.downhill_velocity_scale.max(m.uphill_velocity_scale).max(1.0);
        sqrt(forward * forward + sideways * sideways) * slope * 1.1
    }

    /// The map as bytes, for a server to keep in one row: the six world
    /// bounds and the [`Movement::COUNT`] movement values (all little-endian
    /// `f32`) followed by
    /// [`CollisionBsp::to_bytes`](halo_map::collision::CollisionBsp::to_bytes).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for v in self.world_bounds.iter().chain(self.movement.to_array().iter()) {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&self.collision.to_bytes());
        out
    }

    /// Rebuild a map from [`MapData::to_bytes`], checking every index.
    pub fn from_bytes(bytes: &[u8]) -> halo_map::Result<MapData> {
        const BOUNDS: usize = 6 * 4;
        const MOVEMENT: usize = Movement::COUNT * 4;
        let Some((bounds, rest)) = bytes.split_at_checked(BOUNDS) else {
            return Err(halo_map::MapError::Malformed("map data is shorter than its bounds".into()));
        };
        let Some((movement_bytes, collision)) = rest.split_at_checked(MOVEMENT) else {
            return Err(halo_map::MapError::Malformed("map data is shorter than its movement values".into()));
        };
        let mut world_bounds = [0.0; 6];
        for (v, b) in world_bounds.iter_mut().zip(bounds.as_chunks::<4>().0) {
            *v = f32::from_le_bytes(*b);
        }
        let mut numbers = [0.0; Movement::COUNT];
        for (v, b) in numbers.iter_mut().zip(movement_bytes.as_chunks::<4>().0) {
            *v = f32::from_le_bytes(*b);
        }
        let movement = Movement::from_array(numbers);
        if !movement.is_sane() {
            return Err(halo_map::MapError::Malformed("the map's movement values are not usable".into()));
        }
        Ok(MapData { collision: CollisionBsp::from_bytes(collision)?, world_bounds, movement })
    }
}

impl From<HaloMap> for MapData {
    fn from(map: HaloMap) -> MapData {
        MapData { collision: map.collision, world_bounds: map.world_bounds, movement: map.movement }
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
        assert_eq!(back.movement, map.movement);
        assert_eq!(back.collision, map.collision);
    }

    #[test]
    fn map_bytes_too_short_for_the_bounds_are_refused() {
        assert!(MapData::from_bytes(&[0; 23]).is_err());
        assert!(MapData::from_bytes(&[0; 24 + 4 * Movement::COUNT - 1]).is_err());
    }

    #[test]
    fn movement_values_that_cannot_move_a_player_are_refused() {
        let mut map = crate::fixtures::flat_floor_map();
        map.movement.collision_radius = 0.0;
        assert!(MapData::from_bytes(&map.to_bytes()).is_err());
    }

    #[test]
    fn the_speed_bound_comes_from_the_tags_and_is_above_any_walking_speed() {
        let map = crate::fixtures::flat_floor_map();
        let m = &map.movement;
        let diagonal = sqrt(m.run_forward_speed * m.run_forward_speed + m.run_sideways_speed * m.run_sideways_speed);
        assert!(map.max_move_speed() > diagonal);
        assert!(map.max_move_speed() < 2.0 * diagonal);
    }
}
