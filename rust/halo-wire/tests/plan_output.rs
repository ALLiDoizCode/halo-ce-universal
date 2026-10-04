//! The planner's output is pinned: the same crowd, recipients and acknowledgements give the same
//! datagrams, byte for byte, whatever way the planner is written. The hashes were taken from the
//! planner as it was before its ranking was made cheaper (#44); a change to the priority rule
//! (#45) changes them on purpose and says so.

use halo_wire::{Ack, Bounds, Entry, Observer, PackedState, Planner, PlannerConfig, UnitState};

const BOUNDS: Bounds = Bounds { min: [-200.0; 3], max: [200.0; 3] };

/// A small deterministic generator (xorshift), so that the crowd is the same everywhere.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
}

fn fnv(hash: &mut u64, bytes: &[u8]) {
    for b in bytes {
        *hash = (*hash ^ *b as u64).wrapping_mul(0x100_0000_01b3);
    }
}

/// `players` walkers (a few ids missing), `ticks` ticks, a player leaving now and then, and three
/// recipients that lose a share of the datagrams they are sent and acknowledge the rest.
fn run(players: u16, budget: u32, loss: f32, ticks: u32, seed: u64) -> u64 {
    let mut rng = Rng(seed);
    let ids: Vec<u16> = (0..players).filter(|i| i % 11 != 7).collect();
    let mut pos: Vec<[f32; 3]> =
        ids.iter().map(|_| [rng.unit() * 80.0 - 40.0, rng.unit() * 80.0 - 40.0, 0.0]).collect();
    let recipients = [ids[0], ids[ids.len() / 2], ids[ids.len() - 1]];
    let mut planners: Vec<Planner> =
        recipients.iter().map(|_| Planner::new(PlannerConfig::with_budget(budget))).collect();
    let mut acks = [Ack::NONE; 3];
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for tick in 0..ticks {
        for p in pos.iter_mut() {
            p[0] = (p[0] + rng.unit() * 2.0 - 1.0).clamp(-190.0, 190.0);
            p[1] = (p[1] + rng.unit() * 2.0 - 1.0).clamp(-190.0, 190.0);
        }
        let gone = if tick % 97 == 50 { Some((tick as usize / 97) % ids.len()) } else { None };
        let mut world = Vec::new();
        let mut at = Vec::new();
        for (i, id) in ids.iter().enumerate() {
            if Some(i) == gone {
                continue;
            }
            let state = UnitState {
                player: *id,
                position: pos[i],
                velocity: [0.0; 3],
                yaw: 0.0,
                pitch: 0.0,
                tick: tick as u8,
                flags: 0,
            };
            world.push(Entry { position: pos[i], packed: PackedState::pack(&state, &BOUNDS) });
            at.push(i);
        }
        for (r, planner) in planners.iter_mut().enumerate() {
            let Some(me) = at.iter().position(|i| ids[*i] == recipients[r]) else { continue };
            let observer = Observer {
                player: recipients[r],
                position: pos[at[me]],
                yaw: tick as f32 * 0.05 + r as f32,
                pitch: (tick as f32 * 0.01).sin() * 0.5,
            };
            planner.acknowledge(acks[r]);
            let plan = planner.plan(&observer, &world, tick);
            fnv(&mut hash, &(plan.states as u32).to_le_bytes());
            for d in &plan.datagrams {
                fnv(&mut hash, d);
                // the snapshot's number follows its kind and tick
                let seq = u16::from_le_bytes([d[5], d[6]]);
                if rng.unit() >= loss {
                    acks[r].record(seq);
                }
            }
        }
    }
    hash
}

#[test]
fn the_plan_is_byte_for_byte_what_it_was() {
    let got = [
        run(60, 90_000, 0.0, 300, 1),
        run(500, 90_000, 0.0, 300, 2),
        run(500, 90_000, 0.05, 400, 3),
        run(500, 20_000, 0.02, 400, 4),
        run(300, 400_000, 0.1, 300, 5),
    ];
    println!("{got:#x?}");
    assert_eq!(got, PINNED);
}

const PINNED: [u64; 5] =
    [0x4f22090629f735bb, 0xd8c5a48e63a33d86, 0x16bf6abd079fed43, 0xb2803eed1075894b, 0xb8fb17536ed43868];
