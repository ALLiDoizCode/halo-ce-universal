//! What goes over UDP between a player and the gateway in the large-scale
//! mode, and the rule that decides which other players a recipient is sent.
//!
//! - [`datagram`]: the four datagram kinds and their byte layouts.
//! - [`unit`]: a player's state packed into 16 bytes, relative to the map's
//!   world bounds.
//! - [`planner`]: each tick, turns every player's state into the datagrams one
//!   recipient is sent, highest priority first, within a byte budget.
//!
//! Hit reports are not here: a hit lost to UDP loss is a kill lost, so a client
//! sends them over its own reliable SpacetimeDB connection (the module's
//! `report_hits`, records of `halo_sim::wire::HIT_SIZE` bytes), not through the
//! gateway.
//!
//! The crate does no I/O and reads no clock. The gateway uses all of it; the
//! client library uses [`datagram`] and [`unit`] to read what it is sent.

pub mod auth;
pub mod datagram;
pub mod planner;
pub mod unit;

pub use datagram::{Ack, ClientMessage, ServerMessage, Snapshot, Welcome};
pub use planner::{Entry, Observer, Planner, PlannerConfig, STALENESS_BOUND_TICKS};
pub use unit::{Bounds, PackedState, UnitState};
