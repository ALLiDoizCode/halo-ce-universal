//! A unit's state in 16 bytes.
//!
//! ```text
//! offset  size
//!  0      u16     player id
//!  2      u16 x3  x, y, z: 0 is the map's lower world bound, 65535 the upper
//!  8      i8  x3  velocity x, y, z: 127 is VELOCITY_RANGE world units a second
//! 11      u16     yaw: 65536 is a full turn, counted from 0 radians
//! 13      i8      pitch: 127 is straight up (+pi/2), -127 straight down
//! 14      u8      the low 8 bits of the tick this state is from
//! 15      u8      flags (see below)
//! ```
//!
//! # Flags, and how a remote player's shots reach the others
//!
//! ```text
//! bit 0      airborne    the server's judgement of the moves it accepted (`halo_sim::FLAG_AIRBORNE`)
//! bit 1      crouched    the player's own report (`halo_sim::FLAG_CROUCHED`)
//! bits 2-4   shots       how many shots the player's weapon has fired, modulo 8
//!                        (`halo_sim::FLAG_SHOTS_MASK`)
//! bit 5      reloading   the player's weapon is reloading (`halo_sim::FLAG_RELOADING`)
//! bits 6-7   free
//! ```
//!
//! Firing is carried in the state and not in a table of shots, or an event of its own. A table of
//! every shot of everybody is a write per shot and a row for every client to hear of; an event the
//! gateway had to send would need its own packets, its own loss handling and its own priority. A
//! state is already sent to exactly the recipients the player matters to (the ones in range), under
//! the budget, and a lost one costs one update. So the 8 bits of flags cost no byte more: the
//! shooter's client counts its own shots (`halo_sim::weapon::Hands` says when the trigger fired) and
//! reports the count with each input, the server passes the counter on (`halo_sim::step`: a client
//! says its crouch, shot counter and reload, never the air), and the gateway puts it in the packed
//! state of every recipient that gets the shooter.
//!
//! It is a *count* because far players are sent only 6 to 8 times a second and the shots of an
//! automatic weapon come faster than that: a recipient that last saw count `a` and now sees `b` knows
//! that `(b - a) mod 8` shots were fired in between (`halo_sim::shots_between`), however long since
//! it last heard, so that no shot of those it can tell is lost to a datagram that was. (Eight shots
//! between two states, a whole turn, reads as none. The planner sends every player in range at least
//! every `max_stale_ticks`, 15 by default, half a second: 5 shots of the fastest weapon at 10 a
//! second. Under heavy loss the gap can reach the 40 ticks of `STALENESS_BOUND_TICKS`, 13 shots, and
//! the shots that wrapped are not shown; nothing else is hurt by it.) A recipient that
//! has just got a player in range takes the count it sees as the start and plays nothing for it.
//! Whether a shot hit is not here: the shooter reports hits to the server (the match's
//! `report_hits`), the server's `fighter` table says what they did, and what a recipient shows of a
//! hit (a shield flash, a pain sound, the damage on its own screen) comes from that.
//!
//! All little-endian. A coordinate is quantised to its axis's range over
//! 65535, so on a 150-unit axis a step is 0.0023 units.

use std::f32::consts::{FRAC_PI_2, TAU};

/// Bytes of one packed state.
pub const UNIT_STATE_SIZE: usize = 16;

/// What an 8-bit velocity component of 127 stands for, in world units a
/// second: 1.5 times the server's speed bound, so that a legal move never
/// saturates.
pub const VELOCITY_RANGE: f32 = 6.0;

/// The map's world bounds: what positions are quantised against.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

impl Bounds {
    /// From `x0, x1, y0, y1, z0, z1`, the order of `MapData::world_bounds`.
    pub fn from_world(w: [f32; 6]) -> Bounds {
        Bounds { min: [w[0], w[2], w[4]], max: [w[1], w[3], w[5]] }
    }

    pub fn to_world(self) -> [f32; 6] {
        [self.min[0], self.max[0], self.min[1], self.max[1], self.min[2], self.max[2]]
    }
}

/// A unit state in the units the simulation uses.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UnitState {
    pub player: u16,
    pub position: [f32; 3],
    /// World units a second.
    pub velocity: [f32; 3],
    /// Radians.
    pub yaw: f32,
    pub pitch: f32,
    /// The low 8 bits of the tick the state is from.
    pub tick: u8,
    pub flags: u8,
}

/// A state as it goes over the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackedState(pub [u8; UNIT_STATE_SIZE]);

impl PackedState {
    pub fn player(&self) -> u16 {
        u16::from_le_bytes([self.0[0], self.0[1]])
    }

    pub fn pack(state: &UnitState, bounds: &Bounds) -> PackedState {
        let mut b = [0u8; UNIT_STATE_SIZE];
        b[0..2].copy_from_slice(&state.player.to_le_bytes());
        for axis in 0..3 {
            let span = bounds.max[axis] - bounds.min[axis];
            let t = if span > 0.0 { (state.position[axis] - bounds.min[axis]) / span } else { 0.0 };
            let q = quantise(t * 65535.0, 0.0, 65535.0) as u16;
            b[2 + 2 * axis..4 + 2 * axis].copy_from_slice(&q.to_le_bytes());
            b[8 + axis] = quantise(state.velocity[axis] / VELOCITY_RANGE * 127.0, -127.0, 127.0) as i8 as u8;
        }
        let turns = (state.yaw / TAU).rem_euclid(1.0);
        let yaw = quantise(turns * 65536.0, 0.0, 65536.0) as u32 as u16; // 65536 wraps to 0
        b[11..13].copy_from_slice(&yaw.to_le_bytes());
        b[13] = quantise(state.pitch / FRAC_PI_2 * 127.0, -127.0, 127.0) as i8 as u8;
        b[14] = state.tick;
        b[15] = state.flags;
        PackedState(b)
    }

    pub fn unpack(&self, bounds: &Bounds) -> UnitState {
        let b = &self.0;
        let mut position = [0.0; 3];
        let mut velocity = [0.0; 3];
        for axis in 0..3 {
            let q = u16::from_le_bytes([b[2 + 2 * axis], b[3 + 2 * axis]]);
            position[axis] = bounds.min[axis] + q as f32 / 65535.0 * (bounds.max[axis] - bounds.min[axis]);
            velocity[axis] = b[8 + axis] as i8 as f32 / 127.0 * VELOCITY_RANGE;
        }
        UnitState {
            player: self.player(),
            position,
            velocity,
            yaw: u16::from_le_bytes([b[11], b[12]]) as f32 / 65536.0 * TAU,
            pitch: b[13] as i8 as f32 / 127.0 * FRAC_PI_2,
            tick: b[14],
            flags: b[15],
        }
    }
}

/// `v` rounded to the nearest integer and clamped to `lo..=hi`; NaN becomes
/// the clamp's lower end or 0, whichever is greater.
fn quantise(v: f32, lo: f32, hi: f32) -> f32 {
    if v.is_nan() {
        return lo.max(0.0);
    }
    v.round().clamp(lo, hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOUNDS: Bounds = Bounds { min: [-60.0, -70.0, -10.0], max: [66.0, 75.0, 40.0] };

    fn state() -> UnitState {
        UnitState {
            player: 0x1234,
            position: [12.5, -33.25, 3.0],
            velocity: [1.5, -2.0, 0.0],
            yaw: 2.0,
            pitch: -0.4,
            tick: 200,
            flags: 0,
        }
    }

    #[test]
    fn a_state_is_16_bytes_and_round_trips_within_the_quantisation_steps() {
        let packed = PackedState::pack(&state(), &BOUNDS);
        assert_eq!(packed.0.len(), 16);
        assert_eq!(packed.player(), 0x1234);
        let back = packed.unpack(&BOUNDS);
        let s = state();
        for axis in 0..3 {
            let step = (BOUNDS.max[axis] - BOUNDS.min[axis]) / 65535.0;
            assert!((back.position[axis] - s.position[axis]).abs() <= step, "axis {axis}");
            assert!((back.velocity[axis] - s.velocity[axis]).abs() <= VELOCITY_RANGE / 127.0);
        }
        assert!((back.yaw - s.yaw).abs() <= TAU / 65536.0);
        assert!((back.pitch - s.pitch).abs() <= FRAC_PI_2 / 127.0);
        assert_eq!((back.player, back.tick, back.flags), (0x1234, 200, 0));
    }

    #[test]
    fn the_byte_layout_is_fixed() {
        let s = UnitState {
            player: 0x0102,
            position: [-60.0, 75.0, 40.0],
            velocity: [VELOCITY_RANGE, -VELOCITY_RANGE, 0.0],
            yaw: 0.0,
            pitch: FRAC_PI_2,
            tick: 7,
            flags: 9,
        };
        let b = PackedState::pack(&s, &BOUNDS).0;
        assert_eq!(&b[0..2], [0x02, 0x01]);
        assert_eq!(&b[2..4], 0u16.to_le_bytes(), "x at its lower bound");
        assert_eq!(&b[4..6], 65535u16.to_le_bytes(), "y at its upper bound");
        assert_eq!(&b[6..8], 65535u16.to_le_bytes());
        assert_eq!(&b[8..11], [127, 129, 0]);
        assert_eq!(&b[11..13], [0, 0]);
        assert_eq!(b[13], 127);
        assert_eq!(&b[14..], [7, 9]);
    }

    #[test]
    fn out_of_range_and_non_finite_values_are_clamped_not_wrapped() {
        let mut s = state();
        s.position = [1e9, -1e9, f32::NAN];
        s.velocity = [100.0, -100.0, f32::INFINITY];
        s.pitch = 10.0;
        let b = PackedState::pack(&s, &BOUNDS).unpack(&BOUNDS);
        assert_eq!(b.position[0], BOUNDS.max[0]);
        assert_eq!(b.position[1], BOUNDS.min[1]);
        assert_eq!(b.velocity[0], VELOCITY_RANGE);
        assert_eq!(b.velocity[1], -VELOCITY_RANGE);
        assert_eq!(b.velocity[2], VELOCITY_RANGE);
        assert!((b.pitch - FRAC_PI_2).abs() < 1e-6);
    }

    #[test]
    fn yaw_wraps_a_full_turn() {
        for yaw in [-1.0f32, 0.0, 3.0, 7.0, TAU - 1e-7] {
            let mut s = state();
            s.yaw = yaw;
            let back = PackedState::pack(&s, &BOUNDS).unpack(&BOUNDS).yaw;
            let diff = (back - yaw).rem_euclid(TAU);
            assert!(diff.min(TAU - diff) <= TAU / 65536.0 + 1e-6, "yaw {yaw} came back as {back}");
        }
    }
}
