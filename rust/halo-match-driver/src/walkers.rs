//! Simulated players that walk on a map, plus a local copy of the match that
//! the server's state can be compared with.
//!
//! Each walker moves as a player does, with the same `halo_sim::walk` that the
//! game's client runs for its own player (running ahead along a heading, held
//! off walls, up and down slopes), and reports where they got to; so the
//! server judges what a real client's moves are.
//!
//! The copy runs the same `halo_sim::step` on the same inputs, so after each
//! tick it must equal the server's tables exactly.

use halo_sim::walk::{settled, walk, Body, Controls};
use halo_sim::{step, Event, MapData, MemoryStore, Player, PlayerInput, Rng, Store};

/// The most a walker may be moved by one tick of walking: more than that is
/// not the ground they were walking on but an edge they went over or a wall
/// they were put through, which they turn away from.
const MAX_STRIDE: f32 = 0.25;

pub struct Walkers {
    pub map: MapData,
    /// What the match should look like after the inputs applied so far.
    pub mirror: MemoryStore,
    headings: Vec<f32>,
    /// Where each walker is as a walking player, with the speed they have.
    bodies: Vec<Body>,
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
        let mut bodies = Vec::new();
        let mut spawn = Vec::new();
        let mut ids = Vec::new();
        for id in 0..players {
            let anchor = anchors[id as usize % anchors.len()];
            let ring = (id as usize / anchors.len()) as f32;
            let angle = ring * 2.4;
            let (dx, dy) = (ring.sqrt() * 0.7 * angle.cos(), ring.sqrt() * 0.7 * angle.sin());
            // (a hair above the ground to start with: a player is placed, then settles)
            let start = match ground_below(&map, [anchor[0] + dx, anchor[1] + dy, anchor[2] + 1.0], 3.0) {
                Some(z) => [anchor[0] + dx, anchor[1] + dy, z],
                None => anchor,
            };
            let position = settled(&map, start);
            let player = Player { id, position, yaw: 0.0, pitch: 0.0 };
            mirror.set_player(player);
            bodies.push(Body::at(position));
            spawn.push(PlayerInput { player: id, position, yaw: 0.0, pitch: 0.0 });
            headings.push(rng.next_f32() * core::f32::consts::TAU);
            ids.push(id);
        }
        let rejects_seen = vec![0; ids.len()];
        (Walkers { map, mirror, headings, bodies, rejects_seen, rng, ids }, spawn)
    }

    /// This tick's moves: each walker runs ahead along its heading, and turns
    /// to a new one (standing where they are) when that would take them over
    /// an edge.
    pub fn next_inputs(&mut self) -> Vec<PlayerInput> {
        let mut inputs = Vec::with_capacity(self.ids.len());
        for (i, &id) in self.ids.iter().enumerate() {
            let Some(p) = self.mirror.player(id) else { continue };
            let mut body = self.bodies[i];
            walk(&self.map, &mut body, &Controls { forward: 1.0, strafe: 0.0, yaw: self.headings[i], pitch: 0.0 });
            let stride = ((body.position[0] - p.position[0]).powi(2) + (body.position[1] - p.position[1]).powi(2)
                + (body.position[2] - p.position[2]).powi(2))
            .sqrt();
            let position = if body.airborne || stride > MAX_STRIDE {
                // an edge: stand still and turn to a new random heading
                self.headings[i] = self.rng.next_f32() * core::f32::consts::TAU;
                self.bodies[i] = Body::at(p.position);
                p.position
            } else {
                self.bodies[i] = body;
                body.position
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
            let position = [row.x, row.y, row.z];
            self.mirror.set_player(Player { id: row.id, position, yaw: row.yaw, pitch: row.pitch });
            // (a walker the server has somewhere else than they thought is put there, at rest)
            if self.bodies[index].position != position {
                self.bodies[index] = Body::at(position);
            }
            if row.rejected_moves > self.rejects_seen[index] {
                self.rejects_seen[index] = row.rejected_moves;
                self.headings[index] = self.rng.next_f32() * core::f32::consts::TAU;
            }
        }
    }

    /// Apply a tick's inputs to the local copy, as the server will.
    /// A walker whose move was rejected turns to a new heading, and starts
    /// again from where they are, instead of pushing on.
    pub fn apply(&mut self, inputs: &[PlayerInput], tick: u64) -> Vec<Event> {
        let events = step(&mut self.mirror, inputs, &self.map, &mut Rng::seeded(tick));
        for event in &events {
            if let Event::MoveRejected { player, .. } = event {
                // ids are 0..players, so a player's id is its index
                if let Some(heading) = self.headings.get_mut(*player as usize) {
                    *heading = self.rng.next_f32() * core::f32::consts::TAU;
                    if let Some(p) = self.mirror.player(*player) {
                        self.bodies[*player as usize] = Body::at(p.position);
                    }
                }
            }
        }
        events
    }
}

/// The height standing on the ground below `from`.
fn ground_below(map: &MapData, from: [f32; 3], length: f32) -> Option<f32> {
    map.collision.ray_down(from, length).map(|hit| hit.z + 0.01)
}
