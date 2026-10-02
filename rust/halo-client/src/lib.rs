//! The large-scale mode's client library. It is built as a 32-bit static
//! library and linked into the C client (port/linux/game/large_mode.c is the
//! adapter that calls it), and as an ordinary Rust library for its tests.
//!
//! - [`session`]: one session, in Rust terms: UDP to the gateway, SpacetimeDB
//!   directly, each on its own thread.
//! - [`browser`]: the server list, from the root database, under the player's
//!   own identity ([`identity`] keeps its token between sessions);
//! - [`ffi`]: the same, as C functions that pass only floats, 32-bit integers
//!   and pointers.
//!
//! The SpacetimeDB client SDK is inside it, so the library is its own
//! workspace (rust/halo-client). It links into the game for `i686-unknown-linux-gnu`
//! (with OpenSSL built from source into the archive, see Cargo.toml) and
//! `i686-pc-windows-msvc` (with Schannel); `tools/rust_client.py` builds it.

pub mod browser;
pub mod ffi;
pub mod identity;
pub mod session;

pub use browser::{Browser, ServerEntry};
pub use identity::IdentityFile;
pub use session::{Config, Frame, Refusal, RefusalKind, RemoteUnit, Session, Slow};
