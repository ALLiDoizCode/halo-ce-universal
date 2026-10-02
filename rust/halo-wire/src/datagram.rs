//! The datagrams. All multi-byte numbers are little-endian, and every
//! datagram starts with a one-byte kind. A datagram of an unknown kind, or
//! one that does not parse, is ignored by whoever receives it.
//!
//! Player to gateway:
//!
//! ```text
//! Hello  0x01 | player u16
//!     Claim a player id and ask for the gateway's Welcome. Resend until it
//!     comes. (Proving the player is who they say is a later ticket's job:
//!     today the gateway believes the claim.)
//! Input  0x02 | seq u32 | player u16 | x y z f32 | yaw f32 | pitch f32     (27 bytes)
//!     The player's position and facing now: the 22-byte record of
//!     `halo_sim::wire`. The gateway keeps the newest `seq` and drops any
//!     input whose `seq` is not newer than one it has (wrapping compare).
//! ```
//!
//! Gateway to player:
//!
//! ```text
//! Welcome   0x81 | player u16 | tick u32 | x0 x1 y0 y1 z0 z1 f32      (31 bytes)
//!     Answer to Hello: the tick the match is at and the map's world bounds,
//!     which the 16-byte states are quantised against.
//! Snapshot  0x82 | tick u32 | count u8 | count x 16-byte unit state
//!     Some of the other players' states as of `tick`, at most
//!     MAX_STATES_PER_SNAPSHOT of them, so that no datagram is over
//!     MAX_DATAGRAM bytes. A tick can take several Snapshots. Every player
//!     is sent at least one Snapshot every tick (with `count` 0 when the
//!     budget allows no state), so a receiver knows no tick was lost.
//! ```

use halo_sim::wire::{decode_inputs, encode_inputs, INPUT_SIZE};
use halo_sim::PlayerInput;

use crate::unit::{Bounds, PackedState, UNIT_STATE_SIZE};

/// No datagram is longer than this: under a 1,280-byte IPv6 minimum MTU with
/// room for headers, so it is never fragmented.
pub const MAX_DATAGRAM: usize = 1200;

/// Bytes a datagram adds on the wire beyond its payload: IPv4 and UDP headers.
/// Budgets are counted with it.
pub const IP_UDP_OVERHEAD: usize = 28;

pub const KIND_HELLO: u8 = 0x01;
pub const KIND_INPUT: u8 = 0x02;
pub const KIND_WELCOME: u8 = 0x81;
pub const KIND_SNAPSHOT: u8 = 0x82;

/// Bytes before a Snapshot's states.
pub const SNAPSHOT_HEADER: usize = 1 + 4 + 1;

/// The most states one Snapshot holds.
pub const MAX_STATES_PER_SNAPSHOT: usize = (MAX_DATAGRAM - SNAPSHOT_HEADER) / UNIT_STATE_SIZE;

pub const INPUT_DATAGRAM_SIZE: usize = 1 + 4 + INPUT_SIZE;
pub const WELCOME_SIZE: usize = 1 + 2 + 4 + 24;

/// What a player sends.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ClientMessage {
    Hello { player: u16 },
    Input { seq: u32, input: PlayerInput },
}

impl ClientMessage {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            ClientMessage::Hello { player } => {
                let mut b = vec![KIND_HELLO];
                b.extend_from_slice(&player.to_le_bytes());
                b
            }
            ClientMessage::Input { seq, input } => {
                let mut b = Vec::with_capacity(INPUT_DATAGRAM_SIZE);
                b.push(KIND_INPUT);
                b.extend_from_slice(&seq.to_le_bytes());
                b.extend_from_slice(&encode_inputs(std::slice::from_ref(input)));
                b
            }
        }
    }

    pub fn decode(datagram: &[u8]) -> Option<ClientMessage> {
        match *datagram.first()? {
            KIND_HELLO if datagram.len() == 3 => {
                Some(ClientMessage::Hello { player: u16::from_le_bytes([datagram[1], datagram[2]]) })
            }
            KIND_INPUT if datagram.len() == INPUT_DATAGRAM_SIZE => {
                let seq = u32::from_le_bytes(datagram[1..5].try_into().ok()?);
                let input = decode_inputs(&datagram[5..]).ok()?.pop()?;
                Some(ClientMessage::Input { seq, input })
            }
            _ => None,
        }
    }
}

/// Whether `seq` is newer than `than`, counting a u32 as wrapping.
pub fn seq_newer(seq: u32, than: u32) -> bool {
    seq != than && seq.wrapping_sub(than) < 0x8000_0000
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Welcome {
    pub player: u16,
    pub tick: u32,
    pub bounds: Bounds,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub tick: u32,
    pub states: Vec<PackedState>,
}

/// What the gateway sends.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerMessage {
    Welcome(Welcome),
    Snapshot(Snapshot),
}

impl Welcome {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(WELCOME_SIZE);
        b.push(KIND_WELCOME);
        b.extend_from_slice(&self.player.to_le_bytes());
        b.extend_from_slice(&self.tick.to_le_bytes());
        for v in self.bounds.to_world() {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b
    }
}

/// Start a Snapshot datagram in `out` (cleared first), for `append_state` to fill.
pub fn begin_snapshot(out: &mut Vec<u8>, tick: u32) {
    out.clear();
    out.push(KIND_SNAPSHOT);
    out.extend_from_slice(&tick.to_le_bytes());
    out.push(0);
}

/// Add a state to a datagram started by `begin_snapshot`. The caller keeps to
/// `MAX_STATES_PER_SNAPSHOT`.
pub fn append_state(out: &mut Vec<u8>, state: &PackedState) {
    debug_assert!(out.len() + UNIT_STATE_SIZE <= MAX_DATAGRAM);
    out.extend_from_slice(&state.0);
    out[5] += 1;
}

impl ServerMessage {
    pub fn decode(datagram: &[u8]) -> Option<ServerMessage> {
        match *datagram.first()? {
            KIND_WELCOME if datagram.len() == WELCOME_SIZE => {
                let f = |i: usize| f32::from_le_bytes(datagram[7 + 4 * i..11 + 4 * i].try_into().unwrap());
                Some(ServerMessage::Welcome(Welcome {
                    player: u16::from_le_bytes([datagram[1], datagram[2]]),
                    tick: u32::from_le_bytes(datagram[3..7].try_into().ok()?),
                    bounds: Bounds::from_world([f(0), f(1), f(2), f(3), f(4), f(5)]),
                }))
            }
            KIND_SNAPSHOT if datagram.len() >= SNAPSHOT_HEADER => {
                let count = datagram[5] as usize;
                if datagram.len() != SNAPSHOT_HEADER + count * UNIT_STATE_SIZE {
                    return None;
                }
                let states = datagram[SNAPSHOT_HEADER..]
                    .as_chunks::<UNIT_STATE_SIZE>()
                    .0
                    .iter()
                    .map(|c| PackedState(*c))
                    .collect();
                Some(ServerMessage::Snapshot(Snapshot {
                    tick: u32::from_le_bytes(datagram[1..5].try_into().ok()?),
                    states,
                }))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_client_message_round_trips() {
        let input = PlayerInput { player: 9, position: [1.0, 2.0, 3.0], yaw: 0.5, pitch: -0.5 };
        for m in [ClientMessage::Hello { player: 513 }, ClientMessage::Input { seq: 0xdead_beef, input }] {
            assert_eq!(ClientMessage::decode(&m.encode()), Some(m));
        }
        assert_eq!(ClientMessage::Input { seq: 1, input }.encode().len(), 27);
    }

    #[test]
    fn the_client_layouts_are_fixed() {
        assert_eq!(ClientMessage::Hello { player: 0x0102 }.encode(), [0x01, 0x02, 0x01]);
        let input = PlayerInput { player: 0x0304, position: [0.0; 3], yaw: 0.0, pitch: 0.0 };
        let b = ClientMessage::Input { seq: 0x0a0b0c0d, input }.encode();
        assert_eq!(&b[..7], [0x02, 0x0d, 0x0c, 0x0b, 0x0a, 0x04, 0x03]);
    }

    #[test]
    fn malformed_datagrams_decode_to_nothing() {
        assert_eq!(ClientMessage::decode(&[]), None);
        assert_eq!(ClientMessage::decode(&[0x01, 0x00]), None);
        assert_eq!(ClientMessage::decode(&[0x02; 26]), None);
        assert_eq!(ClientMessage::decode(&[0x7f, 0, 0]), None);
        assert_eq!(ServerMessage::decode(&[0x82, 0, 0, 0, 0, 2, 0]), None, "count says 2 states, there are none");
        assert_eq!(ServerMessage::decode(&[0x82, 0, 0]), None);
        assert_eq!(ServerMessage::decode(&[0x81; 30]), None);
    }

    #[test]
    fn a_welcome_round_trips() {
        let w = Welcome { player: 4, tick: 1234, bounds: Bounds::from_world([-1.0, 2.0, -3.0, 4.0, -5.0, 6.0]) };
        assert_eq!(w.encode().len(), WELCOME_SIZE);
        assert_eq!(ServerMessage::decode(&w.encode()), Some(ServerMessage::Welcome(w)));
    }

    #[test]
    fn a_full_snapshot_fits_in_one_datagram() {
        let mut buf = Vec::new();
        begin_snapshot(&mut buf, 77);
        let state = PackedState([3; 16]);
        for _ in 0..MAX_STATES_PER_SNAPSHOT {
            append_state(&mut buf, &state);
        }
        assert!(buf.len() <= MAX_DATAGRAM);
        assert_eq!(MAX_STATES_PER_SNAPSHOT, 74);
        let Some(ServerMessage::Snapshot(s)) = ServerMessage::decode(&buf) else { panic!("did not decode") };
        assert_eq!((s.tick, s.states.len()), (77, 74));
        assert_eq!(s.states[73], state);
    }

    #[test]
    fn sequence_numbers_compare_across_the_wrap() {
        assert!(seq_newer(5, 4));
        assert!(!seq_newer(4, 5));
        assert!(!seq_newer(5, 5));
        assert!(seq_newer(2, u32::MAX - 2));
        assert!(!seq_newer(u32::MAX - 2, 2));
    }
}
