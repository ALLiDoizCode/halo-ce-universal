//! How a remote player is drawn between updates, as the session does it, tick by tick: a scripted
//! track of one player (walked by the simulation's own movement on the fixture map, packed and
//! unpacked as the wire does) is fed to the session as updates every 1, 2, 4, 10 or 15 ticks, and
//! what it draws on each tick is held to the track. No socket and no clock: the time is given.

use std::time::Duration;

use halo_sim::fixtures::flat_floor_map;
use halo_sim::walk::{walk, Body, Controls};
use halo_sim::{MapData, FLAG_AIRBORNE};
use halo_wire::unit::{Bounds, PackedState, UnitState};

use super::*;

const PLAYER: u16 = 7;
const INTERVALS: [u32; 5] = [1, 2, 4, 10, 15];
/// The tick numbers of a track begin here (0 is "no tick yet" to the session).
const FIRST_TICK: u32 = 1000;

fn time(start: Instant, tick: usize) -> Instant {
    start + Duration::from_secs_f64(tick as f64 / 30.0)
}

/// Where the player really is on one tick.
#[derive(Debug, Clone, Copy)]
struct Truth {
    position: [f32; 3],
    velocity: [f32; 3],
    airborne: bool,
}

/// The track of a player who `warm_up` ticks of `controls` have brought to speed, and then
/// `ticks` more of them.
fn track(map: &MapData, warm_up: usize, ticks: usize, controls: impl Fn(usize) -> Controls) -> Vec<Truth> {
    let mut body = Body::at([0.0, 0.0, 0.0]);
    let mut out = Vec::new();
    for step in 0..warm_up + ticks {
        walk(map, &mut body, &controls(step));
        if step >= warm_up {
            out.push(Truth { position: body.position, velocity: body.velocity_per_second(), airborne: body.airborne });
        }
    }
    out
}

fn running(yaw: f32) -> Controls {
    Controls { forward: 1.0, ..Controls::standing(yaw) }
}

/// What the gateway sends of a tick of the track: packed against the map's bounds.
fn sent(map: &MapData, truth: &Truth, tick: u32) -> UnitState {
    let bounds = Bounds::from_world(map.world_bounds);
    let state = UnitState {
        player: PLAYER,
        position: truth.position,
        velocity: truth.velocity,
        yaw: 0.0,
        pitch: 0.0,
        tick: tick as u8,
        flags: if truth.airborne { FLAG_AIRBORNE } else { 0 },
    };
    PackedState::pack(&state, &bounds).unpack(&bounds)
}

/// One session's view of one player, fed a track.
struct Watch {
    shared: Shared,
    start: Instant,
}

impl Watch {
    fn new() -> Watch {
        let mut shared = Shared::default();
        shared.roster.insert(PLAYER, Member { team: 0, name: "p".into() });
        Watch { shared, start: Instant::now() }
    }

    /// A datagram of tick `k` of the track comes, with `truth` of it.
    fn deliver(&mut self, k: usize, truth: &Truth, map: &MapData, ground: Option<&MapData>) {
        let tick = FIRST_TICK + k as u32;
        let at = time(self.start, k);
        self.shared.newest_tick = tick;
        self.shared.newest_at = Some(at);
        take_state(&mut self.shared, sent(map, truth, tick), tick, at, ground);
    }

    /// What is drawn of the player at the time of tick `k` of the track.
    fn drawn(&self, k: usize, ground: Option<&MapData>) -> Option<DrawnUnit> {
        frame_with(&self.shared, time(self.start, k), ground).units.first().copied()
    }
}

/// What each tick of a track draws, with an update every `interval` ticks from `first` on.
/// `ground` is the map handed the session, or none.
fn play(map: &MapData, truth: &[Truth], interval: u32, first: usize, ground: Option<&MapData>) -> Vec<DrawnUnit> {
    let mut watch = Watch::new();
    let mut drawn = Vec::new();
    for (k, t) in truth.iter().enumerate() {
        if k >= first && (k - first).is_multiple_of(interval as usize) {
            watch.deliver(k, t, map, ground);
        }
        if let Some(unit) = watch.drawn(k, ground) {
            drawn.push(unit);
        }
    }
    drawn
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    remote::length([a[0] - b[0], a[1] - b[1], a[2] - b[2]])
}

/// The most the drawn position is off the track from tick `from` on, and the most it moves in a tick.
fn worst(drawn: &[DrawnUnit], truth: &[Truth], from: usize) -> (f32, f32) {
    let error = (from..drawn.len()).map(|k| distance(drawn[k].position, truth[k].position)).fold(0.0, f32::max);
    let step = (1..drawn.len()).map(|k| distance(drawn[k].position, drawn[k - 1].position)).fold(0.0, f32::max);
    (error, step)
}

/// A tick's movement at the fastest speed the server allows, which the drawn position may exceed by half.
fn step_limit(map: &MapData) -> f32 {
    1.5 * map.max_move_speed() / halo_sim::TICKS_PER_SECOND as f32
}

#[test]
fn a_player_running_in_a_straight_line_is_drawn_on_their_track_at_every_update_interval() {
    let map = flat_floor_map();
    let truth = track(&map, 60, 150, |_| running(0.3));
    for interval in INTERVALS {
        let drawn = play(&map, &truth, interval, 0, Some(&map));
        assert_eq!(drawn.len(), truth.len());
        let (error, step) = worst(&drawn, &truth, 0);
        assert!(error <= 0.05, "every {interval} ticks: off the track by {error}");
        assert!(step <= step_limit(&map), "every {interval} ticks: moved {step} in a tick");
    }
}

#[test]
fn a_player_who_jumps_and_falls_onto_flat_ground_is_drawn_near_their_track_and_never_below_the_ground() {
    let map = flat_floor_map();
    for (name, run) in [("standing", 0.0), ("running", 1.0)] {
        let truth = track(&map, 20, 80, |k| Controls { forward: run, jump: k == 20, ..Controls::standing(0.0) });
        assert!(truth.iter().any(|t| t.airborne) && !truth[79].airborne, "{name}: the track jumps and lands");
        for interval in INTERVALS {
            // (the first update is of the take-off: a jump nobody has sent a state of yet is not known, and
            // the player is off the ground by what an update's interval allows; the landing is not announced)
            let drawn = play(&map, &truth, interval, 0, Some(&map));
            let (error, step) = worst(&drawn, &truth, 0);
            assert!(error <= 0.1, "{name}, every {interval} ticks: off the track by {error}");
            assert!(step <= step_limit(&map), "{name}, every {interval} ticks: moved {step} in a tick");
            let lowest = drawn.iter().map(|d| d.position[2]).fold(f32::MAX, f32::min);
            assert!(lowest >= -1e-4, "{name}, every {interval} ticks: drawn down to {lowest}, below the ground");
        }
    }
}

#[test]
fn with_no_map_an_airborne_player_is_carried_on_for_four_ticks_and_then_held() {
    let map = flat_floor_map();
    let truth = track(&map, 20, 60, |k| Controls { forward: 1.0, jump: k == 20, ..Controls::standing(0.0) });
    // one update, of a tick high in the air, and then none
    let up = truth.iter().position(|t| t.airborne).unwrap() + 2;
    let mut watch = Watch::new();
    watch.deliver(up, &truth[up], &map, None);
    let at_arrival = watch.drawn(up, None).unwrap();
    let mut moved = Vec::new();
    for k in up..up + 12 {
        let unit = watch.drawn(k, None).unwrap();
        moved.push(distance(unit.position, at_arrival.position));
    }
    // (a tick takes it further, up to the fourth)
    assert!(moved[1] > 0.0 && moved[4] > moved[3] && moved[3] > moved[2], "{moved:?}");
    assert_eq!(moved[5], moved[4], "held after four ticks: {moved:?}");
    assert_eq!(moved[11], moved[4]);
    // (it is the same player the map would have carried on: no further than the fourth tick's)
    let with_map = watch.drawn(up + 4, Some(&map)).unwrap();
    assert!(distance(with_map.position, watch.drawn(up + 4, None).unwrap().position) < 1e-6);
    assert!(watch.drawn(up + 11, Some(&map)).unwrap().position != watch.drawn(up + 11, None).unwrap().position);
}

/// A scripted track of a player who runs along +x at the map's speed and, on the tick after the update at
/// tick `last`, stops or turns round (their velocity changes at once, as a track of straight lines does):
/// `then` is their speed along +x from then on. Returns the track and `last`.
fn changing_track(map: &MapData, interval: u32, then: f32) -> (Vec<Truth>, usize) {
    let run = map.movement.run_forward_speed;
    let per_tick = 1.0 / halo_sim::TICKS_PER_SECOND as f32;
    // (the update the change follows: the first at or after tick 40)
    let last = 40usize.div_ceil(interval as usize) * interval as usize;
    let mut x = -20.0;
    let truth = (0..130)
        .map(|k| {
            let v = if k <= last { run } else { then };
            x += v * per_tick;
            Truth { position: [x, 0.0, 0.0], velocity: [v, 0.0, 0.0], airborne: false }
        })
        .collect();
    (truth, last)
}

/// What a stop or a reversal comes to at one update interval, from the tick of the update after the change.
struct Change {
    /// How far off the drawn position is when that update arrives.
    error: f32,
    /// The ticks after it that it takes to be within 0.05 of the track, for good.
    settled_after: usize,
    /// The most the drawn position moves in a tick, over the whole track.
    step: f32,
    /// How much of the error a tick can close, for the player the update is of.
    closes: f32,
}

fn change(map: &MapData, interval: u32, then: f32) -> Change {
    let (truth, last) = changing_track(map, interval, then);
    let drawn = play(map, &truth, interval, 0, Some(map));
    let next = last + interval as usize;
    let off = |k: usize| distance(drawn[k].position, truth[k].position);
    // (the drawn position at the arrival, before the update is taken: the tick before, and a tick on)
    let error = off(next).max(drawn_before(map, &truth, interval, next));
    let settled_after = (0..60).find(|n| (next + n..truth.len()).all(|k| off(k) < 0.05)).expect("it settles");
    Change { error, settled_after, step: worst(&drawn, &truth, 0).1, closes: remote::correction_step([then, 0.0, 0.0]) }
}

/// How far off the drawn position is on tick `next`, just before the update of that tick is taken.
fn drawn_before(map: &MapData, truth: &[Truth], interval: u32, next: usize) -> f32 {
    let mut watch = Watch::new();
    for (k, t) in truth.iter().enumerate().take(next + 1) {
        if k < next && k % interval as usize == 0 {
            watch.deliver(k, t, map, Some(map));
        }
    }
    // (time at `next`, with the newest tick the last update's)
    let unit = watch.drawn(next, Some(map)).unwrap();
    distance(unit.position, truth[next].position)
}

/// Stops and reversals, at every interval: how far off they put the drawn position is at most twice the
/// speed times the interval, the drawn position never moves more than 1.5 times the fastest legal speed in
/// a tick, and it is within 0.05 of the track 4 ticks after the next update wherever the offset can be closed
/// that fast by steps of that size.
fn stop_and_reversal(then: f32, name: &str, cannot_settle_in_4: &[u32], snapped: &[u32]) {
    let map = flat_floor_map();
    let speed = map.movement.run_forward_speed;
    for interval in INTERVALS {
        let c = change(&map, interval, then);
        let bound = 2.0 * speed * interval as f32 / halo_sim::TICKS_PER_SECOND as f32;
        assert!(c.error <= bound, "{name}, every {interval} ticks: off the track by {}, more than {bound}", c.error);
        if snapped.contains(&interval) {
            // more than the 2 units that are closed in steps: the player is drawn at the state's position at once,
            // which is a move of the whole error in a tick, and the player is on the track at once
            assert!(c.error > remote::SNAP_DISTANCE, "{name}, every {interval} ticks: {} is not a snap", c.error);
            assert!(c.step >= 0.9 * c.error, "{name}, every {interval} ticks: moved {} for {}", c.step, c.error);
            assert!(c.settled_after <= 4, "{name}, every {interval} ticks: {} ticks to settle", c.settled_after);
            continue;
        }
        assert!(
            c.step <= step_limit(&map),
            "{name}, every {interval} ticks: moved {} in a tick, more than {}",
            c.step,
            step_limit(&map)
        );
        // 4 ticks of the largest step close at most this much (the track's own movement is what is left)
        let closable = 4.0 * c.closes + 0.05;
        if cannot_settle_in_4.contains(&interval) {
            // the two criteria cannot both hold: the step bound is kept and the offset takes as long as the steps need
            assert!(c.error > closable, "{name}, every {interval} ticks: {} could be closed in 4 ticks", c.error);
            let steps = ((c.error - 0.05) / c.closes).ceil() as usize;
            assert!(
                c.settled_after <= steps + 1,
                "{name}, every {interval} ticks: {} ticks, not {steps}",
                c.settled_after
            );
            assert!(c.settled_after > 4, "{name}, every {interval} ticks: settled in {}", c.settled_after);
        } else {
            assert!(c.error <= closable, "{name}, every {interval} ticks: {} cannot be closed in 4 ticks", c.error);
            assert!(c.settled_after <= 4, "{name}, every {interval} ticks: {} ticks to settle", c.settled_after);
        }
    }
}

#[test]
fn a_player_who_stops_is_drawn_within_twice_the_distance_to_the_next_update_and_settles_in_4_ticks() {
    // (at 15 ticks the drawn player is 1.11 off: 4 ticks of the largest step close 0.8)
    stop_and_reversal(0.0, "stop", &[15], &[]);
}

#[test]
fn a_player_who_turns_round_is_drawn_within_twice_the_distance_to_the_next_update_and_settles() {
    // (a reversal at the backward run speed: at 10 ticks the drawn player is 1.37 off, which 4 ticks of the largest
    // step cannot close, and at 15 it is 2.06 off, which is more than is closed in steps: it is a snap)
    stop_and_reversal(-1.9, "reversal", &[10], &[15]);
}

#[test]
fn a_state_older_than_the_planners_staleness_cap_stops_the_player_and_their_velocity_is_zero() {
    let map = flat_floor_map();
    let truth = track(&map, 60, 80, |_| running(0.0));
    let mut watch = Watch::new();
    watch.deliver(0, &truth[0], &map, Some(&map));
    let cap = remote::extrapolation_limit() as usize;
    assert_eq!(cap, 15, "the cap is the planner's");
    let before = watch.drawn(cap - 1, Some(&map)).unwrap();
    assert!(before.velocity[0] > 2.0, "{:?}", before.velocity);
    let at_cap = watch.drawn(cap, Some(&map)).unwrap();
    for k in cap + 1..cap + 20 {
        let unit = watch.drawn(k, Some(&map)).unwrap();
        assert_eq!(unit.position, at_cap.position, "tick {k}: held");
        assert_eq!(unit.velocity, [0.0; 3], "tick {k}: no velocity for the engine to play a run from");
    }
    // the facing and the flags are the state's all along
    assert_eq!(at_cap.state.flags, truth_flags(&truth[0]));
}

fn truth_flags(t: &Truth) -> u8 {
    if t.airborne {
        FLAG_AIRBORNE
    } else {
        0
    }
}

#[test]
fn a_state_far_from_where_the_player_was_drawn_puts_them_there_at_once() {
    let map = flat_floor_map();
    let truth = track(&map, 60, 60, |_| running(0.0));
    let mut watch = Watch::new();
    watch.deliver(0, &truth[0], &map, Some(&map));
    // a respawn: the next state is 5 units away (more than the 2 that are faded)
    let mut moved = truth[10];
    moved.position[1] += 5.0;
    watch.deliver(10, &moved, &map, Some(&map));
    let unit = watch.drawn(10, Some(&map)).unwrap();
    assert!(distance(unit.position, moved.position) < 0.01, "drawn at {:?}, sent {:?}", unit.position, moved.position);
    let later = watch.drawn(11, Some(&map)).unwrap();
    // (no fade: only the player's own movement from there)
    assert!(distance(later.position, unit.position) < 0.1);
    // while one under that is faded in, so that the player does not jump
    let mut near = truth[20];
    // (the player has been drawn 5 units over since the respawn; this state is 1 unit further)
    near.position[1] += 6.0;
    watch.deliver(20, &near, &map, Some(&map));
    let unit = watch.drawn(20, Some(&map)).unwrap();
    let mut was = truth[20].position;
    was[1] += 5.0;
    assert!(distance(unit.position, was) < 0.05, "drawn where they were being drawn, not jumped to {near:?}");
    let (a, b) = (watch.drawn(21, Some(&map)).unwrap(), watch.drawn(24, Some(&map)).unwrap());
    let off = |u: &DrawnUnit, k: usize| distance(u.position, near_at(&near, &truth, 20, k));
    assert!(off(&b, 24) < off(&a, 21) && off(&a, 21) < 1.0, "{} {}", off(&a, 21), off(&b, 24));
}

/// Where a player at `near` on tick `from` is on tick `k`, running.
fn near_at(near: &Truth, truth: &[Truth], from: usize, k: usize) -> [f32; 3] {
    let mut p = near.position;
    for (axis, p) in p.iter_mut().enumerate() {
        *p += truth[k].position[axis] - truth[from].position[axis];
    }
    p
}

#[test]
fn the_error_a_late_update_leaves_is_what_the_game_logs() {
    let map = flat_floor_map();
    let truth = track(&map, 60, 60, |k| if k < 60 + 5 { running(0.0) } else { Controls::standing(0.0) });
    let mut watch = Watch::new();
    watch.deliver(0, &truth[0], &map, Some(&map));
    assert_eq!(watch.drawn(0, Some(&map)).unwrap().arrival_error, 0.0, "a first state is drawn where it says");
    watch.deliver(15, &truth[15], &map, Some(&map));
    let unit = watch.drawn(15, Some(&map)).unwrap();
    // the player stopped after 5 ticks and was carried on for 15: more than half a unit
    assert!(unit.arrival_error > 0.4 && unit.arrival_error < 1.2, "{}", unit.arrival_error);
    assert_eq!(unit.tick, FIRST_TICK + 15);
}

#[test]
fn with_datagrams_stopped_for_five_ticks_the_players_go_on_along_their_extrapolation() {
    let map = flat_floor_map();
    let truth = track(&map, 60, 40, |_| running(0.0));
    let mut watch = Watch::new();
    watch.deliver(0, &truth[0], &map, Some(&map));
    // (the newest tick stays 0's: nothing has come)
    let mut last = watch.drawn(0, Some(&map)).unwrap().position[0];
    for (k, truth) in truth.iter().enumerate().take(6).skip(1) {
        let unit = watch.drawn(k, Some(&map)).unwrap();
        assert!(unit.position[0] > last + 0.05, "tick {k}: {} after {last}", unit.position[0]);
        assert!(distance(unit.position, truth.position) < 0.05);
        last = unit.position[0];
    }
}

#[test]
fn a_player_back_in_range_is_drawn_where_the_state_says_without_a_fade() {
    let map = flat_floor_map();
    let truth = track(&map, 60, 400, |_| running(0.0));
    let mut watch = Watch::new();
    watch.deliver(0, &truth[0], &map, Some(&map));
    // out of range for a long while (the player is not in the frame), then sent again, 1.5 units on
    let back = 3 * halo_wire::planner::STALENESS_BOUND_TICKS as usize + 10;
    let mut again = truth[back];
    again.position[1] += 1.5;
    watch.deliver(back, &again, &map, Some(&map));
    let unit = watch.drawn(back, Some(&map)).unwrap();
    assert_eq!(unit.arrival_error, 0.0);
    assert!(distance(unit.position, again.position) < 0.01);
}
