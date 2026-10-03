//! A long scripted scenario for the simulation step, used to check that the
//! native and WebAssembly builds give identical results.
//!
//! [`run`] plays `ticks` ticks of a seeded scenario on a flat floor: a crowd of
//! players, each tick reporting a mix of valid moves and every kind of invalid
//! one (too fast, through the floor, in the air, not a number, an
//! unknown player). It returns a byte string: the final state, then a hash that
//! chains the state and events of every tick, then how many events of each kind
//! there were. Any difference between two builds, on any tick, changes the
//! bytes.
//!
//! Beside the crowd, four players walk (`halo_sim::walk`) on a floor with a
//! wall and on a floor with a ramp, each tick on seeded controls, turning,
//! running, strafing and pressing into the wall and up the ramp: every tick's
//! bodies are chained into the hash and their last ones are in the result
//! (between the crowd's state and the hash).
//!
//! The scenario uses only integer work and IEEE-754 `f32` arithmetic, like the
//! simulation itself. The `.wasm` build exports [`parity_run`] and
//! [`parity_output`] for a host to call; the test in `tests/` does so.

use halo_sim::fixtures::{flat_floor_map, ramp_map, walled_floor_map, FLOOR_HALF_SIZE, RAMP_START_X, WALL_X};
use halo_sim::rules::{begin, enter, leave, snapshot_game, Death, GameEvent, GameStore, MemoryGame, Rules, Winner};
use halo_sim::walk::{walk, Body, Controls};
use halo_sim::{snapshot, step, Event, MemoryStore, Player, PlayerInput, RejectReason, Rng, Store};

pub const PLAYERS: u16 = 24;

/// Accepted, then each [`RejectReason`] in declaration order.
const EVENT_KINDS: usize = 7;

fn max_step() -> f32 {
    flat_floor_map().max_move_speed() / halo_sim::TICKS_PER_SECOND as f32
}

struct Fnv(u64);

impl Fnv {
    fn bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ b as u64).wrapping_mul(0x0000_0100_0000_01B3);
        }
    }
}

fn event_code(event: &Event) -> (u16, u8) {
    match *event {
        Event::MoveAccepted { player } => (player, 0),
        Event::MoveRejected { player, reason } => (
            player,
            match reason {
                RejectReason::UnknownPlayer => 1,
                RejectReason::NotFinite => 2,
                RejectReason::TooFast => 3,
                RejectReason::ThroughSurface => 4,
                RejectReason::OffGround => 5,
                RejectReason::DuplicateInput => 6,
            },
        ),
    }
}

fn signed(rng: &mut Rng) -> f32 {
    rng.next_f32() - 0.5
}

/// A body as bytes: where it is, how it moves, what it stands on.
fn body_bytes(body: &Body) -> Vec<u8> {
    let mut out = Vec::new();
    for v in body.position.iter().chain(&body.velocity).chain(&body.ground_plane.n) {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&body.ground_plane.d.to_le_bytes());
    out.extend_from_slice(&body.landing_velocity.to_le_bytes());
    out.extend_from_slice(&body.support_surface.to_le_bytes());
    out.push(body.airborne as u8);
    out.push(body.crouching as u8);
    out.extend_from_slice(&body.crouch.to_le_bytes());
    out.extend_from_slice(&body.pill_crouch.to_le_bytes());
    out.push(body.jump_timer);
    out.extend_from_slice(&[body.landing as u8, body.landing_counter as u8, body.landing_time as u8]);
    out
}

/// Play the scenario; see the crate docs for the layout of the result.
pub fn run(seed: u64, ticks: u32) -> Vec<u8> {
    let map = flat_floor_map();
    let mut store = MemoryStore::new();
    let mut rng = Rng::seeded(seed);
    for id in 0..PLAYERS {
        let (x, y) = (signed(&mut rng) * FLOOR_HALF_SIZE, signed(&mut rng) * FLOOR_HALF_SIZE);
        // (every fourth player is put well above the floor, in the air, where each of their moves is refused)
        let z = if id % 4 == 0 { 2.0 } else { 0.0 };
        store.set_player(Player::new(id, [x, y, z], 0.0, 0.0));
    }

    // the walkers: two on each of the maps with a wall and a ramp, from a random
    // generator of their own so that the crowd's stays as it was
    let mut walker_rng = Rng::seeded(seed ^ 0x57A1_C0DE);
    let maps = [walled_floor_map(), walled_floor_map(), ramp_map(0.36), ramp_map(0.36)];
    let starts = [
        [WALL_X - 6.0, 0.0, 0.0],
        [WALL_X - 3.0, 4.0, 0.0],
        [RAMP_START_X - 6.0, 0.0, 0.0],
        [RAMP_START_X + 12.0, 3.0, 4.3],
    ];
    let mut walkers: Vec<(Body, Controls)> = starts.iter().map(|p| (Body::at(*p), Controls::standing(0.0))).collect();

    let mut chain = Fnv(0xCBF2_9CE4_8422_2325);
    let mut counts = [0u32; EVENT_KINDS];
    for tick in 0..ticks {
        let mut inputs = Vec::with_capacity(PLAYERS as usize + 1);
        for id in 0..PLAYERS {
            let p = store.player(id).unwrap().position;
            let kind = rng.next_u32() % 100;
            let mut to = p;
            let walk = |rng: &mut Rng, to: &mut [f32; 3]| {
                to[0] += signed(rng) * max_step() * 1.2;
                to[1] += signed(rng) * max_step() * 1.2;
            };
            match kind {
                0..=77 => walk(&mut rng, &mut to),
                78..=84 => to[0] += max_step() * (1.5 + rng.next_f32()),
                85..=89 => {
                    walk(&mut rng, &mut to);
                    to[2] = -0.1 * rng.next_f32();
                }
                90..=94 => {
                    walk(&mut rng, &mut to);
                    to[2] = 0.6 + rng.next_f32();
                }
                _ => to[1] = f32::NAN,
            }
            // (a third of them say they are crouched)
            let flags = if rng.next_u32().is_multiple_of(3) { halo_sim::FLAG_CROUCHED } else { 0 };
            inputs.push(PlayerInput {
                player: id,
                position: to,
                yaw: signed(&mut rng),
                pitch: signed(&mut rng),
                flags,
            });
        }
        if rng.next_u32().is_multiple_of(40) {
            // a second input for a player who has one already
            inputs.push(PlayerInput { player: 0, position: [0.0; 3], yaw: 0.0, pitch: 0.0, flags: 0 });
        }
        if rng.next_u32().is_multiple_of(50) {
            inputs.push(PlayerInput { player: PLAYERS + 5, position: [0.0; 3], yaw: 0.0, pitch: 0.0, flags: 0 });
        }

        let events = step(&mut store, &inputs, &map, &mut rng);

        for event in &events {
            let (player, code) = event_code(event);
            counts[code as usize] += 1;
            chain.bytes(&player.to_le_bytes());
            chain.bytes(&[code]);
        }
        chain.bytes(&snapshot(&store));

        // (a new heading and throttle every second or so, held in between)
        for (i, (body, controls)) in walkers.iter_mut().enumerate() {
            if tick % 40 == (i as u32 * 9) % 40 {
                controls.yaw = signed(&mut walker_rng) * 2.0 * core::f32::consts::PI;
                controls.forward = (walker_rng.next_f32() * 1.4 - 0.2).min(1.0);
                controls.strafe = signed(&mut walker_rng) * 2.0;
            }
            // (and a jump now and then, and the crouch held for stretches)
            controls.jump = tick % 40 == (i as u32 * 9 + 5) % 40 && walker_rng.next_u32().is_multiple_of(2);
            controls.crouch = (tick / 60 + i as u32).is_multiple_of(3);
            walk(&maps[i], body, controls);
            chain.bytes(&body_bytes(body));
        }
    }

    let mut out = snapshot(&store);
    for (body, _) in &walkers {
        out.extend_from_slice(&body_bytes(body));
    }
    out.extend_from_slice(&chain.0.to_le_bytes());
    for c in counts {
        out.extend_from_slice(&c.to_le_bytes());
    }
    out
}

/// Read back what [`run`] returned: how many events there were of each kind,
/// accepted first, then each [`RejectReason`] in declaration order.
pub fn event_counts(output: &[u8]) -> [u32; EVENT_KINDS] {
    let tail = &output[output.len() - EVENT_KINDS * 4..];
    std::array::from_fn(|i| u32::from_le_bytes(tail[i * 4..i * 4 + 4].try_into().unwrap()))
}

static OUTPUT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// For the WebAssembly host: run the scenario and return the length of the
/// result, which [`parity_output`] points to.
#[no_mangle]
pub extern "C" fn parity_run(seed_low: u32, seed_high: u32, ticks: u32) -> u32 {
    let bytes = run(seed_low as u64 | (seed_high as u64) << 32, ticks).into_boxed_slice();
    let len = bytes.len() as u32;
    // leaked on purpose: the host reads it, once per run
    OUTPUT.store(Box::leak(bytes).as_ptr() as usize, std::sync::atomic::Ordering::SeqCst);
    len
}

#[no_mangle]
pub extern "C" fn parity_output() -> *const u8 {
    OUTPUT.load(std::sync::atomic::Ordering::SeqCst) as *const u8
}

// ---- the game's rules

/// Kinds of [`GameEvent`] counted by [`run_match`], in the order of
/// [`match_event_counts`]: died, scored, death refused, spawned at a start,
/// spawned in a wave, told to wait, over.
pub const MATCH_EVENT_KINDS: usize = 7;

fn match_event_bytes(event: &GameEvent, out: &mut Vec<u8>) -> usize {
    let (kind, fields): (u8, Vec<u64>) = match *event {
        GameEvent::Died { victim, killer, kind, respawn_at } => {
            (0, vec![victim as u64, killer.map_or(u64::MAX, |k| k as u64), kind as u64, respawn_at])
        }
        GameEvent::Scored { player, delta, score } => {
            (1, vec![player as u64, delta as i64 as u64, score as i64 as u64])
        }
        GameEvent::DeathRefused { victim, reason } => (2, vec![victim as u64, reason as u64]),
        GameEvent::Spawned { player, position, yaw, wave } => {
            let mut f = vec![player as u64, wave as u64, yaw.to_bits() as u64];
            f.extend(position.iter().map(|v| v.to_bits() as u64));
            (if wave { 4 } else { 3 }, f)
        }
        GameEvent::Waiting { player, wave_at } => (5, vec![player as u64, wave_at]),
        GameEvent::Over(ending) => {
            let (winner, who) = match ending.winner {
                Winner::Nobody => (0, 0),
                Winner::Player(p) => (1, p as u64),
                Winner::Team(t) => (2, t as u64),
            };
            (6, vec![ending.reason as u64, winner, who])
        }
    };
    out.push(kind);
    for f in fields {
        out.extend_from_slice(&f.to_le_bytes());
    }
    kind as usize
}

/// A match of the game's rules on a flat floor with six starting locations: 40
/// players who spawn, wait for waves, kill one another, betray, fall, leave
/// and join, in a game with teams or without (by the seed's lowest bit), with
/// a score limit that ends it, restarted every 300 ticks while over. Returns
/// the final state of the step's store and the game, then the hash that
/// chains every tick's events and states, then the counts of each kind of
/// event.
pub fn run_match(seed: u64, ticks: u32) -> Vec<u8> {
    let starts: Vec<_> = (0..6)
        .map(|i| {
            halo_sim::fixtures::start_at((i % 3) as f32 * 14.0 - 14.0, (i / 3) as f32 * 14.0 - 7.0, (i % 2) as i16)
        })
        .collect();
    let map = halo_sim::fixtures::with_starts(flat_floor_map(), &starts);
    let mut rules = if seed & 1 == 0 { Rules::slayer() } else { Rules::team_slayer() };
    rules.score_limit = 8;
    rules.respawn_ticks = 30;
    rules.respawn_growth_ticks = if seed & 2 == 0 { 0 } else { 20 };
    rules.wave_ticks = 45;
    rules.time_limit_ticks = 4_000;
    let mut store = MemoryStore::new();
    let mut game = MemoryGame::new(rules);
    let mut rng = Rng::seeded(seed);
    for id in 0..40u16 {
        enter(&mut game, id, (id % 2) as u8, 0);
    }
    let mut next_id = 40u16;

    let mut chain = Fnv(0xCBF2_9CE4_8422_2325);
    let mut counts = [0u32; MATCH_EVENT_KINDS];
    let mut bytes = Vec::new();
    for tick in 1..=ticks as u64 {
        let alive: Vec<u16> = game.contestants().iter().filter(|c| c.is_alive()).map(|c| c.id).collect();
        let all: Vec<u16> = game.contestants().iter().map(|c| c.id).collect();
        let mut deaths = Vec::new();
        if rng.next_u32() % 100 < 6 && !all.is_empty() {
            // (a player who may be dead already, or not in the match: refused)
            let victim =
                if rng.next_u32().is_multiple_of(10) { next_id + 9 } else { all[rng.next_u32() as usize % all.len()] };
            let killer = match rng.next_u32() % 8 {
                0 => None,
                1 => Some(victim),
                _ => Some(all[rng.next_u32() as usize % all.len()]),
            };
            deaths.push(Death { victim, killer });
        }
        let inputs: Vec<PlayerInput> = alive
            .iter()
            .map(|&id| {
                let p = store.player(id).unwrap().position;
                let to = [p[0] + signed(&mut rng) * max_step(), p[1] + signed(&mut rng) * max_step(), p[2]];
                PlayerInput { player: id, position: to, yaw: signed(&mut rng), pitch: 0.0, flags: 0 }
            })
            // (and one of the dead, whose input is dropped)
            .chain(all.iter().filter(|id| !alive.contains(id)).take(1).map(|&id| PlayerInput {
                player: id,
                position: [0.0; 3],
                yaw: 0.0,
                pitch: 0.0,
                flags: 0,
            }))
            .collect();

        let outcome = halo_sim::rules::play(&mut store, &mut game, &map, &mut rng, tick, &deaths, &inputs);
        for event in &outcome.moves {
            let (player, code) = event_code(event);
            chain.bytes(&player.to_le_bytes());
            chain.bytes(&[code]);
        }
        bytes.clear();
        for event in &outcome.events {
            let kind = match_event_bytes(event, &mut bytes);
            counts[kind] += 1;
        }
        chain.bytes(&bytes);
        chain.bytes(&snapshot(&store));
        chain.bytes(&snapshot_game(&game));

        if tick % 500 == 0 && !all.is_empty() {
            // a player leaves and another joins
            let leaver = all[rng.next_u32() as usize % all.len()];
            leave(&mut game, leaver);
            store.remove_player(leaver);
            enter(&mut game, next_id, (next_id % 2) as u8, tick);
            next_id += 1;
        }
        if tick % 300 == 0 && game.game().ending.is_some() {
            begin(&mut game, tick);
        }
    }

    let mut out = snapshot(&store);
    out.extend_from_slice(&snapshot_game(&game));
    out.extend_from_slice(&chain.0.to_le_bytes());
    for c in counts {
        out.extend_from_slice(&c.to_le_bytes());
    }
    out
}

/// How many events of each kind [`run_match`] had (see [`MATCH_EVENT_KINDS`]).
pub fn match_event_counts(output: &[u8]) -> [u32; MATCH_EVENT_KINDS] {
    let tail = &output[output.len() - MATCH_EVENT_KINDS * 4..];
    std::array::from_fn(|i| u32::from_le_bytes(tail[i * 4..i * 4 + 4].try_into().unwrap()))
}

/// For the WebAssembly host: run [`run_match`] and return the length of the
/// result, which [`parity_output`] points to.
#[no_mangle]
pub extern "C" fn parity_match_run(seed_low: u32, seed_high: u32, ticks: u32) -> u32 {
    let bytes = run_match(seed_low as u64 | (seed_high as u64) << 32, ticks).into_boxed_slice();
    let len = bytes.len() as u32;
    OUTPUT.store(Box::leak(bytes).as_ptr() as usize, std::sync::atomic::Ordering::SeqCst);
    len
}
