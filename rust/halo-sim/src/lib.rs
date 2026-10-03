//! The simulation of the large-scale mode: one library, built as WebAssembly
//! inside the SpacetimeDB module and as a native library in the client, and
//! giving the same results in both.
//!
//! The whole interface is [`step`]: a [`Store`] holding the state, one tick's
//! [`PlayerInput`]s, the [`MapData`] and a seeded [`Rng`] go in; the state in
//! the store is advanced and the [`Event`]s of the tick come out.
//!
//! ```
//! use halo_sim::{step, Event, MemoryStore, Player, PlayerInput, Rng, Store};
//! let map = halo_sim::fixtures::flat_floor_map();
//! let mut store = MemoryStore::new();
//! store.set_player(Player::new(1, [0.0, 0.0, 0.0], 0.0, 0.0));
//! let input = PlayerInput { player: 1, position: [0.05, 0.0, 0.0], yaw: 0.1, pitch: 0.0, flags: 0 };
//! let events = step(&mut store, &[input], &map, &mut Rng::seeded(1));
//! assert_eq!(events, [Event::MoveAccepted { player: 1 }]);
//! ```
//!
//! # The game
//!
//! [`rules`] holds the game's rules for Slayer and Team Slayer: who is alive,
//! where players spawn ([`spawn`], the engine's choice of a starting location,
//! and waves when none is free), what a death is worth, and when the match
//! ends. The match module calls [`rules::play`], which applies the tick's
//! deaths, judges the moves of the players who are alive with [`step`], and
//! spawns the players who are due. A death is a [`rules::Death`]: the hit
//! validation produces them.
//!
//! # Fighting
//!
//! [`combat::resolve`] judges a tick's hit reports (the shooter's weapon, rate
//! and reach, the target's position as the server saw it) and deals the damage
//! of those that pass to the [`damage::Vitals`] of the target, as the engine's
//! shields and body do (`halo_map::combat` reads the weapons, damages and
//! shield tags from the map, the same on the server and the client).
//! [`weapon::Hands`] is a weapon's trigger, rounds, heat and reload, one tick
//! at a time.
//!
//! # Determinism
//!
//! The library reads no clock, does no I/O and is `no_std`: `core` has no
//! floating-point maths, so no platform maths library (libm, the C runtime's
//! `sqrt`, a browser's `Math`) can be called from here. Only IEEE-754 addition,
//! subtraction, multiplication, division and comparison of `f32` are used,
//! which are exact and the same on every target. Anything that would need more
//! (a square root, trigonometry) belongs in this crate's own pure-Rust code.

#![no_std]

extern crate alloc;

pub mod combat;
pub mod damage;
pub mod fixtures;
mod map;
pub mod math;
mod movement;
mod pill;
mod rng;
pub mod rules;
pub mod spawn;
mod state;
mod step;
pub mod walk;
pub mod weapon;
pub mod wire;

pub use map::MapData;
pub use movement::{GROUND_TOLERANCE, MAX_CATCH_UP_TICKS, PENETRATION_TOLERANCE, TICKS_PER_SECOND};
pub use rng::Rng;
pub use state::{snapshot, MemoryStore, Player, PlayerId, Store, FLAG_AIRBORNE, FLAG_CROUCHED};
pub use step::{step, Event, PlayerInput, RejectReason};
