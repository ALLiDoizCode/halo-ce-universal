//! Reads the Xbox Halo CE multiplayer map files a player or operator already
//! owns and returns what the large-scale mode needs from them: the collision
//! BSP, the world bounds, player starting locations, netgame flags, netgame
//! equipment and vehicle placements.
//!
//! ```no_run
//! let map = halo_map::HaloMap::from_path("maps/bloodgulch.map")?;
//! let start = map.player_starts[0];
//! let above = [start.position[0], start.position[1], start.position[2] + 1.0];
//! let ground = map.collision.ray_down(above, 3.0);
//! # Ok::<(), halo_map::MapError>(())
//! ```
//!
//! A map file is a fixed 0x800-byte header followed by one zlib stream (on the
//! retail disc) or the same bytes already inflated; both load. Inside, tag
//! data holds absolute 32-bit pointers, which are resolved against the
//! engine's load addresses. No game data is shipped, embedded or committed.
//!
//! # Running the tests against real maps
//!
//! Tests that need the game's own map files read their directory from the
//! `HALO_MAP_DIR` environment variable, a folder holding `bloodgulch.map`,
//! `sidewinder.map` and so on (the 13 multiplayer maps from the Xbox disc's
//! `maps` folder). Without it they print a note and pass without testing,
//! so a machine without game data still has a green `cargo test`.
//!
//! The crate depends only on pure-Rust code, so it builds for the host, for
//! `i686-unknown-linux-gnu` and for `wasm32-unknown-unknown`, and its results
//! do not depend on the target.

mod codec;
pub mod collision;
mod error;
mod map;
mod movement;
mod reader;

pub use error::{MapError, Result};
pub use map::{flag_type, game_type, HaloMap, MapHeader, NetgameEquipment, NetgameFlag, PlayerStart, VehiclePlacement};
pub use movement::Movement;
