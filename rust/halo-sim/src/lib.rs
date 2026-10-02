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
//! store.set_player(Player { id: 1, position: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0 });
//! let input = PlayerInput { player: 1, position: [0.05, 0.0, 0.0], yaw: 0.1, pitch: 0.0 };
//! let events = step(&mut store, &[input], &map, &mut Rng::seeded(1));
//! assert_eq!(events, [Event::MoveAccepted { player: 1 }]);
//! ```
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

pub mod fixtures;
mod map;
pub mod math;
mod movement;
mod pill;
mod rng;
mod state;
mod step;
pub mod walk;
pub mod wire;

pub use map::MapData;
pub use movement::{GROUND_TOLERANCE, MAX_CATCH_UP_TICKS, PENETRATION_TOLERANCE, TICKS_PER_SECOND};
pub use rng::Rng;
pub use state::{snapshot, MemoryStore, Player, PlayerId, Store};
pub use step::{step, Event, PlayerInput, RejectReason};
