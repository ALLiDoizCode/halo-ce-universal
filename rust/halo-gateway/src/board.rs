//! What the gateway knows about its players between ticks: who is bound to
//! which address, and each player's newest input.

use std::collections::HashMap;
use std::net::SocketAddr;

use halo_sim::PlayerInput;
use halo_wire::datagram::seq_newer;

/// Which address plays which player.
///
/// Today a Hello binds whatever address sent it, to whatever player it
/// names: the gateway does not check who is asking (a later ticket ties a
/// player to their SpacetimeDB identity). What is enforced is that an input
/// counts only if it comes from the address its player is bound to.
#[derive(Debug, Default)]
pub struct Sessions {
    by_addr: HashMap<SocketAddr, u16>,
    by_player: HashMap<u16, SocketAddr>,
}

/// What a Hello did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bound {
    /// A player bound for the first time.
    New,
    /// The same address saying hello again.
    Again,
    /// The player's address changed; the old one is unbound.
    Moved,
}

impl Sessions {
    pub fn bind(&mut self, addr: SocketAddr, player: u16) -> Bound {
        // an address plays one player: a new claim from it replaces the old
        if let Some(old_player) = self.by_addr.insert(addr, player) {
            if old_player != player {
                self.by_player.remove(&old_player);
            }
        }
        match self.by_player.insert(player, addr) {
            None => Bound::New,
            Some(old) if old == addr => Bound::Again,
            Some(old) => {
                self.by_addr.remove(&old);
                Bound::Moved
            }
        }
    }

    pub fn player_at(&self, addr: SocketAddr) -> Option<u16> {
        self.by_addr.get(&addr).copied()
    }

    pub fn addr_of(&self, player: u16) -> Option<SocketAddr> {
        self.by_player.get(&player).copied()
    }

    /// The players with `id % modulus == remainder` and where to send them.
    pub fn partition(&self, remainder: usize, modulus: usize) -> Vec<(u16, SocketAddr)> {
        self.by_player.iter().filter(|(id, _)| **id as usize % modulus == remainder).map(|(id, a)| (*id, *a)).collect()
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
    fn a_hello_binds_an_address_to_a_player_and_a_new_address_replaces_the_old() {
        let mut s = Sessions::default();
        assert_eq!(s.bind(addr(1), 10), Bound::New);
        assert_eq!(s.bind(addr(1), 10), Bound::Again);
        assert_eq!(s.player_at(addr(1)), Some(10));
        assert_eq!(s.bind(addr(2), 10), Bound::Moved);
        assert_eq!(s.player_at(addr(1)), None, "the old address no longer plays");
        assert_eq!(s.player_at(addr(2)), Some(10));
        assert_eq!(s.addr_of(10), Some(addr(2)));
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn an_address_plays_one_player_at_a_time() {
        let mut s = Sessions::default();
        s.bind(addr(1), 10);
        s.bind(addr(1), 11);
        assert_eq!(s.addr_of(10), None);
        assert_eq!(s.player_at(addr(1)), Some(11));
    }

    #[test]
    fn partitions_cover_every_player_once() {
        let mut s = Sessions::default();
        for id in 0..10u16 {
            s.bind(addr(100 + id), id);
        }
        let mut all: Vec<u16> = (0..3).flat_map(|k| s.partition(k, 3)).map(|(id, _)| id).collect();
        all.sort();
        assert_eq!(all, (0..10).collect::<Vec<u16>>());
    }
}
