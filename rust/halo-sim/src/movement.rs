use halo_map::collision::{TEST_BACK_FACING, TEST_FRONT_FACING};

use crate::map::MapData;
use crate::step::{PlayerInput, RejectReason};

pub const TICKS_PER_SECOND: u32 = 30;

/// The fastest a player may travel, in world units per second. A bound the
/// server enforces, not the player's real speed: it sits above the fastest
/// on-foot speed (which later tickets read from the map's tags) with room for
/// network jitter.
pub const MAX_MOVE_SPEED: f32 = 4.0;

/// The most a move may cover in one tick, squared, so that checking it needs
/// no square root.
pub const MAX_MOVE_SPEED_SQUARED_PER_TICK: f32 =
    (MAX_MOVE_SPEED / TICKS_PER_SECOND as f32) * (MAX_MOVE_SPEED / TICKS_PER_SECOND as f32);

/// How far above the reported position the ground probe starts, so that a
/// position exactly on a surface still finds it.
pub const GROUND_PROBE_HEIGHT: f32 = 0.05;

/// How far below the reported position ground may be for the player to count
/// as standing on it.
pub const GROUND_TOLERANCE: f32 = 0.05;

fn finite(values: &[f32]) -> bool {
    // NaN fails the comparison, and so does infinity
    values.iter().all(|c| c.abs() <= f32::MAX)
}

/// Whether a player at `from` may make the reported move, or why not.
pub(crate) fn validate(map: &MapData, from: [f32; 3], input: &PlayerInput) -> Result<(), RejectReason> {
    let to = input.position;
    if !finite(&to) || !finite(&[input.yaw, input.pitch]) {
        return Err(RejectReason::NotFinite);
    }
    let delta = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    let squared = delta[0] * delta[0] + delta[1] * delta[1] + delta[2] * delta[2];
    // both ends are finite, so the square is a number or, on overflow, infinity
    if squared > MAX_MOVE_SPEED_SQUARED_PER_TICK {
        return Err(RejectReason::TooFast);
    }
    if map.collision.test_vector(TEST_FRONT_FACING | TEST_BACK_FACING, from, delta, 1.0).is_some() {
        return Err(RejectReason::ThroughSurface);
    }
    let probe = [to[0], to[1], to[2] + GROUND_PROBE_HEIGHT];
    if map.collision.ray_down(probe, GROUND_PROBE_HEIGHT + GROUND_TOLERANCE).is_none() {
        return Err(RejectReason::OffGround);
    }
    Ok(())
}
