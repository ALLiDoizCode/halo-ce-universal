use alloc::vec::Vec;

use halo_map::collision::CollisionBsp;
use halo_map::{HaloMap, Movement};

use crate::math::sqrt;
use crate::spawn::Start;

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
    /// The player starting locations, settled on the ground (see
    /// [`crate::spawn`]). Where [`crate::rules`] spawns players.
    pub starts: Vec<Start>,
}

impl MapData {
    /// The fastest an on-foot player can go over the ground, in world units a
    /// second, from the tags: the run speeds on both axes at once (the
    /// throttle's two axes are not limited to a circle), at the faster of the
    /// slopes' speed scales, with a quarter over: room for a client that
    /// delivers two ticks of its walking in the one tick the server counts (a
    /// frame that ran two game ticks, which the gateway's keep-the-newest
    /// makes one move), as a player running straight ahead does at twice the
    /// run speed. What the server checks a move against (see [`crate::step`]).
    pub fn max_move_speed(&self) -> f32 {
        let m = &self.movement;
        let forward = m.run_forward_speed.max(m.run_backward_speed);
        let sideways = m.run_sideways_speed;
        let slope = m.downhill_velocity_scale.max(m.uphill_velocity_scale).max(1.0);
        sqrt(forward * forward + sideways * sideways) * slope * 1.25
    }

    /// The map as bytes, for a server to keep in one row: the six world
    /// bounds and the [`Movement::COUNT`] movement values (all little-endian
    /// `f32`), the starting locations (a `u32` count, then for each its
    /// position and yaw as `f32`s, its team and four game types as `i16`s),
    /// and
    /// [`CollisionBsp::to_bytes`](halo_map::collision::CollisionBsp::to_bytes).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for v in self.world_bounds.iter().chain(self.movement.to_array().iter()) {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&(self.starts.len() as u32).to_le_bytes());
        for start in &self.starts {
            for v in start.position.iter().chain([&start.yaw]) {
                out.extend_from_slice(&v.to_le_bytes());
            }
            for v in core::iter::once(&start.team).chain(&start.game_types) {
                out.extend_from_slice(&v.to_le_bytes());
            }
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
        let Some((movement_bytes, rest)) = rest.split_at_checked(MOVEMENT) else {
            return Err(halo_map::MapError::Malformed("map data is shorter than its movement values".into()));
        };
        let short = || halo_map::MapError::Malformed("map data is shorter than its starting locations".into());
        let (count, rest) = rest.split_at_checked(4).ok_or_else(short)?;
        let count = u32::from_le_bytes(count.try_into().unwrap()) as usize;
        let (start_bytes, collision) =
            count.checked_mul(Start::BYTES).and_then(|n| rest.split_at_checked(n)).ok_or_else(short)?;
        let starts = start_bytes
            .as_chunks::<{ Start::BYTES }>()
            .0
            .iter()
            .map(|b| {
                let f = |i: usize| f32::from_le_bytes(b[4 * i..4 * i + 4].try_into().unwrap());
                let h = |i: usize| i16::from_le_bytes(b[16 + 2 * i..18 + 2 * i].try_into().unwrap());
                Start { position: [f(0), f(1), f(2)], yaw: f(3), team: h(0), game_types: [h(1), h(2), h(3), h(4)] }
            })
            .collect();
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
        Ok(MapData { collision: CollisionBsp::from_bytes(collision)?, world_bounds, movement, starts })
    }
}

impl From<HaloMap> for MapData {
    fn from(map: HaloMap) -> MapData {
        let mut data = MapData {
            collision: map.collision,
            world_bounds: map.world_bounds,
            movement: map.movement,
            starts: Vec::new(),
        };
        let starts: Vec<Start> = map
            .player_starts
            .iter()
            .map(|s| Start { position: s.position, yaw: s.facing, team: s.team_index, game_types: s.game_types })
            .collect();
        data.starts = starts.into_iter().map(|s| crate::spawn::settle(&data, s)).collect();
        data
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
