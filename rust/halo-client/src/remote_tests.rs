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

/// A player running who stops or turns round on the tick after an update: the controls from then on.
fn changing_track(map: &MapData, interval: u32, then: Controls) -> (Vec<Truth>, usize) {
    // (the update the change follows: the first at or after tick 40)
    let last = 40usize.div_ceil(interval as usize) * interval as usize;
    let change = last + 1;
    let truth = track(map, 60, 130, |k| if k < 60 + change { running(0.0) } else { then });
    (truth, last)
}

fn stop_and_reversal(then: Controls, name: &str) {
    let map = flat_floor_map();
    let speed = map.movement.run_forward_speed;
    for interval in INTERVALS {
        let (truth, last) = changing_track(&map, interval, then);
        let drawn = play(&map, &truth, interval, 0, Some(&map));
        // how far off the drawn position gets
        let bound = 2.0 * speed * interval as f32 / halo_sim::TICKS_PER_SECOND as f32;
        let (error, step) = worst(&drawn, &truth, 0);
        assert!(error <= bound, "{name}, every {interval} ticks: off the track by {error}, more than {bound}");
        // (a late update's correction takes a share of what it corrects each tick, on top of the speed)
        let allowed = step_limit(&map) + (1.0 - remote::FADE) * error;
        assert!(step <= allowed, "{name}, every {interval} ticks: moved {step} in a tick, more than {allowed}");
        // and it is on the track again, to 0.05, 4 ticks after the first update that follows the
        // movement having settled (it takes the player about 12 ticks to stop or turn round)
        let settled_at = last + 1 + 12 + interval as usize + 4;
        let off = (settled_at..truth.len()).map(|k| distance(drawn[k].position, truth[k].position)).fold(0.0, f32::max);
        assert!(off <= 0.05, "{name}, every {interval} ticks: still {off} off from tick {settled_at}");
        // a player who stops or turns for one tick of the updates is better followed: the next update
        // after the change brings them back within 0.05 in 4 ticks when it comes in 2 ticks or fewer
        if interval <= 2 {
            let next = last + interval as usize;
            let off =
                (next + 4..truth.len()).map(|k| distance(drawn[k].position, truth[k].position)).fold(0.0, f32::max);
            assert!(off <= 0.05, "{name}, every {interval} ticks: {off} off 4 ticks after the next update");
        }
    }
}

#[test]
fn a_player_who_stops_is_drawn_off_by_less_than_twice_the_distance_to_the_next_update_and_settles() {
    stop_and_reversal(Controls::standing(0.0), "stop");
}

#[test]
fn a_player_who_turns_round_is_drawn_off_by_less_than_twice_the_distance_to_the_next_update_and_settles() {
    stop_and_reversal(Controls { forward: -1.0, ..Controls::standing(0.0) }, "reversal");
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
