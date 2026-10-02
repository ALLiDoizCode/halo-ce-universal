//! The gateway of the large-scale mode, and the wire harness that tests it.
//!
//! - [`Gateway`] holds one connection to the match module and serves players
//!   over a [`Transport`] (UDP: [`UdpTransport`]).
//! - [`harness`] is the other end: simulated UDP players that assert on what
//!   a player sends and receives, with loss and delay injected.
//!
//! The datagram formats, how a player joins and proves who they are, and the
//! priority rule are in `halo-wire`. The gateway ties each UDP address to a
//! SpacetimeDB identity through the match's `seat` table (see the module), and
//! must run as the identity the match accepts input from:
//!
//! ```text
//! HALO_GATEWAY_TOKEN=<the gateway identity's token> cargo run --release --bin halo-gateway -- \
//!     --spacetimedb http://127.0.0.1:3000 --database match --bind 0.0.0.0:7777 --budget 90000
//! ```

pub mod board;
pub mod gateway;
pub mod harness;
pub mod stats;
pub mod transport;

pub use gateway::{Gateway, GatewayConfig};
pub use stats::{Spread, Stats, StatsSnapshot};
pub use transport::{Transport, UdpTransport};
