use alloc::collections::BTreeMap;
use alloc::vec::Vec;

pub type PlayerId = u16;

/// [`Player::flags`]: the player is not on the ground (the server's judgement
/// of the moves they reported: a jump or a fall).
pub const FLAG_AIRBORNE: u8 = 1;
/// [`Player::flags`]: the player is crouched (what their client says).
pub const FLAG_CROUCHED: u8 = 2;
/// Where the shot counter sits in [`Player::flags`] (bits 2 to 4, see [`FLAG_SHOTS_MASK`]).
pub const FLAG_SHOTS_SHIFT: u8 = 2;
/// [`Player::flags`], bits 2 to 4: how many shots the player's weapon has fired, modulo 8 (what their
/// client says). It is a count and not "firing" so that a player who is seen only every few ticks (a far
/// one is sent at 6 to 8 updates a second) still shows every shot: whoever last saw count `a` and now sees
/// `b` knows that `(b - a) mod 8` shots were fired in between ([`shots_between`]).
pub const FLAG_SHOTS_MASK: u8 = 0b0001_1100;
/// [`Player::flags`]: the player's weapon is reloading (what their client says).
pub const FLAG_RELOADING: u8 = 32;
/// The bits of a player's flags that the player says and the server only passes on: the crouch, the shot
/// counter and the reload. (Whether the player is in the air is the server's to judge, and the two high
/// bits are free.)
pub const CLIENT_FLAGS: u8 = FLAG_CROUCHED | FLAG_SHOTS_MASK | FLAG_RELOADING;

/// The shot counter of a set of flags (0 to 7).
pub fn shot_counter(flags: u8) -> u8 {
    (flags & FLAG_SHOTS_MASK) >> FLAG_SHOTS_SHIFT
}

/// `flags` with the shot counter set to `counter` modulo 8.
pub fn with_shot_counter(flags: u8, counter: u8) -> u8 {
    (flags & !FLAG_SHOTS_MASK) | ((counter << FLAG_SHOTS_SHIFT) & FLAG_SHOTS_MASK)
}

/// How many shots were fired between a player's flags `before` and `after`: the counter's difference
/// modulo 8 (a counter that went round a whole turn between two updates, 8 shots, reads as none).
pub fn shots_between(before: u8, after: u8) -> u8 {
    shot_counter(after).wrapping_sub(shot_counter(before)) & 7
}

/// A player as the simulation holds them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Player {
    pub id: PlayerId,
    pub position: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
    /// [`FLAG_AIRBORNE`], [`FLAG_CROUCHED`], the shot counter and [`FLAG_RELOADING`]: how the others are to
    /// show the player.
    pub flags: u8,
    /// Ticks the player has been off the ground, as far as the accepted moves
    /// say (0 on the ground): what the airborne rule of the validation
    /// measures a move against (see [`crate::step`]).
    pub air_ticks: u32,
    /// Where the height of the player was when they left the ground.
    pub air_z: f32,
    /// Ticks since the player was last by a surface, and how high they were
    /// then (see the airborne rule); 0 if they are by one.
    pub free_ticks: u32,
    pub free_z: f32,
}

impl Player {
    /// Bytes per player in [`snapshot`].
    pub const SNAPSHOT_SIZE: usize = 2 + 5 * 4 + 1 + 4 * 4;

    /// A player standing on the ground at `position`.
    pub fn new(id: PlayerId, position: [f32; 3], yaw: f32, pitch: f32) -> Player {
        Player { id, position, yaw, pitch, flags: 0, air_ticks: 0, air_z: 0.0, free_ticks: 0, free_z: 0.0 }
    }

    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.id.to_le_bytes());
        for v in self.position.iter().chain([&self.yaw, &self.pitch]) {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.push(self.flags);
        out.extend_from_slice(&self.air_ticks.to_le_bytes());
        out.extend_from_slice(&self.air_z.to_le_bytes());
        out.extend_from_slice(&self.free_ticks.to_le_bytes());
        out.extend_from_slice(&self.free_z.to_le_bytes());
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
    /// Ticks since the player's last accepted move (at least 1; 1 for a
    /// player who moved last tick). A move may cover that many ticks' worth of
    /// the speed bound, up to [`crate::MAX_CATCH_UP_TICKS`], so that a player
    /// whose input was lost or skipped is not refused at the next one. A store
    /// that does not track time need not override it: the bound is then one
    /// tick's.
    fn ticks_since_move(&self, _id: PlayerId) -> u32 {
        1
    }
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
