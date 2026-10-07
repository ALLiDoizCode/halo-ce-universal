use alloc::vec::Vec;

use halo_map::collision::CollisionBsp;
use halo_map::combat::Combat;
use halo_map::items::Items;
use halo_map::vehicles::Vehicles;
use halo_map::{HaloMap, Movement};

use crate::math::sqrt;
use crate::spawn::Start;

/// What the simulation reads from a map: its collision BSP and bounds, and
/// the tags' movement and combat values. The server keeps one per match and
/// passes it by reference to every step.
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
    /// What the tags say of fighting: the weapons, and the player's health and
    /// shields (see [`crate::combat`]).
    pub combat: Combat,
    /// What the tags say of items: what can be picked up, where it appears and
    /// how often, and how far a player reaches (see [`crate::items`]).
    pub items: Items,
    /// What the tags say of vehicles: their handling, physics, seats, weapons
    /// and bodies, and which vehicle each placement of the map places.
    pub vehicles: Vehicles,
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
    /// the combat values ([`Combat::to_bytes`], behind a `u32` length), the
    /// item values ([`Items::to_bytes`], likewise), the vehicle values
    /// ([`Vehicles::to_bytes`], likewise), and
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
        let combat = self.combat.to_bytes();
        out.extend_from_slice(&(combat.len() as u32).to_le_bytes());
        out.extend_from_slice(&combat);
        let items = self.items.to_bytes();
        out.extend_from_slice(&(items.len() as u32).to_le_bytes());
        out.extend_from_slice(&items);
        let vehicles = self.vehicles.to_bytes();
        out.extend_from_slice(&(vehicles.len() as u32).to_le_bytes());
        out.extend_from_slice(&vehicles);
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
        let (start_bytes, rest) =
            count.checked_mul(Start::BYTES).and_then(|n| rest.split_at_checked(n)).ok_or_else(short)?;
        let short_combat = || halo_map::MapError::Malformed("map data is shorter than its combat values".into());
        let (combat_len, rest) = rest.split_at_checked(4).ok_or_else(short_combat)?;
        let combat_len = u32::from_le_bytes(combat_len.try_into().unwrap()) as usize;
        let (combat_bytes, rest) = rest.split_at_checked(combat_len).ok_or_else(short_combat)?;
        let combat = Combat::from_bytes(combat_bytes)?;
        let short_items = || halo_map::MapError::Malformed("map data is shorter than its item values".into());
        let (items_len, rest) = rest.split_at_checked(4).ok_or_else(short_items)?;
        let items_len = u32::from_le_bytes(items_len.try_into().unwrap()) as usize;
        let (items_bytes, rest) = rest.split_at_checked(items_len).ok_or_else(short_items)?;
        let items = Items::from_bytes(items_bytes)?;
        let short_vehicles = || halo_map::MapError::Malformed("map data is shorter than its vehicle values".into());
        let (vehicles_len, rest) = rest.split_at_checked(4).ok_or_else(short_vehicles)?;
        let vehicles_len = u32::from_le_bytes(vehicles_len.try_into().unwrap()) as usize;
        let (vehicles_bytes, collision) = rest.split_at_checked(vehicles_len).ok_or_else(short_vehicles)?;
        let vehicles = Vehicles::from_bytes(vehicles_bytes)?;
        vehicles.check_against(&combat)?;
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
        Ok(MapData {
            collision: CollisionBsp::from_bytes(collision)?,
            world_bounds,
            movement,
            starts,
            combat,
            items,
            vehicles,
        })
    }
}

impl From<HaloMap> for MapData {
    fn from(map: HaloMap) -> MapData {
        let mut data = MapData {
            collision: map.collision,
            world_bounds: map.world_bounds,
            movement: map.movement,
            starts: Vec::new(),
            combat: map.combat,
            items: map.items,
            vehicles: map.vehicle_tags,
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
    use alloc::vec;

    use super::*;

    #[test]
    fn a_map_survives_its_bytes() {
        let map = crate::fixtures::flat_floor_map();
        let back = MapData::from_bytes(&map.to_bytes()).unwrap();
        assert_eq!(back.world_bounds, map.world_bounds);
        assert_eq!(back.movement, map.movement);
        assert_eq!(back.collision, map.collision);
        assert_eq!(back.combat, map.combat);
    }

    #[test]
    fn a_map_with_fighting_survives_its_bytes() {
        let mut map = crate::fixtures::flat_floor_map();
        map.combat = crate::fixtures::combat_fixture();
        let back = MapData::from_bytes(&map.to_bytes()).unwrap();
        assert_eq!(back.combat, map.combat);
        assert!(!back.combat.weapons.is_empty());
    }

    fn vehicles_fixture(weapon: u16) -> Vehicles {
        use halo_map::vehicles::{Handling, MassPoint, Physics, Seat, VehicleDef};
        let point = MassPoint {
            name: "wheel".into(),
            powered_mass_point_index: -1,
            model_node_index: 2,
            flags: 1,
            relative_mass: 1.0,
            mass: 2.0,
            relative_density: 3.0,
            density: 4.0,
            position: [0.5, 1.0, 0.0],
            forward: [0.0, 1.0, 0.0],
            up: [0.0, 0.0, 1.0],
            friction_type: 1,
            friction_parallel_scale: 0.9,
            friction_perpendicular_scale: 0.8,
            radius: 0.4,
        };
        let physics = Physics {
            radius: 1.5,
            moment: 2.0,
            mass: 1200.0,
            center_of_mass: [0.0, 0.0, 0.5],
            density: 1.0,
            gravity_scale: 1.0,
            ground_friction: 0.25,
            ground_depth: 0.5,
            ground_damp_fraction: 0.75,
            ground_normal_k1: 0.125,
            ground_normal_k0: 0.0625,
            water_friction: 0.5,
            water_depth: 1.0,
            water_density: 1.5,
            air_friction: 0.125,
            xx_moment: 0.1,
            yy_moment: 0.2,
            zz_moment: 0.3,
            powered_mass_points: Vec::new(),
            mass_points: vec![point],
        };
        let seat = Seat {
            flags: 4,
            label: "warthog_d".into(),
            marker_name: "driver".into(),
            acceleration_scale: [0.0, 0.5, 0.25],
            yaw_rate: 1.5,
            pitch_rate: 1.25,
            yaw_minimum: -1.0,
            yaw_maximum: 1.0,
        };
        let def = VehicleDef {
            tag_index: 40,
            name: "vehicles\\warthog\\warthog.vehicle".into(),
            object_flags: 5,
            bounding_radius: 3.0,
            bounding_offset: [0.0, 0.0, 0.75],
            unit_flags: 0x40,
            child_damage_fraction: 0.5,
            handling: Handling {
                vehicle_type: 1,
                maximum_forward_speed: 28.0,
                maximum_reverse_speed: -9.0,
                ..Default::default()
            },
            physics: Some(physics),
            seats: vec![seat],
            weapons: vec![weapon],
            resistance: Some(halo_map::combat::Resistance::default()),
        };
        Vehicles { defs: vec![def], placements: vec![Some(40), None], ..Default::default() }
    }

    #[test]
    fn a_map_with_vehicles_survives_its_bytes() {
        let mut map = crate::fixtures::flat_floor_map();
        map.vehicles = vehicles_fixture(map.combat.weapons[0].tag_index);
        let back = MapData::from_bytes(&map.to_bytes()).unwrap();
        assert_eq!(back.vehicles, map.vehicles);
        assert_eq!(back.items, map.items);
        assert_eq!(back.collision, map.collision);
    }

    #[test]
    fn vehicle_values_that_are_cut_short_or_not_numbers_or_carry_a_weapon_the_map_lacks_are_refused() {
        let mut map = crate::fixtures::flat_floor_map();
        map.vehicles = vehicles_fixture(map.combat.weapons[0].tag_index);
        let bytes = map.to_bytes();
        let at = bytes.len() - map.collision.to_bytes().len() - map.vehicles.to_bytes().len();
        assert!(MapData::from_bytes(&bytes[..at + 8]).is_err());
        let mut broken = map.clone();
        broken.vehicles.defs[0].handling.speed_acceleration = f32::NAN;
        assert!(MapData::from_bytes(&broken.to_bytes()).is_err());
        let mut broken = map;
        broken.vehicles.defs[0].weapons = vec![broken.combat.weapons[0].tag_index.wrapping_add(1000)];
        assert!(MapData::from_bytes(&broken.to_bytes()).is_err());
    }

    #[test]
    fn combat_values_that_are_cut_short_or_not_numbers_are_refused() {
        let mut map = crate::fixtures::flat_floor_map();
        map.combat = crate::fixtures::combat_fixture();
        let bytes = map.to_bytes();
        // (cut anywhere in the combat values and what follows is no longer what they say)
        let combat_at = 24 + 4 * Movement::COUNT + 4;
        assert!(MapData::from_bytes(&bytes[..combat_at + 6]).is_err());
        let mut broken = map.clone();
        broken.combat.resistance.maximum_body_vitality = f32::NAN;
        assert!(MapData::from_bytes(&broken.to_bytes()).is_err());
        let mut broken = map;
        broken.combat.weapons[0].triggers[0].magazine_index = 7;
        assert!(MapData::from_bytes(&broken.to_bytes()).is_err());
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
