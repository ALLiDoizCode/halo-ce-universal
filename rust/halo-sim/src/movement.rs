use halo_map::collision::{TEST_BACK_FACING, TEST_FRONT_FACING};

use crate::map::MapData;
use crate::pill::footing;
use crate::step::{PlayerInput, RejectReason};

pub const TICKS_PER_SECOND: u32 = 30;

/// The most ticks of speed bound one move may cover after input was lost or
/// skipped: a second. A player who reports rarely is not allowed to go faster
/// on average (the bound is [`MapData::max_move_speed`] times the time since the last
/// accepted move, so the sum of moves over any period stays within it), only
/// to jump up to this far at once.
pub const MAX_CATCH_UP_TICKS: u32 = TICKS_PER_SECOND;

/// How far below a reported position ground may be for the player to count
/// as standing on it.
pub const GROUND_TOLERANCE: f32 = 0.05;

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

/// Whether a player at `from` may make the reported move, or why not.
///
/// `ticks` is the time since the player's last accepted move, in ticks; the
/// speed bound is that many ticks' worth, at most [`MAX_CATCH_UP_TICKS`].
pub(crate) fn validate(map: &MapData, from: [f32; 3], input: &PlayerInput, ticks: u32) -> Result<(), RejectReason> {
    let to = input.position;
    if !finite(&to) || !finite(&[input.yaw, input.pitch]) {
        return Err(RejectReason::NotFinite);
    }
    let delta = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    let squared = delta[0] * delta[0] + delta[1] * delta[1] + delta[2] * delta[2];
    // both ends are finite, so the square is a number or, on overflow, infinity
    let allowed = map.max_move_speed() / TICKS_PER_SECOND as f32 * ticks.clamp(1, MAX_CATCH_UP_TICKS) as f32;
    if squared > allowed * allowed {
        return Err(RejectReason::TooFast);
    }
    if delta == [0.0; 3] {
        // nothing moved: nothing new to find wrong with where the player is
        return Ok(());
    }

    // the player's pill, as the movement of a walking player has it: the
    // reported position is its origin, near the feet, and its base sphere's
    // centre a radius above
    let m = &map.movement;
    let radius = m.collision_radius;
    let height = m.collision_height_standing - 2.0 * radius;
    let base_from = [from[0], from[1], from[2] + radius];
    let base_to = [to[0], to[1], to[2] + radius];

    // the centre of a pill never crosses a surface of the map (it stays a
    // radius from them), so a straight path of it that does is a move through
    // one; and where it ends it is not inside the margin the engine keeps
    if map.collision.test_vector(TEST_FRONT_FACING | TEST_BACK_FACING, base_from, delta, 1.0).is_some() {
        return Err(RejectReason::ThroughSurface);
    }
    let footing = footing(&map.collision, base_to, height, radius, GROUND_TOLERANCE);
    if footing.penetration > PENETRATION_TOLERANCE {
        // a player can be put a little inside something (a start next to an
        // overhang) and the engine lets them stay: what no move does is go deeper
        let before = crate::pill::footing(&map.collision, base_from, height, radius, 0.0);
        if footing.penetration > before.penetration + DEEPER_TOLERANCE {
            return Err(RejectReason::ThroughSurface);
        }
    }
    if !footing.supported {
        return Err(RejectReason::OffGround);
    }
    Ok(())
}
