//! Where another player is drawn between the gateway's updates.
//!
//! The gateway sends a far player a few times a second, so the session holds
//! the newest state of each player (their *held state*) and draws them where
//! they would be by now: the held state's position carried forward by its
//! velocity for the ticks since (and gravity too, while it says the player is
//! in the air, down to the ground). The position that comes of it is the
//! *drawn position*. See `docs/adr/0001-remote-players-are-extrapolated.md`
//! where it exists, and the issue it came from (#50).
//!
//! - It stops at [`extrapolation_limit`] ticks (the planner's staleness cap): a
//!   player nothing has been heard of for longer is held where they were last
//!   drawn, with no velocity, so that the engine shows them standing.
//! - An airborne player with no map to find the ground in is extrapolated for
//!   [`BLIND_AIRBORNE_TICKS`] only.
//! - A state that arrives where the extrapolation was not leaves a difference
//!   between where the player was drawn and where the new state puts them. It
//!   is kept as an offset on top of the new extrapolation and shrinks by
//!   closed by at most [`correction_step`] a tick (so that the player never
//!   moves faster than 1.5 times the fastest legal speed, whatever the
//!   correction adds to their own movement), unless it is more than
//!   [`SNAP_DISTANCE`] (a respawn, a teleport), when the player is drawn at the
//!   new state's position at once.
//!
//! The functions here take what they work from and no clock or socket, so
//! that tests can run them tick by tick.

use halo_sim::walk::GRAVITY;
use halo_sim::{MapData, FLAG_AIRBORNE, TICKS_PER_SECOND};

use crate::session::RemoteUnit;

/// How long an airborne player is extrapolated, in ticks, when no map is in to
/// find the ground with.
pub const BLIND_AIRBORNE_TICKS: f32 = 4.0;

/// The fastest a player may legally move, world units a second: the wire's
/// velocity range is 1.5 times the server's speed bound.
const FASTEST_LEGAL: f32 = halo_wire::unit::VELOCITY_RANGE / 1.5;

/// How much faster than the fastest legal speed a drawn player may be seen to
/// move, in the tick a late update's offset is being closed.
const CORRECTION_SPEED_FACTOR: f32 = 1.5;

/// How far a late update's offset may be closed in a tick, in world units, for
/// a player whose state has `velocity` (world units a second): what the player's
/// own movement of a tick leaves of 1.5 times the fastest legal speed's. The
/// offset and the velocity may point the same way, so they are added.
pub fn correction_step(velocity: [f32; 3]) -> f32 {
    let per_tick = TICKS_PER_SECOND as f32;
    (CORRECTION_SPEED_FACTOR * FASTEST_LEGAL / per_tick - length(velocity) / per_tick).max(0.0)
}

/// How far, in world units, a new state may be from where the player was
/// drawn and have the difference faded: past it the player is drawn at the new
/// state's position.
pub const SNAP_DISTANCE: f32 = 2.0;

/// How far above the player the probe for the ground begins, in world units.
const PROBE_ABOVE: f32 = 0.1;

/// How many ticks a held state is extrapolated for: the planner's staleness
/// cap (a player is sent at least this often, loss aside).
pub fn extrapolation_limit() -> f32 {
    halo_wire::planner::PlannerConfig::with_budget(0).max_stale_ticks as f32
}

/// Where a remote player is drawn, and the velocity the engine is handed for
/// them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Drawn {
    pub position: [f32; 3],
    /// World units a second: the held state's, with gravity's part of it for
    /// an airborne player; zero for a player held where they are (see the
    /// module's limit), so that the engine plays a standing animation.
    pub velocity: [f32; 3],
    /// How many of the ticks asked for were past what is extrapolated: how
    /// long the player has been held.
    pub held_ticks: f32,
}

/// The path of a held state `age` ticks on: the position and velocity with no
/// offset and nothing of the ground.
fn path(unit: &RemoteUnit, age: f32, map: Option<&MapData>) -> (Drawn, bool) {
    let s = &unit.state;
    let airborne = s.flags & FLAG_AIRBORNE != 0;
    let mut used = age.max(0.0);
    let mut limit = extrapolation_limit();
    if airborne && map.is_none() {
        limit = limit.min(BLIND_AIRBORNE_TICKS);
    }
    let held = used > limit;
    if held {
        used = limit;
    }
    let per_tick = TICKS_PER_SECOND as f32;
    let mut position = [0.0; 3];
    let mut velocity = [0.0; 3];
    for axis in 0..3 {
        position[axis] = s.position[axis] + s.velocity[axis] / per_tick * used;
        velocity[axis] = s.velocity[axis];
    }
    if airborne {
        // (a tick lowers the velocity by gravity and then moves the player by it)
        position[2] -= GRAVITY * used * (used + 1.0) * 0.5;
        velocity[2] -= GRAVITY * used * per_tick;
    }
    if held {
        velocity = [0.0; 3];
    }
    (Drawn { position, velocity, held_ticks: age.max(0.0) - used }, airborne)
}

/// A position that has fallen through the ground, put on it. A probe from just
/// above the higher end of the fall down to where it ended: what it meets is
/// ground the player has come down onto.
fn on_the_ground(map: &MapData, from_z: f32, position: &mut [f32; 3], velocity: &mut [f32; 3]) {
    let top = from_z.max(position[2]) + PROBE_ABOVE;
    if let Some(hit) = map.collision.ray_down([position[0], position[1], top], top - position[2]) {
        if hit.z > position[2] {
            position[2] = hit.z;
        }
        // (they have landed: nothing falls on)
        velocity[2] = 0.0;
    }
}

/// Where `unit`, whose state is `age` ticks old, is drawn. `late` is what a late
/// update left, if one did, and the ticks since (the offset fades while the
/// player moves, and stops with them). `map` is the map's collision data, once
/// it is loaded.
pub fn drawn(unit: &RemoteUnit, age: f32, late: Option<(&Track, f32)>, map: Option<&MapData>) -> Drawn {
    let (mut out, airborne) = path(unit, age, map);
    if let Some((track, since_arrival)) = late {
        let offset = track.faded(since_arrival - out.held_ticks);
        for (p, o) in out.position.iter_mut().zip(offset) {
            *p += o;
        }
    }
    if airborne {
        if let Some(map) = map {
            on_the_ground(map, unit.state.position[2], &mut out.position, &mut out.velocity);
        }
    }
    out
}

/// The difference a late update leaves, kept to close: where the player was
/// drawn less where the new state puts them. `None` when it is too much to close.
pub fn late_offset(was_drawn: [f32; 3], now_extrapolated: [f32; 3]) -> Option<[f32; 3]> {
    let offset =
        [was_drawn[0] - now_extrapolated[0], was_drawn[1] - now_extrapolated[1], was_drawn[2] - now_extrapolated[2]];
    (length(offset) <= SNAP_DISTANCE).then_some(offset)
}

pub fn length(v: [f32; 3]) -> f32 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

/// A late update's offset and the time it was left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Track {
    pub offset: [f32; 3],
    /// Where the offset began, as the distance it was, for the log: how far
    /// off the drawn position was when the state came.
    pub error: f32,
    /// How much of it is closed each tick ([`correction_step`] of the state's velocity).
    pub step: f32,
    /// When the state came.
    pub arrived: std::time::Instant,
}

impl Track {
    /// The offset after `ticks` ticks (whole or part) of closing it.
    pub fn faded(&self, ticks: f32) -> [f32; 3] {
        let size = length(self.offset);
        if size <= 0.0 {
            return [0.0; 3];
        }
        let left = (size - self.step * ticks.max(0.0)).max(0.0) / size;
        [self.offset[0] * left, self.offset[1] * left, self.offset[2] * left]
    }
}
