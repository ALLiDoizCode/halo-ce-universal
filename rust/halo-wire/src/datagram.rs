//! The datagrams. All multi-byte numbers are little-endian, and every
//! datagram starts with a one-byte kind. A datagram of an unknown kind, or
//! one that does not parse, is ignored by whoever receives it.
//!
//! Player to gateway:
//!
//! ```text
//! Hello  0x01 | player u16                                            (3 bytes)
//!     "I am this player; challenge me." Resend until a Challenge comes.
//!     (Until the join and leave ticket, #7, a Hello was answered with a
//!     Welcome at once and the gateway believed the claim. It no longer does:
//!     the answer is a Challenge, and the Welcome comes after an Auth.)
//! Auth   0x03 | player u16 | stamp u64 | cookie [16] | signature [64]   (91 bytes)
//!     The answer to a Challenge: its stamp and cookie echoed, and the
//!     Ed25519 signature, with the private key of the public key the player
//!     put on their seat, of `auth::auth_message(player, stamp, cookie)`.
//!     Accepted, it binds the sending address to the player and is answered
//!     with a Welcome; refused, with a Refused.
//! Input  0x02 | seq u32 | player u16 | x y z f32 | yaw f32 | pitch f32
//!             | ack_newest u16 | ack_bits u32                          (33 bytes)
//!     The player's position and facing now: the 22-byte record of
//!     `halo_sim::wire`, then the acknowledgement of Snapshots (see [`Ack`]).
//!     The gateway keeps the newest `seq` and drops any input whose `seq` is
//!     not newer than one it has (wrapping compare). Only counts from the
//!     address that authenticated as `player`.
//!     Changed by #7: the six `ack` bytes are new (an Input was 27 bytes).
//! ```
//!
//! Gateway to player:
//!
//! ```text
//! Challenge 0x83 | stamp u64 | cookie [16]                              (25 bytes)
//!     Answer to Hello. Stateless on the gateway's side; see [`crate::auth`].
//! Welcome   0x81 | player u16 | tick u32 | x0 x1 y0 y1 z0 z1 f32       (31 bytes)
//!     Answer to an accepted Auth: the tick the match is at and the map's
//!     world bounds, which the 16-byte states are quantised against. Resent
//!     for every repeat of the same Auth (the first may have been lost).
//! Refused   0x84 | player u16 | reason u8                               (4 bytes)
//!     Answer to a Hello or Auth that did not get through. `reason` is one of
//!     the `REFUSED_*` codes. Nothing else is sent to an address that is not
//!     bound to a player.
//! Snapshot  0x82 | tick u32 | seq u16 | count u8 | count x 16-byte unit state
//!     Some of the other players' states as of `tick`, at most
//!     MAX_STATES_PER_SNAPSHOT of them, so that no datagram is over
//!     MAX_DATAGRAM bytes. A tick can take several Snapshots. Every player
//!     is sent at least one Snapshot every tick (with `count` 0 when the
//!     budget allows no state), so a receiver knows no tick was lost.
//!     `seq` numbers the Snapshots sent to this player, from 1, wrapping
//!     past 0 (0 is never used): it is what the player acknowledges.
//!     Changed by #7: `seq` is new (the header was 6 bytes).
//! ```
//!
//! # How a player joins, leaves and comes back
//!
//! 1. On their SpacetimeDB connection to the match's database, the player
//!    calls the module's `join(udp_public_key)` and reads their `seat` row
//!    (their `player` id).
//! 2. Over UDP they send Hello and get a Challenge, answer it with an Auth and
//!    get a Welcome; Hello is resent every few hundred milliseconds until the
//!    Challenge comes and Auth until the Welcome does. The Challenge is good
//!    for 30 seconds.
//! 3. They send an Input every tick (it keeps the session alive; the gateway
//!    unbinds an address silent for the idle timeout) and receive Snapshots.
//!    Each Input carries what they have received, so the gateway can resend
//!    what was lost.
//! 4. To leave they call the module's `leave`: the player is removed from the
//!    match and from the player list, and the gateway stops sending.
//! 5. After a dropped connection the player's seat is held for a grace period.
//!    Back from the same SpacetimeDB identity, from any address, they call
//!    `join` again (the same player id, their place and a new key if they like)
//!    and authenticate again as in 2.

use halo_sim::wire::{decode_inputs, encode_inputs, INPUT_SIZE};
use halo_sim::PlayerInput;

use crate::auth::{COOKIE_SIZE, SIGNATURE_SIZE};
use crate::unit::{Bounds, PackedState, UNIT_STATE_SIZE};

/// No datagram is longer than this: under a 1,280-byte IPv6 minimum MTU with
/// room for headers, so it is never fragmented.
pub const MAX_DATAGRAM: usize = 1200;

/// Bytes a datagram adds on the wire beyond its payload: IPv4 and UDP headers.
/// Budgets are counted with it.
pub const IP_UDP_OVERHEAD: usize = 28;

pub const KIND_HELLO: u8 = 0x01;
pub const KIND_INPUT: u8 = 0x02;
pub const KIND_AUTH: u8 = 0x03;
pub const KIND_WELCOME: u8 = 0x81;
pub const KIND_SNAPSHOT: u8 = 0x82;
pub const KIND_CHALLENGE: u8 = 0x83;
pub const KIND_REFUSED: u8 = 0x84;

/// Refused: the match has no such player, or the player has not joined (no
/// seat, so no key to check a proof against).
pub const REFUSED_NO_SEAT: u8 = 1;
/// Refused: the signature does not check out, or the cookie is not one this
/// gateway made for this address.
pub const REFUSED_BAD_PROOF: u8 = 2;
/// Refused: the challenge is too old, or its proof was already accepted.
pub const REFUSED_STALE: u8 = 3;

/// Bytes before a Snapshot's states.
pub const SNAPSHOT_HEADER: usize = 1 + 4 + 2 + 1;

/// The most states one Snapshot holds.
pub const MAX_STATES_PER_SNAPSHOT: usize = (MAX_DATAGRAM - SNAPSHOT_HEADER) / UNIT_STATE_SIZE;

pub const ACK_SIZE: usize = 2 + 4;
pub const INPUT_DATAGRAM_SIZE: usize = 1 + 4 + INPUT_SIZE + ACK_SIZE;
pub const AUTH_DATAGRAM_SIZE: usize = 1 + 2 + 8 + COOKIE_SIZE + SIGNATURE_SIZE;
pub const WELCOME_SIZE: usize = 1 + 2 + 4 + 24;
pub const CHALLENGE_SIZE: usize = 1 + 8 + COOKIE_SIZE;
pub const REFUSED_SIZE: usize = 1 + 2 + 1;

/// Whether `seq` is newer than `than`, counting a u32 as wrapping.
pub fn seq_newer(seq: u32, than: u32) -> bool {
    seq != than && seq.wrapping_sub(than) < 0x8000_0000
}

/// Whether `seq` is newer than `than`, counting a u16 as wrapping.
pub fn seq16_newer(seq: u16, than: u16) -> bool {
    seq != than && seq.wrapping_sub(than) < 0x8000
}

/// The Snapshot number after `seq`: wraps past 0, which is never used.
pub fn next_snapshot_seq(seq: u16) -> u16 {
    match seq.wrapping_add(1) {
        0 => 1,
        next => next,
    }
}

/// Which Snapshots a player has received: the newest one's `seq`, and one bit
/// for each of the 32 before it (bit `i` is `newest - 1 - i`). Sent with every
/// Input, and cumulative, so that a lost Input costs the sender nothing the
/// next one does not repeat. `newest == 0` means none yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Ack {
    pub newest: u16,
    pub bits: u32,
}

impl Ack {
    pub const NONE: Ack = Ack { newest: 0, bits: 0 };

    /// Note that Snapshot `seq` arrived.
    pub fn record(&mut self, seq: u16) {
        if seq == 0 {
            return;
        }
        if self.newest == 0 {
            *self = Ack { newest: seq, bits: 0 };
        } else if seq16_newer(seq, self.newest) {
            let d = seq.wrapping_sub(self.newest) as u32;
            let shifted = if d <= 32 { (((self.bits as u64) << d) | (1u64 << (d - 1))) as u32 } else { 0 };
            *self = Ack { newest: seq, bits: shifted };
        } else if seq != self.newest {
            let d = self.newest.wrapping_sub(seq) as u32;
            if (1..=32).contains(&d) {
                self.bits |= 1 << (d - 1);
            }
        }
    }

    /// Whether Snapshot `seq` is known to have arrived.
    pub fn has(&self, seq: u16) -> bool {
        if self.newest == 0 || seq == 0 {
            return false;
        }
        if seq == self.newest {
            return true;
        }
        let d = self.newest.wrapping_sub(seq) as u32;
        (1..=32).contains(&d) && self.bits >> (d - 1) & 1 == 1
    }

    /// Everything either of two acknowledgements knows (for the receiver of
    /// Inputs that may arrive out of order).
    pub fn merge(self, other: Ack) -> Ack {
        if self.newest == 0 {
            return other;
        }
        if other.newest == 0 {
            return self;
        }
        let (old, mut new) = if seq16_newer(other.newest, self.newest) { (self, other) } else { (other, self) };
        new.record_older(old);
        new
    }

    fn record_older(&mut self, old: Ack) {
        let d = self.newest.wrapping_sub(old.newest) as u32;
        if d == 0 {
            self.bits |= old.bits;
        } else if d <= 32 {
            self.bits |= (((old.bits as u64) << d) | (1u64 << (d - 1))) as u32;
        }
    }

    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.newest.to_le_bytes());
        out.extend_from_slice(&self.bits.to_le_bytes());
    }

    fn decode(b: &[u8]) -> Ack {
        Ack { newest: u16::from_le_bytes([b[0], b[1]]), bits: u32::from_le_bytes([b[2], b[3], b[4], b[5]]) }
    }
}

/// What a player sends.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ClientMessage {
    Hello { player: u16 },
    Auth { player: u16, stamp: u64, cookie: [u8; COOKIE_SIZE], signature: [u8; SIGNATURE_SIZE] },
    Input { seq: u32, input: PlayerInput, ack: Ack },
}

impl ClientMessage {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            ClientMessage::Hello { player } => {
                let mut b = vec![KIND_HELLO];
                b.extend_from_slice(&player.to_le_bytes());
                b
            }
            ClientMessage::Auth { player, stamp, cookie, signature } => {
                let mut b = Vec::with_capacity(AUTH_DATAGRAM_SIZE);
                b.push(KIND_AUTH);
                b.extend_from_slice(&player.to_le_bytes());
                b.extend_from_slice(&stamp.to_le_bytes());
                b.extend_from_slice(cookie);
                b.extend_from_slice(signature);
                b
            }
            ClientMessage::Input { seq, input, ack } => {
                let mut b = Vec::with_capacity(INPUT_DATAGRAM_SIZE);
                b.push(KIND_INPUT);
                b.extend_from_slice(&seq.to_le_bytes());
                b.extend_from_slice(&encode_inputs(std::slice::from_ref(input)));
                ack.encode(&mut b);
                b
            }
        }
    }

    pub fn decode(datagram: &[u8]) -> Option<ClientMessage> {
        match *datagram.first()? {
            KIND_HELLO if datagram.len() == 3 => {
                Some(ClientMessage::Hello { player: u16::from_le_bytes([datagram[1], datagram[2]]) })
            }
            KIND_AUTH if datagram.len() == AUTH_DATAGRAM_SIZE => Some(ClientMessage::Auth {
                player: u16::from_le_bytes([datagram[1], datagram[2]]),
                stamp: u64::from_le_bytes(datagram[3..11].try_into().ok()?),
                cookie: datagram[11..11 + COOKIE_SIZE].try_into().ok()?,
                signature: datagram[11 + COOKIE_SIZE..].try_into().ok()?,
            }),
            KIND_INPUT if datagram.len() == INPUT_DATAGRAM_SIZE => {
                let seq = u32::from_le_bytes(datagram[1..5].try_into().ok()?);
                let input = decode_inputs(&datagram[5..5 + INPUT_SIZE]).ok()?.pop()?;
                Some(ClientMessage::Input { seq, input, ack: Ack::decode(&datagram[5 + INPUT_SIZE..]) })
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Welcome {
    pub player: u16,
    pub tick: u32,
    pub bounds: Bounds,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Challenge {
    pub stamp: u64,
    pub cookie: [u8; COOKIE_SIZE],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Refused {
    pub player: u16,
    pub reason: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub tick: u32,
    pub seq: u16,
    pub states: Vec<PackedState>,
}

/// What the gateway sends.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerMessage {
    Challenge(Challenge),
    Welcome(Welcome),
    Refused(Refused),
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

impl Challenge {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(CHALLENGE_SIZE);
        b.push(KIND_CHALLENGE);
        b.extend_from_slice(&self.stamp.to_le_bytes());
        b.extend_from_slice(&self.cookie);
        b
    }
}

impl Refused {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = vec![KIND_REFUSED];
        b.extend_from_slice(&self.player.to_le_bytes());
        b.push(self.reason);
        b
    }
}

/// Start a Snapshot datagram in `out` (cleared first), for `append_state` to fill.
pub fn begin_snapshot(out: &mut Vec<u8>, tick: u32, seq: u16) {
    out.clear();
    out.push(KIND_SNAPSHOT);
    out.extend_from_slice(&tick.to_le_bytes());
    out.extend_from_slice(&seq.to_le_bytes());
    out.push(0);
}

/// Add a state to a datagram started by `begin_snapshot`. The caller keeps to
/// `MAX_STATES_PER_SNAPSHOT`.
pub fn append_state(out: &mut Vec<u8>, state: &PackedState) {
    debug_assert!(out.len() + UNIT_STATE_SIZE <= MAX_DATAGRAM);
    out.extend_from_slice(&state.0);
    out[SNAPSHOT_HEADER - 1] += 1;
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
            KIND_CHALLENGE if datagram.len() == CHALLENGE_SIZE => Some(ServerMessage::Challenge(Challenge {
                stamp: u64::from_le_bytes(datagram[1..9].try_into().ok()?),
                cookie: datagram[9..].try_into().ok()?,
            })),
            KIND_REFUSED if datagram.len() == REFUSED_SIZE => Some(ServerMessage::Refused(Refused {
                player: u16::from_le_bytes([datagram[1], datagram[2]]),
                reason: datagram[3],
            })),
            KIND_SNAPSHOT if datagram.len() >= SNAPSHOT_HEADER => {
                let count = datagram[SNAPSHOT_HEADER - 1] as usize;
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
                    seq: u16::from_le_bytes([datagram[5], datagram[6]]),
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

    fn input() -> PlayerInput {
        PlayerInput { player: 9, position: [1.0, 2.0, 3.0], yaw: 0.5, pitch: -0.5 }
    }

    #[test]
    fn a_client_message_round_trips() {
        let ack = Ack { newest: 700, bits: 0xf0f0_0001 };
        let auth = ClientMessage::Auth { player: 4, stamp: 1 << 40, cookie: [3; 16], signature: [9; 64] };
        for m in
            [ClientMessage::Hello { player: 513 }, ClientMessage::Input { seq: 0xdead_beef, input: input(), ack }, auth]
        {
            assert_eq!(ClientMessage::decode(&m.encode()), Some(m));
        }
        assert_eq!(ClientMessage::Input { seq: 1, input: input(), ack }.encode().len(), 33);
        assert_eq!(auth.encode().len(), 91);
    }

    #[test]
    fn the_client_layouts_are_fixed() {
        assert_eq!(ClientMessage::Hello { player: 0x0102 }.encode(), [0x01, 0x02, 0x01]);
        let moved = PlayerInput { player: 0x0304, position: [0.0; 3], yaw: 0.0, pitch: 0.0 };
        let ack = Ack { newest: 0x0506, bits: 0x0708090a };
        let b = ClientMessage::Input { seq: 0x0a0b0c0d, input: moved, ack }.encode();
        assert_eq!(&b[..7], [0x02, 0x0d, 0x0c, 0x0b, 0x0a, 0x04, 0x03]);
        assert_eq!(&b[27..], [0x06, 0x05, 0x0a, 0x09, 0x08, 0x07], "the acknowledgement is last");
        let auth = ClientMessage::Auth {
            player: 0x0102,
            stamp: 0x0807060504030201,
            cookie: [0xc0; 16],
            signature: [0x51; 64],
        };
        let a = auth.encode();
        assert_eq!(&a[..11], [0x03, 0x02, 0x01, 1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!((a[11], a[26], a[27], a[90]), (0xc0, 0xc0, 0x51, 0x51));
    }

    #[test]
    fn malformed_datagrams_decode_to_nothing() {
        assert_eq!(ClientMessage::decode(&[]), None);
        assert_eq!(ClientMessage::decode(&[0x01, 0x00]), None);
        assert_eq!(ClientMessage::decode(&[0x02; 32]), None);
        assert_eq!(ClientMessage::decode(&[0x02; 27]), None, "an Input without its acknowledgement");
        assert_eq!(ClientMessage::decode(&[0x03; 90]), None);
        assert_eq!(ClientMessage::decode(&[0x7f, 0, 0]), None);
        assert_eq!(ServerMessage::decode(&[0x82, 0, 0, 0, 0, 1, 0, 2]), None, "count says 2 states, there are none");
        assert_eq!(ServerMessage::decode(&[0x82, 0, 0]), None);
        assert_eq!(ServerMessage::decode(&[0x81; 30]), None);
        assert_eq!(ServerMessage::decode(&[0x83; 24]), None);
        assert_eq!(ServerMessage::decode(&[0x84; 3]), None);
    }

    #[test]
    fn a_welcome_round_trips() {
        let w = Welcome { player: 4, tick: 1234, bounds: Bounds::from_world([-1.0, 2.0, -3.0, 4.0, -5.0, 6.0]) };
        assert_eq!(w.encode().len(), WELCOME_SIZE);
        assert_eq!(ServerMessage::decode(&w.encode()), Some(ServerMessage::Welcome(w)));
    }

    #[test]
    fn a_challenge_and_a_refusal_round_trip() {
        let c = Challenge { stamp: 99, cookie: [5; 16] };
        assert_eq!(c.encode().len(), CHALLENGE_SIZE);
        assert_eq!(ServerMessage::decode(&c.encode()), Some(ServerMessage::Challenge(c)));
        let r = Refused { player: 300, reason: REFUSED_STALE };
        assert_eq!(r.encode(), [0x84, 0x2c, 0x01, 3]);
        assert_eq!(ServerMessage::decode(&r.encode()), Some(ServerMessage::Refused(r)));
    }

    #[test]
    fn a_full_snapshot_fits_in_one_datagram() {
        let mut buf = Vec::new();
        begin_snapshot(&mut buf, 77, 5);
        let state = PackedState([3; 16]);
        for _ in 0..MAX_STATES_PER_SNAPSHOT {
            append_state(&mut buf, &state);
        }
        assert!(buf.len() <= MAX_DATAGRAM);
        assert_eq!(MAX_STATES_PER_SNAPSHOT, 74);
        let Some(ServerMessage::Snapshot(s)) = ServerMessage::decode(&buf) else { panic!("did not decode") };
        assert_eq!((s.tick, s.seq, s.states.len()), (77, 5, 74));
        assert_eq!(s.states[73], state);
    }

    #[test]
    fn sequence_numbers_compare_across_the_wrap() {
        assert!(seq_newer(5, 4));
        assert!(!seq_newer(4, 5));
        assert!(!seq_newer(5, 5));
        assert!(seq_newer(2, u32::MAX - 2));
        assert!(!seq_newer(u32::MAX - 2, 2));
        assert!(seq16_newer(2, u16::MAX - 2));
        assert!(!seq16_newer(u16::MAX - 2, 2));
        assert_eq!(next_snapshot_seq(1), 2);
        assert_eq!(next_snapshot_seq(u16::MAX), 1, "0 is never a Snapshot's number");
    }

    #[test]
    fn an_ack_remembers_the_newest_snapshot_and_the_32_before_it() {
        let mut a = Ack::NONE;
        assert!(!a.has(1));
        for seq in [1, 2, 4, 3, 7] {
            a.record(seq);
        }
        assert_eq!(a.newest, 7);
        for seq in [1, 2, 3, 4, 7] {
            assert!(a.has(seq), "{seq}");
        }
        assert!(!a.has(5) && !a.has(6) && !a.has(8));
        a.record(5);
        assert!(a.has(5));
        // far ahead: what was behind falls out of the window
        a.record(7 + 33);
        assert!(a.has(40) && !a.has(7) && !a.has(8));
        a.record(41);
        assert!(a.has(40) && a.has(41));
        // a number older than the window is ignored
        a.record(2);
        assert!(!a.has(2));
    }

    #[test]
    fn an_ack_is_right_across_the_wrap() {
        let mut a = Ack::NONE;
        for seq in [u16::MAX - 1, u16::MAX, 1, 3] {
            a.record(seq);
        }
        assert_eq!(a.newest, 3);
        assert!(a.has(u16::MAX - 1) && a.has(u16::MAX) && a.has(1) && a.has(3));
        assert!(!a.has(2));
    }

    #[test]
    fn merged_acks_know_what_both_knew() {
        let mut a = Ack::NONE;
        let mut b = Ack::NONE;
        for seq in [1, 2, 4] {
            a.record(seq);
        }
        for seq in [2, 3, 6] {
            b.record(seq);
        }
        for m in [a.merge(b), b.merge(a)] {
            assert_eq!(m.newest, 6);
            for seq in [1, 2, 3, 4, 6] {
                assert!(m.has(seq), "{seq}");
            }
            assert!(!m.has(5));
        }
        assert_eq!(Ack::NONE.merge(a), a);
        assert_eq!(a.merge(Ack::NONE), a);
    }
}
