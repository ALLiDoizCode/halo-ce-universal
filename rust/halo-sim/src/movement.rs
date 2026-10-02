use halo_map::collision::{TEST_BACK_FACING, TEST_FRONT_FACING};

use crate::map::MapData;
use crate::math::sqrt;
use crate::pill::footing;
use crate::state::{Player, FLAG_CROUCHED};
use crate::step::{PlayerInput, RejectReason};
use crate::walk::GRAVITY;

pub const TICKS_PER_SECOND: u32 = 30;

/// The most ticks of speed bound one move may cover after input was lost or
/// skipped: a second. A player who reports rarely is not allowed to go faster
/// on average (the bound is [`MapData::max_move_speed`] times the time since the last
/// accepted move, so the sum of moves over any period stays within it), only
/// to jump up to this far at once.
pub const MAX_CATCH_UP_TICKS: u32 = TICKS_PER_SECOND;

/// How far below a reported position ground may be for the player to count
/// as standing on it: a little more than a hop off a steep slope or a low
/// step takes (half a second of falling is 0.4). Anything higher is a jump or
/// a fall, which the airborne rule (below) judges.
pub const GROUND_TOLERANCE: f32 = 0.5;

/// How close to a surface a player in the air is to have landed on it: the
/// engine leaves a player who lands on the surface, give or take rounding.
const CONTACT_TOLERANCE: f32 = 0.02;

/// How much room the airborne rule leaves a reported height beyond what a
/// jump or a fall at most takes: a crouch in the air lifts the player's feet
/// by up to the difference of the two stances (added from the tags), and
/// this much besides for a position's rounding.
const AIR_SLACK: f32 = 0.02;

/// How far a reported position may be inside what the player's pill has to
/// stay out of. The engine's own movement leaves a pill exactly on the
/// boundary, give or take a rounding error; a position deeper in than this
/// is not one it could have made.
pub const PENETRATION_TOLERANCE: f32 = 0.01;

/// How much deeper than the last accepted position a position that is
/// already inside something may be: next to nothing, so that a player cannot
/// work their way through a wall a step at a time.
const DEEPER_TOLERANCE: f32 = 0.001;

fn finite(values: &[f32]) -> bool {
    // NaN fails the comparison, and so does infinity
    values.iter().all(|c| c.abs() <= f32::MAX)
}

/// Where a player is in the air, as far as the moves the server accepted say.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Air {
    /// Ticks since the player left the ground: 0 on the ground.
    pub ticks: u32,
    /// The height they left it at.
    pub z: f32,
    /// Ticks since the player was last by a surface (within the ground
    /// tolerance of one that is not an overhang): 0 if they are by one now.
    pub free_ticks: u32,
    /// The height they were at then (unused when `free_ticks` is 0: it is the
    /// position they are at).
    pub free_z: f32,
}

impl Air {
    pub(crate) const GROUNDED: Air = Air { ticks: 0, z: 0.0, free_ticks: 0, free_z: 0.0 };
}

/// The most the rise of a player `ticks` ticks into free air can be above the
/// height they were last by a surface at: a jump sets the upward speed (a
/// player who leaves the ground walking up a slope has up to the vertical part
/// of their walking speed, which is no more), and gravity does the rest.
/// From the tags and the engine's gravity; the same for every player.
fn highest_rise(map: &MapData, ticks: u32) -> f32 {
    let t = ticks as f32;
    up_speed(map) * t - GRAVITY * t * (t + 1.0) * 0.5 + air_slack(map)
}

/// The least the rise of a player `ticks` ticks after leaving the ground can
/// be: a walk off an edge down a slope sets the downward speed (at most the
/// fastest walking speed), gravity does the rest, and nothing in the air makes
/// a player fall faster than that: a slide down a slope or a brush with a wall
/// only slows a fall.
fn lowest_rise(map: &MapData, ticks: u32) -> f32 {
    let t = ticks as f32;
    // (a player who slid down a steep slope before the edge leaves it with more speed down than they
    // walked: no more than their walking speed in all, as it happens, which is all that is allowed)
    -(map.max_move_speed() / TICKS_PER_SECOND as f32 * t) - GRAVITY * t * (t + 1.0) * 0.5 - air_slack(map)
}

/// The most a player rises above where they left the ground, while they are
/// by a surface or in the air: the top of the highest jump (and the slack).
fn air_apex(map: &MapData) -> f32 {
    let up = up_speed(map);
    up * up / (2.0 * GRAVITY) + air_slack(map)
}

/// The vertical part of the fastest walking speed, per tick, up the steepest
/// slope a player stands on: the steepest slope is one whose normal's
/// vertical part is `minimum_normal_k`.
fn walking_vertical(map: &MapData) -> f32 {
    let steepest = sqrt((1.0 - map.movement.minimum_normal_k * map.movement.minimum_normal_k).max(0.0));
    map.max_move_speed() / TICKS_PER_SECOND as f32 * steepest
}

fn up_speed(map: &MapData) -> f32 {
    map.movement.jump_velocity.max(walking_vertical(map))
}

fn air_slack(map: &MapData) -> f32 {
    let m = &map.movement;
    AIR_SLACK + (m.collision_height_standing - m.collision_height_crouching).max(0.0)
}

/// Whether a player may make the reported move, or why not; and where they
/// are in the air after it.
///
/// `ticks` is the time since the player's last accepted move, in ticks; the
/// speed bound is that many ticks' worth, at most [`MAX_CATCH_UP_TICKS`].
///
/// # The airborne rule
///
/// A report that is not on the ground is a jump or a fall. The server keeps
/// how long the player has been off the ground and the height they left it
/// at, and how long they have been clear of every surface and the height they
/// were at when they last were near one ([`Air`]). A position in free air is
/// accepted only if its height is where a jump or a fall could have it by
/// now: no higher than a jump from where they were last by a surface (so
/// a player cannot hang in the air, or climb in it) and no lower than a fall
/// from where they left the ground (so they cannot drop faster than gravity;
/// a slide down a slope, which is slower than a fall, is always within it).
/// By a surface that is not ground to stand on (a wall, a slope too steep to
/// stand on) a player is held to the speed bound going up, to the top of a
/// jump above where they left the ground, and to the same fall going down. A
/// report on the ground ends it, if it is no higher than a jump reaches.
pub(crate) fn validate(map: &MapData, player: &Player, input: &PlayerInput, ticks: u32) -> Result<Air, RejectReason> {
    let from = player.position;
    let before = Air { ticks: player.air_ticks, z: player.air_z, free_ticks: player.free_ticks, free_z: player.free_z };
    let to = input.position;
    if !finite(&to) || !finite(&[input.yaw, input.pitch]) {
        return Err(RejectReason::NotFinite);
    }
    let delta = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    let flat = delta[0] * delta[0] + delta[1] * delta[1];
    let squared = flat + delta[2] * delta[2];
    // both ends are finite, so the square is a number or, on overflow, infinity
    let ticks = ticks.clamp(1, MAX_CATCH_UP_TICKS);
    let allowed = map.max_move_speed() / TICKS_PER_SECOND as f32 * ticks as f32;
    // the way across is a run's at most, in the air too; how far a player goes
    // up and down is the speed bound's on the ground and the airborne rule's off it
    if flat > allowed * allowed {
        return Err(RejectReason::TooFast);
    }
    if delta == [0.0; 3] && before.ticks == 0 {
        // nothing moved on the ground: nothing new to find wrong with where the player is
        return Ok(Air::GROUNDED);
    }

    // the player's pill, as the movement of a walking player has it: the
    // reported position is its origin, near the feet, and its base sphere's
    // centre a radius above (a crouched player's is shorter)
    let m = &map.movement;
    let radius = m.collision_radius;
    let stance =
        if input.flags & FLAG_CROUCHED != 0 { m.collision_height_crouching } else { m.collision_height_standing };
    let height = stance - 2.0 * radius;
    let base_from = [from[0], from[1], from[2] + radius];
    let base_to = [to[0], to[1], to[2] + radius];

    // the centre of a pill never crosses a surface of the map (it stays a
    // radius from them), so a straight path of it that does is a move through
    // one; and where it ends it is not inside the margin the engine keeps
    if delta != [0.0; 3]
        && map.collision.test_vector(TEST_FRONT_FACING | TEST_BACK_FACING, base_from, delta, 1.0).is_some()
    {
        return Err(RejectReason::ThroughSurface);
    }
    let footing = footing(&map.collision, base_to, height, radius, GROUND_TOLERANCE, m.minimum_normal_k);
    if footing.penetration > PENETRATION_TOLERANCE {
        // a player can be put a little inside something (a start next to an
        // overhang) and the engine lets them stay: what no move does is go deeper
        let before = crate::pill::footing(
            &map.collision,
            base_from,
            m.collision_height_standing - 2.0 * radius,
            radius,
            0.0,
            m.minimum_normal_k,
        );
        if footing.penetration > before.penetration + DEEPER_TOLERANCE {
            return Err(RejectReason::ThroughSurface);
        }
    }

    // in the air for this many ticks by this move, from the height they left at
    let (left_at, in_air) =
        if before.ticks == 0 { (from[2], ticks) } else { (before.z, before.ticks.saturating_add(ticks)) };
    let rise = to[2] - left_at;
    let fall_too_fast = rise < lowest_rise(map, in_air);

    if footing.supported {
        // (a step over a surface: the way across the speed bound's, the way up that and a
        // jump's, which a player who jumps while running is making, and two of which
        // the gateway may have been handed in one tick)
        let reach = allowed + map.movement.jump_velocity * ticks as f32;
        let steady = squared <= reach * reach;
        if footing.standing {
            if before.ticks == 0 {
                // walking, or hopping over what is under the player, over the ground: the
                // speed bound's; a move that is longer and goes down is a fall that
                // passes ground, which the rest judges
                if steady {
                    return Ok(Air::GROUNDED);
                }
                if delta[2] >= 0.0 {
                    return Err(RejectReason::TooFast);
                }
            } else if crate::pill::footing(
                &map.collision,
                base_to,
                height,
                radius,
                CONTACT_TOLERANCE,
                m.minimum_normal_k,
            )
            .standing
            {
                // down on the ground, touching it: from no higher than a jump reaches
                if rise > air_apex(map) {
                    return Err(RejectReason::OffGround);
                }
                return Ok(Air::GROUNDED);
            }
        }
        // by a surface that is not ground, or falling past ground: the way up is
        // the speed bound's and a jump's top, the way down a fall's
        if delta[2] >= 0.0 && !steady {
            return Err(RejectReason::TooFast);
        }
        if rise > air_apex(map) || fall_too_fast {
            return Err(RejectReason::OffGround);
        }
        return Ok(Air { ticks: in_air, z: left_at, free_ticks: 0, free_z: to[2] });
    }

    // free air: up no further than a jump from the last surface, down no faster than a fall
    let (since, from_z) = match (before.ticks, before.free_ticks) {
        (0, _) | (_, 0) => (ticks, from[2]),
        (_, free) => (free.saturating_add(ticks), before.free_z),
    };
    if to[2] - from_z > highest_rise(map, since) || fall_too_fast {
        return Err(RejectReason::OffGround);
    }
    Ok(Air { ticks: in_air, z: left_at, free_ticks: since, free_z: from_z })
}
