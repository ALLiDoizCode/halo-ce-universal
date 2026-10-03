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
use halo_sim::weapon::Hands;
use halo_sim::{MapData, FLAG_CROUCHED, FLAG_RELOADING};

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

/// What the weapon in the local player's hands did in one tick of [`Local::fire`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Armed {
    pub rounds_loaded: i16,
    pub rounds_total: i16,
    pub heat: f32,
    pub overheated: bool,
    /// The weapon is reloading.
    pub reloading: bool,
    /// The trigger fired this tick.
    pub fired: bool,
    /// A reload began this tick (an empty magazine's, or one the player asked for).
    pub reload_began: bool,
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
    /// The weapon in hand (its tag index in the map) and what it holds: the model of
    /// its ammunition, heat and reload that the HUD shows ([`Local::fire`]).
    hands: Option<(u16, Hands)>,
    /// How many shots the weapon has fired, which the others are told (the counter of
    /// [`halo_sim::FLAG_SHOTS_MASK`]); kept across lives so that it only ever counts up.
    shots: u8,
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
        Local { map, body: None, clock: None, taken: 0, hands: None, shots: 0 }
    }

    /// A player on a map that is already in hand (for tests).
    pub fn with_map(map: MapData) -> Local {
        let loaded: Loaded = Arc::new(OnceLock::new());
        let _ = loaded.set(Ok(map));
        Local { map: loaded, body: None, clock: None, taken: 0, hands: None, shots: 0 }
    }

    /// Why the map could not be read, if it could not.
    pub fn error(&self) -> Option<String> {
        self.map.get().and_then(|r| r.as_ref().err().cloned())
    }

    /// The map, once it is in: what the player's body is and the weapons' tags, for the fighting.
    pub fn map(&self) -> Option<&MapData> {
        self.map.get().and_then(|r| r.as_ref().ok())
    }

    /// The map is in and movement will be computed.
    pub fn ready(&self) -> bool {
        matches!(self.map.get(), Some(Ok(_)))
    }

    /// Put the player at `position`, at rest: where the server placed them, or
    /// where the engine has the unit until the map is in.
    pub fn place(&mut self, position: [f32; 3]) {
        self.body = Some(Body::at(position));
        // (a player placed is a player spawned: the weapon is a fresh one)
        self.hands = None;
    }

    /// One tick of the weapon in hand (`weapon`, its tag index in the map): whether the trigger is
    /// held and whether the player asked for a reload. The weapon's ammunition, heat and reload are
    /// [`halo_sim::weapon::Hands`], so that what the HUD shows is the model's, the same one the
    /// comparison harness holds to the engine's; each shot it fires counts on the counter the
    /// others see ([`Local::flags`]). `None` while the map is not in or has no such weapon.
    pub fn fire(&mut self, weapon: u16, trigger: bool, reload: bool) -> Option<Armed> {
        let map = self.map.get()?.as_ref().ok()?;
        let tag = map.combat.weapon(weapon)?;
        if self.hands.as_ref().map(|(w, _)| *w) != Some(weapon) {
            self.hands = Some((weapon, Hands::new(tag)));
        }
        let (_, hands) = self.hands.as_mut()?;
        // (the engine looks at the reload control before the trigger)
        let asked = reload && hands.request_reload(tag);
        let shot = hands.update(tag, trigger);
        if shot.fired {
            self.shots = (self.shots + 1) & 7;
        }
        Some(Armed {
            rounds_loaded: hands.rounds_loaded,
            rounds_total: hands.rounds_total,
            heat: hands.heat,
            overheated: hands.overheated,
            reloading: hands.reloading(),
            fired: shot.fired,
            reload_began: asked || shot.reloading,
        })
    }

    /// The flags the player reports with their position: the crouch, how many shots the weapon has
    /// fired and whether it is reloading (what the others show of them, `halo_sim::CLIENT_FLAGS`).
    pub fn flags(&self, crouched: bool) -> u8 {
        let reloading = self.hands.as_ref().is_some_and(|(_, h)| h.reloading());
        halo_sim::with_shot_counter(
            if crouched { FLAG_CROUCHED } else { 0 } | if reloading { FLAG_RELOADING } else { 0 },
            self.shots,
        )
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

    fn armed_local() -> Local {
        let mut local = Local::with_map(flat_floor_map());
        local.place([0.0, 0.0, 0.0]);
        local
    }

    #[test]
    fn a_held_trigger_fires_the_pistol_at_its_rate_and_each_shot_counts_for_the_others() {
        use halo_sim::fixtures::PISTOL;
        let mut local = armed_local();
        let mut shots = 0u32;
        let mut last = None;
        for _ in 0..60 {
            let armed = local.fire(PISTOL, true, false).unwrap();
            shots += armed.fired as u32;
            last = Some(armed);
            assert_eq!(halo_sim::shot_counter(local.flags(false)) as u32, shots % 8);
        }
        // (3.5 a second: a shot on the first tick and then every 9)
        assert_eq!(shots, 7);
        let armed = last.unwrap();
        assert_eq!((armed.rounds_loaded, armed.rounds_total), (12 - 7, 48));
        assert!(!armed.reloading && !armed.overheated);
    }

    #[test]
    fn an_empty_magazine_reloads_and_the_flags_say_so_until_it_is_done() {
        use halo_sim::fixtures::PISTOL;
        let mut local = armed_local();
        let mut began = 0;
        let mut flagged = false;
        let mut reloading_ticks = 0;
        for _ in 0..400 {
            let armed = local.fire(PISTOL, true, false).unwrap();
            began += armed.reload_began as u32;
            if armed.reloading {
                reloading_ticks += 1;
                flagged |= local.flags(false) & FLAG_RELOADING != 0;
            } else {
                assert_eq!(local.flags(false) & FLAG_RELOADING, 0);
            }
        }
        assert!(began >= 1 && flagged && reloading_ticks > 30, "{began} {flagged} {reloading_ticks}");
    }

    #[test]
    fn a_reload_the_player_asks_for_begins_and_is_told() {
        use halo_sim::fixtures::PISTOL;
        let mut local = armed_local();
        local.fire(PISTOL, true, false).unwrap();
        for _ in 0..30 {
            local.fire(PISTOL, false, false).unwrap();
        }
        let armed = local.fire(PISTOL, false, true).unwrap();
        assert!(armed.reload_began && armed.reloading);
        assert_ne!(local.flags(true) & FLAG_RELOADING, 0);
        assert_ne!(local.flags(true) & FLAG_CROUCHED, 0);
    }

    #[test]
    fn a_new_life_has_a_full_weapon_but_the_shot_counter_goes_on_counting() {
        use halo_sim::fixtures::PISTOL;
        let mut local = armed_local();
        for _ in 0..20 {
            local.fire(PISTOL, true, false).unwrap();
        }
        let counted = halo_sim::shot_counter(local.flags(false));
        assert!(counted > 0);
        local.place([5.0, 5.0, 0.0]);
        let armed = local.fire(PISTOL, false, false).unwrap();
        assert_eq!((armed.rounds_loaded, armed.rounds_total), (12, 48));
        assert_eq!(halo_sim::shot_counter(local.flags(false)), counted);
    }

    #[test]
    fn there_is_no_weapon_before_the_map_is_in_and_none_the_map_does_not_have() {
        let mut local = Local::load("/nonexistent/maps/nothing.map".into());
        assert_eq!(local.fire(1, true, false), None);
        let mut local = armed_local();
        assert_eq!(local.fire(60000, true, false), None);
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
