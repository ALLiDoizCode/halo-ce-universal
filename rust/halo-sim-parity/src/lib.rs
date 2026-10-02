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
//! The scenario uses only integer work and IEEE-754 `f32` arithmetic, like the
//! simulation itself. The `.wasm` build exports [`parity_run`] and
//! [`parity_output`] for a host to call; the test in `tests/` does so.

use halo_sim::fixtures::{flat_floor_map, FLOOR_HALF_SIZE};
use halo_sim::{snapshot, step, Event, MemoryStore, Player, PlayerInput, RejectReason, Rng, Store, MAX_MOVE_SPEED};

pub const PLAYERS: u16 = 24;

const MAX_STEP: f32 = MAX_MOVE_SPEED / halo_sim::TICKS_PER_SECOND as f32;

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
            },
        ),
    }
}

fn signed(rng: &mut Rng) -> f32 {
    rng.next_f32() - 0.5
}

/// Play the scenario; see the crate docs for the layout of the result.
pub fn run(seed: u64, ticks: u32) -> Vec<u8> {
    let map = flat_floor_map();
    let mut store = MemoryStore::new();
    let mut rng = Rng::seeded(seed);
    for id in 0..PLAYERS {
        let (x, y) = (signed(&mut rng) * FLOOR_HALF_SIZE, signed(&mut rng) * FLOOR_HALF_SIZE);
        store.set_player(Player { id, position: [x, y, 0.0], yaw: 0.0, pitch: 0.0 });
    }

    let mut chain = Fnv(0xCBF2_9CE4_8422_2325);
    let mut counts = [0u32; 6];
    for _ in 0..ticks {
        let mut inputs = Vec::with_capacity(PLAYERS as usize + 1);
        for id in 0..PLAYERS {
            let p = store.player(id).unwrap().position;
            let kind = rng.next_u32() % 100;
            let mut to = p;
            let walk = |rng: &mut Rng, to: &mut [f32; 3]| {
                to[0] += signed(rng) * MAX_STEP * 1.2;
                to[1] += signed(rng) * MAX_STEP * 1.2;
            };
            match kind {
                0..=77 => walk(&mut rng, &mut to),
                78..=84 => to[0] += MAX_STEP * (1.5 + rng.next_f32()),
                85..=89 => {
                    walk(&mut rng, &mut to);
                    to[2] = -0.1 * rng.next_f32();
                }
                90..=94 => {
                    walk(&mut rng, &mut to);
                    to[2] = 0.1 + rng.next_f32();
                }
                _ => to[1] = f32::NAN,
            }
            inputs.push(PlayerInput { player: id, position: to, yaw: signed(&mut rng), pitch: signed(&mut rng) });
        }
        if rng.next_u32().is_multiple_of(50) {
            inputs.push(PlayerInput { player: PLAYERS + 5, position: [0.0; 3], yaw: 0.0, pitch: 0.0 });
        }

        let events = step(&mut store, &inputs, &map, &mut rng);

        for event in &events {
            let (player, code) = event_code(event);
            counts[code as usize] += 1;
            chain.bytes(&player.to_le_bytes());
            chain.bytes(&[code]);
        }
        chain.bytes(&snapshot(&store));
    }

    let mut out = snapshot(&store);
    out.extend_from_slice(&chain.0.to_le_bytes());
    for c in counts {
        out.extend_from_slice(&c.to_le_bytes());
    }
    out
}

/// Read back what [`run`] returned: how many events there were of each kind,
/// accepted first, then each [`RejectReason`] in declaration order.
pub fn event_counts(output: &[u8]) -> [u32; 6] {
    let tail = &output[output.len() - 24..];
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
