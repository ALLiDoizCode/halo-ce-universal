//! The local player's own movement, as the client computes it: the player's
//! controls and the map's collision data go in, where the player is now comes
//! out ([`halo_sim::walk`], the same movement the server's validation is built
//! around). The client shows it at once and reports it; the server only checks
//! the report.
//!
//! The map is the player's own copy of the Xbox map file, which the game has
//! loaded as well, read here for its collision data and its tags' movement
//! values. It is read on a thread of its own, since inflating it takes a
//! moment the game should not wait for: until it is in, [`Local::step`] has
//! nothing to give and the game moves the player as it did before.

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use halo_sim::walk::{walk, Body, Controls};
use halo_sim::MapData;

type Loaded = Arc<OnceLock<Result<MapData, String>>>;

/// What one tick of [`Local::step`] came to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Moved {
    pub position: [f32; 3],
    /// World units a second.
    pub velocity: [f32; 3],
    /// Not standing on anything.
    pub airborne: bool,
    /// In the crouch, or standing up from it ([`Body::crouched`]).
    pub crouched: bool,
    /// How fast the tick drove the player into the ground it landed on, in
    /// world units a *tick* (the engine's unit): what falling damage and the
    /// landing animation read. 0 when the player did not land.
    pub landing_velocity: f32,
    /// The player moved this tick. Not when the tick was ahead of the clock
    /// (see [`Local::step`]): the state is then the last tick's.
    pub stepped: bool,
}

/// How many ticks the player's movement may run ahead of the clock: a frame's
/// jitter.
const AHEAD: u64 = 2;
/// How many ticks behind the clock it may fall before the ticks that were
/// missed are let go (a game that was paused is not owed its time).
const BEHIND: u64 = 15;

pub struct Local {
    map: Loaded,
    body: Option<Body>,
    /// When the first tick was asked for, and how many have been taken since:
    /// the movement is paced by them.
    clock: Option<Instant>,
    taken: u64,
}

impl Local {
    /// Start reading the map file at `path` (an Xbox map file, as the game's
    /// own data folder has it).
    pub fn load(path: String) -> Local {
        let map: Loaded = Arc::new(OnceLock::new());
        let loading = map.clone();
        let spawned = std::thread::Builder::new().name("halo-large-map".into()).spawn(move || {
            let result = halo_map::HaloMap::from_path(&path).map(MapData::from).map_err(|e| format!("{path}: {e}"));
            let _ = loading.set(result);
        });
        if let Err(e) = spawned {
            let _ = map.set(Err(format!("a thread to read the map: {e}")));
        }
        Local { map, body: None, clock: None, taken: 0 }
    }

    /// A player on a map that is already in hand (for tests).
    pub fn with_map(map: MapData) -> Local {
        let loaded: Loaded = Arc::new(OnceLock::new());
        let _ = loaded.set(Ok(map));
        Local { map: loaded, body: None, clock: None, taken: 0 }
    }

    /// Why the map could not be read, if it could not.
    pub fn error(&self) -> Option<String> {
        self.map.get().and_then(|r| r.as_ref().err().cloned())
    }

    /// The map is in and movement will be computed.
    pub fn ready(&self) -> bool {
        matches!(self.map.get(), Some(Ok(_)))
    }

    /// Put the player at `position`, at rest: where the server placed them, or
    /// where the engine has the unit until the map is in.
    pub fn place(&mut self, position: [f32; 3]) {
        self.body = Some(Body::at(position));
    }

    /// One tick of the player's controls. `None` while the map is not in or
    /// the player has not been placed.
    ///
    /// The movement is paced by the clock: it takes a tick of 1/30 second of
    /// real time for every tick it is asked for, give or take [`AHEAD`] ticks'
    /// jitter, and a tick asked for ahead of that is not taken (`stepped` is
    /// false and the state is the last's). A game that runs ticks in a rush
    /// (it does when it loads, and after a hitch it catches up) would otherwise
    /// report several ticks of movement at once, which the server, who counts
    /// the time between the reports, takes for a speed hack.
    pub fn step(&mut self, controls: Controls) -> Option<Moved> {
        self.step_at(controls, Instant::now())
    }

    /// [`Local::step`] with the time it is asked at.
    pub fn step_at(&mut self, controls: Controls, now: Instant) -> Option<Moved> {
        let Some(Ok(map)) = self.map.get() else { return None };
        let body = self.body.as_mut()?;
        let start = *self.clock.get_or_insert(now);
        let due = (now.saturating_duration_since(start).as_secs_f64() * halo_sim::TICKS_PER_SECOND as f64) as u64;
        // (ticks the game was not asked for are let go of, past a few)
        self.taken = self.taken.max(due.saturating_sub(BEHIND));
        let stepped = self.taken < due + AHEAD;
        if stepped {
            walk(map, body, &controls);
            self.taken += 1;
        }
        Some(Moved {
            position: body.position,
            velocity: body.velocity_per_second(),
            airborne: body.airborne,
            crouched: body.crouched(),
            landing_velocity: if stepped { body.landing_velocity } else { 0.0 },
            stepped,
        })
    }

    /// How long one tick of the clock is.
    pub const TICK: Duration = Duration::from_micros(1_000_000 / halo_sim::TICKS_PER_SECOND as u64);
}

#[cfg(test)]
mod tests {
    use super::*;
    use halo_sim::fixtures::flat_floor_map;

    fn forward(yaw: f32) -> Controls {
        Controls { forward: 1.0, strafe: 0.0, yaw, pitch: 0.0, jump: false, crouch: false }
    }

    /// A clock that gives each call the next tick's time.
    struct Clock {
        start: Instant,
        ticks: u64,
    }

    impl Clock {
        fn new() -> Clock {
            Clock { start: Instant::now(), ticks: 0 }
        }

        /// The time of the next tick, in real time.
        fn tick(&mut self) -> Instant {
            self.ticks += 1;
            self.start + Local::TICK * self.ticks as u32
        }

        /// The same instant again: a game that runs a tick in no time.
        fn now(&self) -> Instant {
            self.start + Local::TICK * self.ticks as u32
        }
    }

    fn step(local: &mut Local, clock: &mut Clock, controls: Controls) -> Option<Moved> {
        local.step_at(controls, clock.tick())
    }

    #[test]
    fn there_is_no_movement_until_the_map_is_in_and_the_player_placed() {
        let mut local = Local::with_map(flat_floor_map());
        let mut clock = Clock::new();
        assert_eq!(step(&mut local, &mut clock, forward(0.0)), None, "not placed yet");
        local.place([0.0, 0.0, 0.0]);
        assert!(step(&mut local, &mut clock, forward(0.0)).is_some());
    }

    #[test]
    fn a_player_walks_with_the_controls_they_give() {
        let mut local = Local::with_map(flat_floor_map());
        let mut clock = Clock::new();
        local.place([0.0, 0.0, 0.0]);
        let mut last = None;
        for _ in 0..90 {
            last = step(&mut local, &mut clock, forward(0.0));
        }
        let moved = last.unwrap();
        assert!(moved.position[0] > 2.0 && !moved.airborne && !moved.crouched, "{moved:?}");
        // the velocity is a second's, and is the map's run speed by now
        assert!((moved.velocity[0] - flat_floor_map().movement.run_forward_speed).abs() < 1e-3);
    }

    #[test]
    fn jumping_and_crouching_are_the_players_controls_and_show_in_what_they_come_to() {
        let mut local = Local::with_map(flat_floor_map());
        let mut clock = Clock::new();
        local.place([0.0, 0.0, 0.0]);
        for _ in 0..10 {
            step(&mut local, &mut clock, Controls::standing(0.0));
        }
        let up = step(&mut local, &mut clock, Controls { jump: true, ..Controls::standing(0.0) }).unwrap();
        assert!(up.airborne && up.velocity[2] > 0.0, "{up:?}");
        // (to the ground again, and then crouched)
        let mut landed = None;
        for _ in 0..80 {
            let m = step(&mut local, &mut clock, Controls::standing(0.0)).unwrap();
            if !m.airborne {
                landed = Some(m);
                break;
            }
        }
        assert!(landed.is_some(), "the jump came down");
        let down = step(&mut local, &mut clock, Controls { crouch: true, ..Controls::standing(0.0) }).unwrap();
        assert!(down.crouched, "{down:?}");
    }

    #[test]
    fn the_movement_does_not_run_ahead_of_the_clock_when_the_game_runs_ticks_in_a_rush() {
        let mut local = Local::with_map(flat_floor_map());
        let mut clock = Clock::new();
        local.place([0.0, 0.0, 0.0]);
        // a second in real time
        for _ in 0..30 {
            step(&mut local, &mut clock, forward(0.0));
        }
        let before = step(&mut local, &mut clock, forward(0.0)).unwrap();
        // and then 100 ticks in no time at all
        let mut taken = 0;
        let mut last = before;
        for _ in 0..100 {
            let m = local.step_at(forward(0.0), clock.now()).unwrap();
            taken += m.stepped as u32;
            last = m;
        }
        assert!(taken <= 3, "{taken} ticks of 100 taken in the time of none");
        // the ones not taken are the last tick's state, and nothing new to report
        assert!(!last.stepped);
        assert!(last.position[0] - before.position[0] < 0.3);
    }

    #[test]
    fn a_hitch_is_caught_up_with_the_ticks_that_it_took() {
        let mut local = Local::with_map(flat_floor_map());
        let mut clock = Clock::new();
        local.place([0.0, 0.0, 0.0]);
        for _ in 0..30 {
            step(&mut local, &mut clock, forward(0.0));
        }
        // a hitch of 6 ticks' time, then the game runs the 6 ticks it missed at once
        clock.ticks += 6;
        let mut taken = 0;
        for _ in 0..6 {
            taken += local.step_at(forward(0.0), clock.now()).unwrap().stepped as u32;
        }
        assert_eq!(taken, 6, "the ticks the hitch took are taken");
    }

    #[test]
    fn a_game_that_was_paused_is_not_owed_its_time() {
        let mut local = Local::with_map(flat_floor_map());
        let mut clock = Clock::new();
        local.place([0.0, 0.0, 0.0]);
        for _ in 0..30 {
            step(&mut local, &mut clock, forward(0.0));
        }
        clock.ticks += 3000; // a hundred seconds in a menu
        let mut taken = 0;
        for _ in 0..200 {
            taken += local.step_at(forward(0.0), clock.now()).unwrap().stepped as u32;
        }
        assert!(taken <= 20, "{taken} ticks owed for a pause");
    }

    #[test]
    fn placing_the_player_again_starts_them_over_at_rest() {
        let mut local = Local::with_map(flat_floor_map());
        let mut clock = Clock::new();
        local.place([0.0, 0.0, 0.0]);
        for _ in 0..60 {
            step(&mut local, &mut clock, forward(0.0));
        }
        local.place([10.0, 5.0, 0.0]);
        let moved = step(&mut local, &mut clock, Controls::standing(0.0)).unwrap();
        assert_eq!((moved.position[0], moved.position[1]), (10.0, 5.0));
        assert_eq!(moved.velocity[0], 0.0);
    }

    #[test]
    fn a_map_file_that_is_not_there_is_an_error_not_a_hang() {
        let local = Local::load("/nonexistent/maps/nothing.map".into());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while local.error().is_none() {
            assert!(std::time::Instant::now() < deadline, "no answer from the loading thread");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!local.ready());
        assert!(local.error().unwrap().contains("nothing.map"));
    }
}
