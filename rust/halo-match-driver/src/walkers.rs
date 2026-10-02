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

/// How far into a wall a walker's start may be.
const PLACED_CLEAR: f32 = 0.001;

/// How far ahead of the server an acrobat's body may be (the inputs on their way,
/// a few ticks of a fall at most) before the server's word is taken instead.
const ACROBAT_LAG: f32 = 1.5;

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
    /// Walkers that also jump now and then, crouch for stretches and walk off
    /// ledges instead of turning away from them ([`Walkers::set_acrobatics`]).
    acrobatics: bool,
    ticks: u64,
    /// Per walker, how hard they push ahead (1 unless [`Walkers::set_throttle`] says).
    throttles: Vec<f32>,
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
            // (a hair above the ground to start with: a player is placed, then settles;
            // and placed clear of the walls: a start a little inside one would push the
            // walker out of it by more than a tick's walking, which is not what is walked)
            let place = |ring: f32, angle: f32| {
                let (dx, dy) = (ring.sqrt() * 0.7 * angle.cos(), ring.sqrt() * 0.7 * angle.sin());
                let start = match ground_below(&map, [anchor[0] + dx, anchor[1] + dy, anchor[2] + 1.0], 3.0) {
                    Some(z) => [anchor[0] + dx, anchor[1] + dy, z],
                    None => anchor,
                };
                settled(&map, start)
            };
            let clear = |position: [f32; 3]| halo_sim::walk::footing(&map, position).penetration <= PLACED_CLEAR;
            let mut position = place(ring, ring * 2.4);
            for attempt in 1..=12 {
                if clear(position) {
                    break;
                }
                position = place(ring + attempt as f32, ring * 2.4 + attempt as f32 * 1.7);
            }
            let player = Player::new(id, position, 0.0, 0.0);
            mirror.set_player(player);
            bodies.push(Body::at(position));
            spawn.push(PlayerInput { player: id, position, yaw: 0.0, pitch: 0.0, flags: 0 });
            headings.push(rng.next_f32() * core::f32::consts::TAU);
            ids.push(id);
        }
        let rejects_seen = vec![0; ids.len()];
        (
            Walkers {
                map,
                mirror,
                headings,
                bodies,
                rejects_seen,
                rng,
                ids,
                acrobatics: false,
                ticks: 0,
                throttles: vec![1.0; players as usize],
            },
            spawn,
        )
    }

    /// Make the walkers acrobats (or not): each jumps every second or two (a
    /// little out of step with the others), crouches for a stretch of each
    /// few seconds, and walks off an edge it comes to, falling and landing,
    /// where a plain walker turns away from it. The server judges every one of
    /// their moves as it does a plain walker's.
    pub fn set_acrobatics(&mut self, acrobatics: bool) {
        self.acrobatics = acrobatics;
    }

    /// How hard a walker pushes ahead, 0 (standing where they are, jumping
    /// and crouching if they are acrobats) to 1, and which way they face.
    pub fn set_course(&mut self, player: u16, throttle: f32, heading: f32) {
        if let Some(i) = self.ids.iter().position(|&id| id == player) {
            self.throttles[i] = throttle;
            self.headings[i] = heading;
        }
    }

    /// This tick's moves: each walker runs ahead along its heading, and turns
    /// to a new one (standing where they are) when that would take them over
    /// an edge (or, for an acrobat, when they would be put through a wall).
    pub fn next_inputs(&mut self) -> Vec<PlayerInput> {
        self.next_inputs_after(1)
    }

    /// This tick's moves after `ticks` ticks have passed since the last (a
    /// walker whose controller was busy for a while, as a game is after a
    /// hitch): each walker walks the ticks it missed and reports where it is
    /// now, which the server judges against the time since their last move.
    pub fn next_inputs_after(&mut self, ticks: u32) -> Vec<PlayerInput> {
        for _ in 1..ticks.max(1) {
            self.step_bodies();
        }
        self.step_bodies()
    }

    /// One tick of every walker: the inputs they would report.
    fn step_bodies(&mut self) -> Vec<PlayerInput> {
        let mut inputs = Vec::with_capacity(self.ids.len());
        self.ticks += 1;
        for (i, &id) in self.ids.iter().enumerate() {
            let Some(p) = self.mirror.player(id) else { continue };
            let mut body = self.bodies[i];
            let phase = self.ticks + 17 * i as u64;
            let (jump, crouch) = if self.acrobatics { (phase % 50 < 2, (phase / 70) % 3 == 0) } else { (false, false) };
            walk(
                &self.map,
                &mut body,
                // (an acrobat at most at 85% throttle: the two inputs of a tick the gateway may
                // hand the server as one are two ticks of running downhill, which the bound
                // has room for at that, not at a full run)
                &Controls {
                    forward: self.throttles[i] * if self.acrobatics { 0.85 } else { 1.0 },
                    strafe: 0.0,
                    yaw: self.headings[i],
                    pitch: 0.0,
                    jump,
                    crouch,
                },
            );
            // (how far this tick took the walker, from where the last left them)
            let before = self.bodies[i].position;
            let flat = ((body.position[0] - before[0]).powi(2) + (body.position[1] - before[1]).powi(2)).sqrt();
            let stride = (flat.powi(2) + (body.position[2] - before[2]).powi(2)).sqrt();
            let off_the_ground = if self.acrobatics { flat > MAX_STRIDE } else { body.airborne || stride > MAX_STRIDE };
            let position = if off_the_ground {
                // an edge: stand still and turn to a new random heading
                self.headings[i] = self.rng.next_f32() * core::f32::consts::TAU;
                self.bodies[i] = Body::at(p.position);
                p.position
            } else {
                self.bodies[i] = body;
                body.position
            };
            let flags = if self.acrobatics && body.crouched() { halo_sim::FLAG_CROUCHED } else { 0 };
            inputs.push(PlayerInput { player: id, position, yaw: self.headings[i], pitch: 0.0, flags });
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
            self.mirror.set_player(Player::new(row.id, position, row.yaw, row.pitch));
            // (a walker the server has somewhere else than they thought is put there, at rest: for
            // an acrobat, whose jump and fall are in the speed the body carries, only when it is
            // somewhere else than the inputs still on their way account for)
            let off = |a: [f32; 3], b: [f32; 3]| (0..3).map(|i| (a[i] - b[i]).powi(2)).sum::<f32>().sqrt();
            let lost = if self.acrobatics { off(self.bodies[index].position, position) > ACROBAT_LAG } else { true };
            if lost && self.bodies[index].position != position {
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
