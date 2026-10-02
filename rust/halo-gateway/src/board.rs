//! What the gateway knows about its players between ticks: who is bound to
//! which address, each player's newest input, and what each has acknowledged.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use halo_sim::PlayerInput;
use halo_wire::datagram::{seq_newer, Ack};

/// Which address plays which player.
///
/// An address is bound to a player only by an accepted Auth (the proof is
/// checked by the gateway, which then calls [`Sessions::bind_proven`]); an
/// input counts only if it comes from the address its player is bound to. A
/// player has one address: a proof from a new one replaces the old.
#[derive(Debug, Default)]
pub struct Sessions {
    by_addr: HashMap<SocketAddr, u16>,
    by_player: HashMap<u16, Session>,
    /// The stamp of the newest challenge each player answered, kept after
    /// they unbind so that an answer is never accepted twice.
    last_stamp: HashMap<u16, u64>,
    generations: u32,
}

#[derive(Debug)]
struct Session {
    addr: SocketAddr,
    /// Changes whenever the player's session starts afresh (a new binding or
    /// a new address), so that what is kept per session can be dropped.
    generation: u32,
    /// Milliseconds on the gateway's clock when the address was last heard.
    last_heard: AtomicU64,
}

/// What a bind did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bound {
    /// A player bound for the first time.
    New,
    /// The same address, bound already.
    Again,
    /// The player's address changed; the old one is unbound.
    Moved,
}

/// A proof that the gateway already accepted, or an older one than it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Replayed;

/// Where to send a player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recipient {
    pub player: u16,
    pub addr: SocketAddr,
    pub generation: u32,
}

impl Sessions {
    /// Bind `addr` to `player` for a proof of a challenge made at `stamp`.
    /// Refused if the player has answered that challenge, or a later one,
    /// before, unless it is the answer to the latest one again from the bound
    /// address (a resend: the Welcome may have been lost).
    pub fn bind_proven(&mut self, addr: SocketAddr, player: u16, stamp: u64, now_ms: u64) -> Result<Bound, Replayed> {
        let last = self.last_stamp.get(&player).copied();
        match last {
            Some(last) if stamp < last => return Err(Replayed),
            Some(last) if stamp == last && self.addr_of(player) != Some(addr) => return Err(Replayed),
            _ => {}
        }
        self.last_stamp.insert(player, stamp);
        Ok(self.bind(addr, player, now_ms))
    }

    fn bind(&mut self, addr: SocketAddr, player: u16, now_ms: u64) -> Bound {
        // an address plays one player: a new claim from it replaces the old
        if let Some(old_player) = self.by_addr.insert(addr, player) {
            if old_player != player {
                self.by_player.remove(&old_player);
            }
        }
        let bound = match self.by_player.get(&player) {
            None => Bound::New,
            Some(old) if old.addr == addr => Bound::Again,
            Some(old) => {
                self.by_addr.remove(&old.addr);
                Bound::Moved
            }
        };
        match bound {
            Bound::Again => self.by_player[&player].last_heard.store(now_ms, Relaxed),
            Bound::New | Bound::Moved => {
                self.generations = self.generations.wrapping_add(1);
                self.by_player
                    .insert(player, Session { addr, generation: self.generations, last_heard: AtomicU64::new(now_ms) });
            }
        }
        bound
    }

    /// Stop playing the player: nothing more is sent, and no input counts,
    /// until a new proof binds an address again.
    pub fn unbind(&mut self, player: u16) -> bool {
        match self.by_player.remove(&player) {
            Some(session) => {
                self.by_addr.remove(&session.addr);
                true
            }
            None => false,
        }
    }

    /// Note that the player's address was heard from now.
    pub fn touch(&self, player: u16, now_ms: u64) {
        if let Some(session) = self.by_player.get(&player) {
            session.last_heard.store(now_ms, Relaxed);
        }
    }

    /// The players not heard from for `idle_ms`.
    pub fn idle(&self, now_ms: u64, idle_ms: u64) -> Vec<u16> {
        self.by_player
            .iter()
            .filter(|(_, s)| now_ms.saturating_sub(s.last_heard.load(Relaxed)) >= idle_ms)
            .map(|(id, _)| *id)
            .collect()
    }

    pub fn player_at(&self, addr: SocketAddr) -> Option<u16> {
        self.by_addr.get(&addr).copied()
    }

    pub fn addr_of(&self, player: u16) -> Option<SocketAddr> {
        self.by_player.get(&player).map(|s| s.addr)
    }

    /// The players with `id % modulus == remainder` and where to send them.
    pub fn partition(&self, remainder: usize, modulus: usize) -> Vec<Recipient> {
        self.by_player
            .iter()
            .filter(|(id, _)| **id as usize % modulus == remainder)
            .map(|(id, s)| Recipient { player: *id, addr: s.addr, generation: s.generation })
            .collect()
    }

    pub fn len(&self) -> usize {
        self.by_player.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_player.is_empty()
    }
}

#[derive(Debug, Clone, Copy)]
struct Slot {
    seq: u32,
    input: PlayerInput,
    /// Not yet handed to the module.
    fresh: bool,
}

/// Each player's newest input. An input whose sequence number is not newer
/// than the one held is late and dropped, so a reordered datagram can never
/// move a player backwards.
#[derive(Debug, Default)]
pub struct InputBoard {
    slots: HashMap<u16, Slot>,
}

impl InputBoard {
    /// Offer an input; `false` if it was late and dropped.
    pub fn offer(&mut self, seq: u32, input: PlayerInput) -> bool {
        match self.slots.get_mut(&input.player) {
            Some(slot) if !seq_newer(seq, slot.seq) => false,
            Some(slot) => {
                *slot = Slot { seq, input, fresh: true };
                true
            }
            None => {
                self.slots.insert(input.player, Slot { seq, input, fresh: true });
                true
            }
        }
    }

    /// Forget a player's sequence numbers, so that a fresh session may start counting again.
    pub fn forget(&mut self, player: u16) {
        self.slots.remove(&player);
    }

    /// The inputs that arrived since the last call, one per player (the
    /// newest), by ascending player id; they are not returned again.
    pub fn take_fresh(&mut self) -> Vec<PlayerInput> {
        let mut out: Vec<PlayerInput> = Vec::new();
        for slot in self.slots.values_mut().filter(|s| s.fresh) {
            slot.fresh = false;
            out.push(slot.input);
        }
        out.sort_unstable_by_key(|i| i.player);
        out
    }
}

/// What each player has said it received, for the sending threads to read.
#[derive(Debug, Default)]
pub struct AckBoard {
    acks: HashMap<u16, Ack>,
}

impl AckBoard {
    /// Add what an Input says; older or reordered ones add what they know.
    pub fn merge(&mut self, player: u16, ack: Ack) {
        let held = self.acks.entry(player).or_default();
        *held = held.merge(ack);
    }

    pub fn get(&self, player: u16) -> Ack {
        self.acks.get(&player).copied().unwrap_or(Ack::NONE)
    }

    /// Forget a player's acknowledgements: a fresh session numbers its Snapshots from 1.
    pub fn forget(&mut self, player: u16) {
        self.acks.remove(&player);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(player: u16, x: f32) -> PlayerInput {
        PlayerInput { player, position: [x, 0.0, 0.0], yaw: 0.0, pitch: 0.0 }
    }

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    #[test]
    fn the_newest_input_wins_and_a_late_one_is_dropped() {
        let mut board = InputBoard::default();
        assert!(board.offer(5, input(1, 5.0)));
        assert!(!board.offer(4, input(1, 4.0)), "late");
        assert!(!board.offer(5, input(1, 9.0)), "a repeat");
        assert!(board.offer(7, input(1, 7.0)));
        assert!(board.offer(6, input(2, 6.0)));
        let taken = board.take_fresh();
        assert_eq!(taken, [input(1, 7.0), input(2, 6.0)]);
    }

    #[test]
    fn an_input_is_handed_over_once() {
        let mut board = InputBoard::default();
        board.offer(1, input(1, 1.0));
        assert_eq!(board.take_fresh().len(), 1);
        assert!(board.take_fresh().is_empty());
        board.offer(2, input(1, 2.0));
        assert_eq!(board.take_fresh(), [input(1, 2.0)]);
    }

    #[test]
    fn sequence_numbers_may_wrap() {
        let mut board = InputBoard::default();
        assert!(board.offer(u32::MAX, input(1, 1.0)));
        assert!(board.offer(0, input(1, 2.0)));
        assert!(!board.offer(u32::MAX, input(1, 3.0)));
    }

    #[test]
    fn a_forgotten_player_may_count_from_zero_again() {
        let mut board = InputBoard::default();
        board.offer(900, input(1, 1.0));
        board.forget(1);
        assert!(board.offer(0, input(1, 2.0)));
    }

    #[test]
    fn a_proof_binds_an_address_to_a_player_and_a_proof_from_a_new_address_replaces_the_old() {
        let mut s = Sessions::default();
        assert_eq!(s.bind_proven(addr(1), 10, 100, 0), Ok(Bound::New));
        assert_eq!(s.bind_proven(addr(1), 10, 100, 0), Ok(Bound::Again), "a resend of the same proof");
        assert_eq!(s.player_at(addr(1)), Some(10));
        assert_eq!(s.bind_proven(addr(2), 10, 200, 0), Ok(Bound::Moved));
        assert_eq!(s.player_at(addr(1)), None, "the old address no longer plays");
        assert_eq!(s.player_at(addr(2)), Some(10));
        assert_eq!(s.addr_of(10), Some(addr(2)));
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn a_proof_is_good_once() {
        let mut s = Sessions::default();
        assert_eq!(s.bind_proven(addr(1), 10, 100, 0), Ok(Bound::New));
        assert_eq!(s.bind_proven(addr(2), 10, 200, 0), Ok(Bound::Moved));
        // the first proof, replayed from the address it was made for (spoofed): the player is not taken back
        assert_eq!(s.bind_proven(addr(1), 10, 100, 0), Err(Replayed));
        assert_eq!(s.addr_of(10), Some(addr(2)));
        // the same proof from another address is a replay too
        assert_eq!(s.bind_proven(addr(3), 10, 200, 0), Err(Replayed));
        // and it is still so once the player has unbound
        s.unbind(10);
        assert_eq!(s.bind_proven(addr(1), 10, 100, 0), Err(Replayed));
        assert_eq!(s.bind_proven(addr(2), 10, 200, 0), Err(Replayed), "an unbound player answers a new challenge");
        assert_eq!(s.bind_proven(addr(2), 10, 300, 0), Ok(Bound::New));
        assert_eq!(s.bind_proven(addr(2), 10, 400, 0), Ok(Bound::Again), "a later one from the same address is fine");
    }

    #[test]
    fn a_session_starts_afresh_when_bound_anew_or_moved_but_not_when_repeated() {
        let mut s = Sessions::default();
        s.bind_proven(addr(1), 10, 1, 0).unwrap();
        let generation = |s: &Sessions| s.partition(0, 1)[0].generation;
        let first = generation(&s);
        s.bind_proven(addr(1), 10, 2, 0).unwrap();
        assert_eq!(generation(&s), first);
        s.bind_proven(addr(2), 10, 3, 0).unwrap();
        assert_ne!(generation(&s), first);
    }

    #[test]
    fn an_address_plays_one_player_at_a_time() {
        let mut s = Sessions::default();
        s.bind_proven(addr(1), 10, 1, 0).unwrap();
        s.bind_proven(addr(1), 11, 1, 0).unwrap();
        assert_eq!(s.addr_of(10), None);
        assert_eq!(s.player_at(addr(1)), Some(11));
    }

    #[test]
    fn an_unbound_player_is_not_sent_to_and_nothing_counts_from_their_address() {
        let mut s = Sessions::default();
        s.bind_proven(addr(1), 10, 1, 0).unwrap();
        assert!(s.unbind(10));
        assert!(!s.unbind(10));
        assert_eq!((s.player_at(addr(1)), s.addr_of(10), s.len()), (None, None, 0));
        assert!(s.partition(0, 1).is_empty());
    }

    #[test]
    fn a_player_not_heard_from_is_idle() {
        let mut s = Sessions::default();
        s.bind_proven(addr(1), 10, 1, 1_000).unwrap();
        s.bind_proven(addr(2), 11, 1, 1_000).unwrap();
        s.touch(11, 9_000);
        assert_eq!(s.idle(10_000, 5_000), [10]);
        assert!(s.idle(10_000, 20_000).is_empty());
        s.touch(10, 9_500);
        assert!(s.idle(10_000, 5_000).is_empty());
    }

    #[test]
    fn partitions_cover_every_player_once() {
        let mut s = Sessions::default();
        for id in 0..10u16 {
            s.bind_proven(addr(100 + id), id, 1, 0).unwrap();
        }
        let mut all: Vec<u16> = (0..3).flat_map(|k| s.partition(k, 3)).map(|r| r.player).collect();
        all.sort();
        assert_eq!(all, (0..10).collect::<Vec<u16>>());
    }

    #[test]
    fn acknowledgements_accumulate_and_a_fresh_session_starts_empty() {
        let mut board = AckBoard::default();
        let mut a = Ack::NONE;
        a.record(1);
        a.record(2);
        let mut b = Ack::NONE;
        b.record(2);
        b.record(4);
        board.merge(3, a);
        board.merge(3, b);
        let got = board.get(3);
        assert!(got.has(1) && got.has(2) && got.has(4) && !got.has(3));
        assert_eq!(board.get(9), Ack::NONE);
        board.forget(3);
        assert_eq!(board.get(3), Ack::NONE);
    }
}
