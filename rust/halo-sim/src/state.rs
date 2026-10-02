use alloc::collections::BTreeMap;
use alloc::vec::Vec;

pub type PlayerId = u16;

/// A player as the simulation holds them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Player {
    pub id: PlayerId,
    pub position: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
}

impl Player {
    /// Bytes per player in [`snapshot`].
    pub const SNAPSHOT_SIZE: usize = 2 + 5 * 4;

    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.id.to_le_bytes());
        for v in self.position.iter().chain([&self.yaw, &self.pitch]) {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
}

/// Where the simulation keeps its state. The server implements it over
/// SpacetimeDB tables and the client over memory; the step only ever goes
/// through this interface, so both run the same code.
pub trait Store {
    fn player(&self, id: PlayerId) -> Option<Player>;
    /// Insert the player, or replace the one with the same id.
    fn set_player(&mut self, player: Player);
    /// Remove a player; `true` if they were there.
    fn remove_player(&mut self, id: PlayerId) -> bool;
    /// The ids of all players in ascending order.
    fn player_ids(&self) -> Vec<PlayerId>;
}

/// A store in memory. Iteration is in id order, so it is deterministic.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemoryStore {
    players: BTreeMap<PlayerId, Player>,
}

impl MemoryStore {
    pub fn new() -> MemoryStore {
        MemoryStore::default()
    }
}

impl Store for MemoryStore {
    fn player(&self, id: PlayerId) -> Option<Player> {
        self.players.get(&id).copied()
    }

    fn set_player(&mut self, player: Player) {
        self.players.insert(player.id, player);
    }

    fn remove_player(&mut self, id: PlayerId) -> bool {
        self.players.remove(&id).is_some()
    }

    fn player_ids(&self) -> Vec<PlayerId> {
        self.players.keys().copied().collect()
    }
}

/// The whole state of a store as bytes: every player in id order, fields
/// little-endian, floats as their IEEE-754 bits. Two stores are byte-for-byte
/// equal exactly when their snapshots are.
pub fn snapshot(store: &impl Store) -> Vec<u8> {
    let ids = store.player_ids();
    let mut out = Vec::with_capacity(ids.len() * Player::SNAPSHOT_SIZE);
    for id in ids {
        if let Some(p) = store.player(id) {
            p.write(&mut out);
        }
    }
    out
}
