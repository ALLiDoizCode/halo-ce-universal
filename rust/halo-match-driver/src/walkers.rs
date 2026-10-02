//! Simulated players that walk on a map, plus a local copy of the match that
//! the server's state can be compared with.
//!
//! The copy runs the same `halo_sim::step` on the same inputs, so after each
//! tick it must equal the server's tables exactly.

use halo_sim::{step, Event, MapData, MemoryStore, Player, PlayerInput, Rng, Store};

/// Metres (world units) a walker covers each tick: under the server's speed
/// bound of `MAX_MOVE_SPEED` / 30 = 0.133.
pub const STEP: f32 = 0.1;

pub struct Walkers {
    pub map: MapData,
    /// What the match should look like after the inputs applied so far.
    pub mirror: MemoryStore,
    headings: Vec<f32>,
    /// Rejections each walker had when last synced with the server.
    rejects_seen: Vec<u64>,
    rng: Rng,
    ids: Vec<u16>,
}

impl Walkers {
    /// `players` walkers placed around `anchors` (points on the ground, such
    /// as a map's player starting locations): each player's feet are put on
    /// the ground at `anchor + a small offset`. Returns them with the inputs
    /// that `add_players` takes.
    pub fn new(map: MapData, anchors: &[[f32; 3]], players: u16, seed: u64) -> (Walkers, Vec<PlayerInput>) {
        let mut rng = Rng::seeded(seed);
        let mut mirror = MemoryStore::new();
        let mut headings = Vec::new();
        let mut spawn = Vec::new();
        let mut ids = Vec::new();
        for id in 0..players {
            let anchor = anchors[id as usize % anchors.len()];
            let ring = (id as usize / anchors.len()) as f32;
            let angle = ring * 2.4;
            let (dx, dy) = (ring.sqrt() * 0.7 * angle.cos(), ring.sqrt() * 0.7 * angle.sin());
            let (x, y) = (anchor[0] + dx, anchor[1] + dy);
            let z = ground_below(&map, [x, y, anchor[2] + 1.0], 3.0).unwrap_or(anchor[2]);
            let player = Player { id, position: [x, y, z], yaw: 0.0, pitch: 0.0 };
            mirror.set_player(player);
            spawn.push(PlayerInput { player: id, position: player.position, yaw: 0.0, pitch: 0.0 });
            headings.push(rng.next_f32() * core::f32::consts::TAU);
            ids.push(id);
        }
        let rejects_seen = vec![0; ids.len()];
        (Walkers { map, mirror, headings, rejects_seen, rng, ids }, spawn)
    }

    /// This tick's moves: each walker steps along its heading, staying on the
    /// ground, and turns around at an edge.
    pub fn next_inputs(&mut self) -> Vec<PlayerInput> {
        let mut inputs = Vec::with_capacity(self.ids.len());
        for (i, &id) in self.ids.iter().enumerate() {
            let Some(p) = self.mirror.player(id) else { continue };
            let h = self.headings[i];
            let (x, y) = (p.position[0] + STEP * h.cos(), p.position[1] + STEP * h.sin());
            let position = match ground_below(&self.map, [x, y, p.position[2] + 0.5], 2.0) {
                Some(z) => [x, y, z],
                None => {
                    // an edge: stand still and turn to a new random heading
                    self.headings[i] = self.rng.next_f32() * core::f32::consts::TAU;
                    p.position
                }
            };
            inputs.push(PlayerInput { player: id, position, yaw: self.headings[i], pitch: 0.0 });
        }
        inputs
    }

    /// Take the server's word for where everyone is: a player who plans from
    /// what the server holds is never ahead of it, however many of their
    /// inputs were lost or delayed on the way. A walker the server rejected
    /// since the last sync turns to a new heading. For walkers behind a lossy
    /// link, where the local copy of [`Walkers::apply`] would drift.
    pub fn sync_with_server<'a>(&mut self, rows: impl IntoIterator<Item = &'a crate::PlayerRow>) {
        for row in rows {
            // ids are 0..players, so a player's id is its index
            let index = row.id as usize;
            if index >= self.ids.len() {
                continue;
            }
            self.mirror.set_player(Player {
                id: row.id,
                position: [row.x, row.y, row.z],
                yaw: row.yaw,
                pitch: row.pitch,
            });
            if row.rejected_moves > self.rejects_seen[index] {
                self.rejects_seen[index] = row.rejected_moves;
                self.headings[index] = self.rng.next_f32() * core::f32::consts::TAU;
            }
        }
    }

    /// Apply a tick's inputs to the local copy, as the server will.
    /// A walker whose move was rejected (a wall, a steep slope) turns to a new
    /// heading instead of pushing on.
    pub fn apply(&mut self, inputs: &[PlayerInput], tick: u64) -> Vec<Event> {
        let events = step(&mut self.mirror, inputs, &self.map, &mut Rng::seeded(tick));
        for event in &events {
            if let Event::MoveRejected { player, .. } = event {
                // ids are 0..players, so a player's id is its index
                if let Some(heading) = self.headings.get_mut(*player as usize) {
                    *heading = self.rng.next_f32() * core::f32::consts::TAU;
                }
            }
        }
        events
    }
}

/// The height standing on the ground below `from` (feet a hair above it).
fn ground_below(map: &MapData, from: [f32; 3], length: f32) -> Option<f32> {
    map.collision.ray_down(from, length).map(|hit| hit.z + 0.01)
}
