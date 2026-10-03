//! Where a player appears: the engine's choice of a player starting location
//! (`find_best_starting_location_index` and the rating functions of
//! `source/game/game_engine.c`), and what is done when none is free: a spot beside one, and a wave as the fallback.
//!
//! # The engine's rule
//!
//! Every starting location the game type allows is given a rating, scaled by
//! a random number's square root, and the best one wins; a rating of 0 is a
//! location that is not free, and when every one is 0 the player does not
//! spawn (the engine tries again each tick). The rating of a location at
//! `p` for a player on team `t`, as the engine has it:
//!
//! - for every player in the world, at distance `d` from `p`: under a quarter
//!   of a world unit makes it 0 and under one a tenth of what it was; for a
//!   player on another team (everyone is another team's in a game without
//!   teams) under 2 makes it 0 and between 2 and 5 scales it by `(d - 2) / 3`;
//! - in a team game, teammates 1 to 6 away add to a bonus of up to four times
//!   (each adds `(1 - (d - 1) / 5) ^ 0.6`, the sum is at most 3, the rating is
//!   multiplied by `3 * sum + 1`).
//!
//! A starting location has a team (the author's), and the engine's Slayer does
//! not look at it: that test is a flag only capture the flag sets, and on the
//! maps as they ship every location that lists Slayer is team 0's, so a team
//! that could use only its own would have nowhere to spawn. A team matters to
//! the rating, through the enemies' room and the friends' bonus.
//!
//! # What differs
//!
//! - A player is never put within a pill's width of another: under twice the
//!   tags' collision radius (0.4 with a real map's) is 0 where the engine's
//!   quarter of a world unit is smaller than a player is wide.
//! - `x ^ 0.6` is `x ^ (5/8)`, built from square roots, as the simulation has
//!   no `pow` that is the same on every target.
//! - There is no check for a vehicle on the location (there are no vehicles
//!   yet).
//! - A location that is a few centimetres inside a wall (a few percent of
//!   them are, on the maps as they ship) is moved clear of it, and put on the
//!   ground, when the map is loaded ([`settle`]).
//! - When a player's spawn asks for it (see [`crate::rules`]: a player whose timer has run
//!   out asks at once, and a wave asks again for those it left waiting) a location that is
//!   taken is not the end:
//!   the places around it are tried too, 0.6 and 1.2 world units out, and are
//!   free if nobody is within a pill's width of them (the engine's room for
//!   enemies is not asked for), so that a crowd can be put on a map with 16
//!   locations.

use alloc::vec::Vec;

use halo_map::collision::{TEST_BACK_FACING, TEST_FRONT_FACING};
use halo_map::game_type;

use crate::map::MapData;
use crate::math::sqrt;
use crate::rng::Rng;
use crate::walk;

/// A player starting location, as the map has it (settled: see [`settle`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Start {
    /// Where a player's feet go.
    pub position: [f32; 3],
    /// Radians.
    pub yaw: f32,
    /// The team the map's author made it for (0 or 1), anything else for none.
    /// Only capture the flag looks at it: on the maps as they ship every start
    /// that lists Slayer is team 0's, and the engine's Slayer, with or without
    /// teams, uses all of them.
    pub team: i16,
    /// `halo_map::game_type` values; the location is used for each listed.
    pub game_types: [i16; 4],
}

impl Start {
    /// Bytes in [`MapData::to_bytes`].
    pub const BYTES: usize = 4 * 4 + 5 * 2;

    /// The engine's `match_game_type` for Slayer (and Team Slayer, which is
    /// Slayer with teams): the location lists Slayer or one of the groups
    /// that contain it.
    pub fn is_for_slayer(&self) -> bool {
        self.game_types
            .iter()
            .any(|t| matches!(*t, game_type::SLAYER | game_type::ALL | game_type::ALL_NON_CTF | game_type::ALL_NORMAL))
    }
}

/// A player in the world, as a rating sees them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Occupant {
    pub position: [f32; 3],
    pub team: u8,
}

fn distance(a: &[f32; 3], b: &[f32; 3]) -> f32 {
    let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    sqrt(d[0] * d[0] + d[1] * d[1] + d[2] * d[2])
}

/// `x ^ 0.6` for `x` in `[0, 1]`, as `x ^ (5/8)`.
fn pow_point_six(x: f32) -> f32 {
    let root = sqrt(x);
    root * sqrt(sqrt(root))
}

/// The rating of a free spot at `position` for a player on `team`: 0 for one
/// that is not free. Without the random scaling (see [`pick`]).
pub fn rate(map: &MapData, teams: bool, team: u8, position: &[f32; 3], others: &[Occupant]) -> f32 {
    let width = 2.0 * map.movement.collision_radius;
    let mut rating = 1.0f32;
    let mut friends = 0.0f32;
    for other in others {
        let d = distance(&other.position, position);
        let enemy = !teams || other.team != team;
        if enemy || d <= 0.25 {
            if d < width.max(0.25) {
                rating = 0.0;
            } else if d < 1.0 {
                rating *= 0.1;
            }
            if enemy {
                if d < 2.0 {
                    rating = 0.0;
                } else if d <= 5.0 {
                    rating *= (d - 2.0) * 0.333_333_34;
                }
            }
        }
        if teams && !enemy && (1.0..=6.0).contains(&d) {
            friends += pow_point_six(1.0 - (d - 1.0) * 0.2);
        }
    }
    if teams && rating > 0.0 {
        rating *= friends.min(3.0) * 3.0 + 1.0;
    }
    rating
}

/// How far from a location, and in which directions, the players of a wave are
/// tried when it is taken.
const RING_RADII: [f32; 2] = [0.6, 1.2];
/// Unit vectors of sixteen directions, a sixteenth of a turn apart.
const DIRECTIONS: [[f32; 2]; 16] = [
    [1.0, 0.0],
    [0.923_879_5, 0.382_683_43],
    [0.707_106_77, 0.707_106_77],
    [0.382_683_43, 0.923_879_5],
    [0.0, 1.0],
    [-0.382_683_43, 0.923_879_5],
    [-0.707_106_77, 0.707_106_77],
    [-0.923_879_5, 0.382_683_43],
    [-1.0, 0.0],
    [-0.923_879_5, -0.382_683_43],
    [-0.707_106_77, -0.707_106_77],
    [-0.382_683_43, -0.923_879_5],
    [0.0, -1.0],
    [0.382_683_43, -0.923_879_5],
    [0.707_106_77, -0.707_106_77],
    [0.923_879_5, -0.382_683_43],
];

/// The ground under `position`: the feet's height a hair above it.
fn grounded(map: &MapData, position: [f32; 3]) -> Option<[f32; 3]> {
    map.collision
        .ray_down([position[0], position[1], position[2] + 1.0], 3.0)
        .map(|hit| [position[0], position[1], hit.z + 0.01])
}

/// Whether a player's feet at `position` are on the ground with nothing of
/// the map in the way of the pill.
fn standable(map: &MapData, position: [f32; 3]) -> bool {
    let footing = walk::footing(map, position);
    footing.penetration <= crate::PENETRATION_TOLERANCE && footing.supported
}

/// A start put on the ground and, if it is inside a wall, moved out of it:
/// to the nearest spot within 0.6 world units where a player can stand.
/// Nothing within reach is a start left where it is (on the ground).
pub fn settle(map: &MapData, start: Start) -> Start {
    let at = grounded(map, start.position).unwrap_or(start.position);
    let mut settled = Start { position: at, ..start };
    if standable(map, at) {
        return settled;
    }
    for step in 1..=12 {
        let radius = step as f32 * 0.05;
        for direction in DIRECTIONS {
            let to = [at[0] + direction[0] * radius, at[1] + direction[1] * radius, at[2]];
            if let Some(ground) = grounded(map, to) {
                if (ground[2] - at[2]).abs() <= 0.3 && standable(map, ground) {
                    settled.position = ground;
                    return settled;
                }
            }
        }
    }
    settled.position = at;
    settled
}

/// Where a player goes, and which way they face.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spot {
    pub position: [f32; 3],
    pub yaw: f32,
    /// A spot beside a location that was taken, found when none was free.
    pub beside: bool,
}

/// The engine's choice for a player on `team`: the best rated of the map's
/// starts that the game allows, each rating scaled by the square root of a
/// random number. `None` when none is free. With `overflow`, if none is, a
/// spot beside one that is taken (see the module's list of what differs).
pub fn pick(map: &MapData, teams: bool, team: u8, others: &[Occupant], rng: &mut Rng, overflow: bool) -> Option<Spot> {
    let mut best: Option<(f32, &Start)> = None;
    let mut allowed = 0u32;
    for start in map.starts.iter().filter(|s| s.is_for_slayer()) {
        allowed += 1;
        let rating = rate(map, teams, team, &start.position, others) * sqrt(rng.next_f32());
        if rating > best.map_or(0.0, |(r, _)| r) {
            best = Some((rating, start));
        }
    }
    if let Some((_, start)) = best {
        return Some(Spot { position: start.position, yaw: start.yaw, beside: false });
    }
    if !overflow || allowed == 0 {
        return None;
    }
    // every one is taken: beside them, from a random one on
    let first = rng.next_u32() % allowed;
    let starts: Vec<&Start> = map.starts.iter().filter(|s| s.is_for_slayer()).collect();
    for radius in RING_RADII {
        for i in 0..starts.len() {
            let start = starts[(first as usize + i) % starts.len()];
            for direction in DIRECTIONS {
                let to = [
                    start.position[0] + direction[0] * radius,
                    start.position[1] + direction[1] * radius,
                    start.position[2],
                ];
                // (beside a start the engine's room for enemies is not asked for: only that
                // nobody is where a player would stand)
                let width = 2.0 * map.movement.collision_radius;
                if others.iter().any(|o| distance(&o.position, &to) < width) {
                    continue;
                }
                let Some(ground) = grounded(map, to) else { continue };
                if (ground[2] - start.position[2]).abs() > 0.3 || !standable(map, ground) {
                    continue;
                }
                // not through a wall from the location
                let from = [start.position[0], start.position[1], start.position[2] + map.movement.collision_radius];
                let delta =
                    [ground[0] - from[0], ground[1] - from[1], ground[2] + map.movement.collision_radius - from[2]];
                if map.collision.test_vector(TEST_FRONT_FACING | TEST_BACK_FACING, from, delta, 1.0).is_some() {
                    continue;
                }
                return Some(Spot { position: ground, yaw: start.yaw, beside: true });
            }
        }
    }
    None
}
