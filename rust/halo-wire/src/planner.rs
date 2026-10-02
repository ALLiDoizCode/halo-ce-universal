//! Which other players one recipient is sent this tick: the rule.
//!
//! Every tick each other player builds up *priority* for the recipient by how
//! much they matter to it, and the recipient has a *credit* of bytes that
//! grows by its budget per tick. The planner sends the highest priorities
//! that the credit pays for, and a player who was sent starts again from zero.
//!
//! A player's weight each tick:
//!
//! - 1 within `near_radius` world units (10),
//! - falling with the square of the distance beyond that
//!   (`(near_radius / distance)^2`), down to `far_floor` (0.06),
//! - doubled when within `facing_degrees` (60) of where the recipient faces.
//!
//! Players within `near_radius` also rank above everyone else, so with room
//! in the budget (and the prototype needed 90 KB/s for Blood Gulch at 500
//! players) they are sent every tick; among themselves, and among everyone
//! else, the highest accumulated priority goes first, so nobody starves.
//!
//! The budget counts bytes on the wire: a datagram's payload plus
//! [`IP_UDP_OVERHEAD`]. Every tick a recipient is sent at least one datagram,
//! empty if need be, so that it can tell no tick was lost; those few bytes
//! may overdraw the credit, which later ticks pay back.

use halo_sim::TICKS_PER_SECOND;

use crate::datagram::{
    append_state, begin_snapshot, IP_UDP_OVERHEAD, MAX_DATAGRAM, MAX_STATES_PER_SNAPSHOT, SNAPSHOT_HEADER,
};
use crate::unit::{PackedState, UNIT_STATE_SIZE};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlannerConfig {
    /// Bytes a second a recipient may be sent, headers included.
    pub budget_bytes_per_second: u32,
    /// Within this many world units a player has full weight.
    pub near_radius: f32,
    /// The least weight a player has, however far.
    pub far_floor: f32,
    /// Half the angle, in degrees, of the cone in front of the recipient
    /// inside which weight is boosted.
    pub facing_degrees: f32,
    pub facing_boost: f32,
}

impl PlannerConfig {
    pub fn with_budget(budget_bytes_per_second: u32) -> PlannerConfig {
        PlannerConfig {
            budget_bytes_per_second,
            near_radius: 10.0,
            far_floor: 0.06,
            facing_degrees: 60.0,
            facing_boost: 2.0,
        }
    }
}

/// One player in the world as a tick's snapshot holds them.
#[derive(Debug, Clone, Copy)]
pub struct Entry {
    pub position: [f32; 3],
    pub packed: PackedState,
}

impl Entry {
    pub fn player(&self) -> u16 {
        self.packed.player()
    }
}

/// The recipient: where they are and which way they look, as the server
/// holds them.
#[derive(Debug, Clone, Copy)]
pub struct Observer {
    pub player: u16,
    pub position: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
}

/// A recipient's priorities and credit. One per recipient, kept between ticks.
#[derive(Debug, Clone)]
pub struct Planner {
    config: PlannerConfig,
    per_tick: f32,
    credit: f32,
    /// Accumulated priority, indexed by player id.
    priority: Vec<f32>,
    scratch: Vec<(f32, u32)>,
}

/// Added to a near player's key so they outrank any accumulated priority.
const NEAR_RANK: f32 = 1.0e9;

impl Planner {
    pub fn new(config: PlannerConfig) -> Planner {
        let per_tick = config.budget_bytes_per_second as f32 / TICKS_PER_SECOND as f32;
        Planner { config, per_tick, credit: per_tick, priority: Vec::new(), scratch: Vec::new() }
    }

    /// The datagrams to send the observer for `tick`, given every player's
    /// entry (the observer's own among them is skipped). Call once per tick.
    /// `world` should be in a stable order, such as ascending player id, which
    /// breaks ties.
    pub fn plan(&mut self, me: &Observer, world: &[Entry], tick: u32) -> Plan {
        self.credit = (self.credit + self.per_tick).min(self.per_tick * 2.0);
        let cfg = self.config;
        let near2 = cfg.near_radius * cfg.near_radius;
        let cone = (cfg.facing_degrees.to_radians()).cos();
        let cone2 = cone * cone;
        let (sy, cy) = me.yaw.sin_cos();
        let (sp, cp) = me.pitch.sin_cos();
        let forward = [cy * cp, sy * cp, sp];

        let max_id = world.iter().map(Entry::player).max().map_or(0, |m| m as usize + 1);
        if self.priority.len() < max_id {
            self.priority.resize(max_id, 0.0);
        }
        self.scratch.clear();
        for (index, other) in world.iter().enumerate() {
            let id = other.player();
            if id == me.player {
                continue;
            }
            let d = [
                other.position[0] - me.position[0],
                other.position[1] - me.position[1],
                other.position[2] - me.position[2],
            ];
            let d2 = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
            let mut weight = if d2 <= near2 { 1.0 } else { (near2 / d2).max(cfg.far_floor) };
            let ahead = d[0] * forward[0] + d[1] * forward[1] + d[2] * forward[2];
            if d2 <= 0.0 || (ahead > 0.0 && ahead * ahead >= cone2 * d2) {
                weight *= cfg.facing_boost;
            }
            let p = &mut self.priority[id as usize];
            *p += weight;
            let key = if d2 <= near2 { *p + NEAR_RANK } else { *p };
            self.scratch.push((key, index as u32));
        }

        let take = affordable(self.credit, self.scratch.len());
        let by_priority = |a: &(f32, u32), b: &(f32, u32)| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1));
        if take > 0 && take < self.scratch.len() {
            self.scratch.select_nth_unstable_by(take - 1, by_priority);
        }
        self.scratch[..take].sort_unstable_by(by_priority);

        let mut datagrams = Vec::new();
        let mut buf = Vec::with_capacity(MAX_DATAGRAM);
        for chunk in self.scratch[..take].chunks(MAX_STATES_PER_SNAPSHOT) {
            begin_snapshot(&mut buf, tick);
            for &(_, index) in chunk {
                let entry = &world[index as usize];
                append_state(&mut buf, &entry.packed);
                self.priority[entry.player() as usize] = 0.0;
            }
            datagrams.push(buf.clone());
        }
        if datagrams.is_empty() {
            begin_snapshot(&mut buf, tick);
            datagrams.push(buf);
        }
        let spent: usize = datagrams.iter().map(|d| d.len() + IP_UDP_OVERHEAD).sum();
        self.credit = (self.credit - spent as f32).max(-self.per_tick * 2.0);
        Plan { datagrams, states: take }
    }

    /// Unspent credit, in bytes. Negative after a heartbeat the budget did not cover.
    pub fn credit(&self) -> f32 {
        self.credit
    }
}

/// What a tick's plan is.
#[derive(Debug, Clone)]
pub struct Plan {
    pub datagrams: Vec<Vec<u8>>,
    /// States in them.
    pub states: usize,
}

impl Plan {
    /// Bytes on the wire, headers included.
    pub fn wire_bytes(&self) -> usize {
        self.datagrams.iter().map(|d| d.len() + IP_UDP_OVERHEAD).sum()
    }
}

/// The most states, of `available`, that `credit` bytes pay for with their datagrams' headers.
fn affordable(credit: f32, available: usize) -> usize {
    let header = (SNAPSHOT_HEADER + IP_UDP_OVERHEAD) as f32;
    let cost = |k: usize| k as f32 * UNIT_STATE_SIZE as f32 + k.div_ceil(MAX_STATES_PER_SNAPSHOT) as f32 * header;
    let mut k = ((credit.max(0.0) / UNIT_STATE_SIZE as f32) as usize).min(available);
    while k > 0 && cost(k) > credit {
        k -= 1;
    }
    k
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datagram::{ServerMessage, MAX_DATAGRAM};
    use crate::unit::{Bounds, UnitState};

    const BOUNDS: Bounds = Bounds { min: [-100.0; 3], max: [100.0; 3] };

    fn entry(player: u16, position: [f32; 3]) -> Entry {
        let state = UnitState { player, position, velocity: [0.0; 3], yaw: 0.0, pitch: 0.0, tick: 0, flags: 0 };
        Entry { position, packed: PackedState::pack(&state, &BOUNDS) }
    }

    fn observer_at_origin() -> Observer {
        Observer { player: 0, position: [0.0; 3], yaw: 0.0, pitch: 0.0 }
    }

    /// Players ringed around the origin at the given radii, ids from 1.
    fn ring(radii: &[f32]) -> Vec<Entry> {
        let mut world = vec![entry(0, [0.0; 3])];
        for (i, r) in radii.iter().enumerate() {
            let angle = 2.0 + i as f32 * 2.399; // spread, and not straight ahead
            world.push(entry(i as u16 + 1, [r * angle.cos(), r * angle.sin(), 0.0]));
        }
        world
    }

    fn states_of(plan: &Plan) -> Vec<u16> {
        plan.datagrams
            .iter()
            .flat_map(|d| match ServerMessage::decode(d) {
                Some(ServerMessage::Snapshot(s)) => s.states,
                other => panic!("not a snapshot: {other:?}"),
            })
            .map(|s| s.player())
            .collect()
    }

    #[test]
    fn everyone_is_sent_every_tick_when_the_budget_allows_it() {
        let world = ring(&[5.0, 30.0, 90.0]);
        let mut planner = Planner::new(PlannerConfig::with_budget(90_000));
        for tick in 0..20 {
            let plan = planner.plan(&observer_at_origin(), &world, tick);
            let mut got = states_of(&plan);
            got.sort();
            assert_eq!(got, [1, 2, 3], "tick {tick}");
            assert_eq!(plan.datagrams.len(), 1);
        }
    }

    #[test]
    fn the_observer_is_never_sent_to_themselves() {
        let world = ring(&[5.0, 6.0]);
        let mut planner = Planner::new(PlannerConfig::with_budget(90_000));
        assert!(!states_of(&planner.plan(&observer_at_origin(), &world, 1)).contains(&0));
    }

    #[test]
    fn a_datagram_never_exceeds_the_limit_and_the_budget_holds_over_time() {
        let radii: Vec<f32> = (0..499).map(|i| 3.0 + (i as f32 * 0.37) % 90.0).collect();
        let world = ring(&radii);
        for budget in [10_000u32, 24_000, 90_000, 400_000] {
            let mut planner = Planner::new(PlannerConfig::with_budget(budget));
            let mut bytes = 0usize;
            let ticks = 300;
            for tick in 0..ticks {
                let plan = planner.plan(&observer_at_origin(), &world, tick);
                assert!(plan.datagrams.iter().all(|d| d.len() <= MAX_DATAGRAM));
                bytes += plan.wire_bytes();
            }
            let allowed = budget as f32 * ticks as f32 / 30.0 + 2.0 * budget as f32 / 30.0;
            assert!(bytes as f32 <= allowed, "budget {budget}: sent {bytes} bytes, allowed {allowed}");
            // and the budget is used, not left idle (unless everyone fits in it every tick)
            let everyone = 499.0 * 16.0 + 7.0 * 34.0;
            let used = (budget as f32 / 30.0).min(everyone) * ticks as f32;
            assert!(bytes as f32 >= 0.9 * used, "budget {budget}: only {bytes}");
        }
    }

    #[test]
    fn players_within_ten_units_are_sent_every_tick_at_90_kb_s_among_five_hundred() {
        // 40 near players among 459 scattered ones
        let mut radii: Vec<f32> = (0..40).map(|i| 1.0 + i as f32 * 0.2).collect();
        radii.extend((0..459).map(|i| 11.0 + (i as f32 * 0.53) % 100.0));
        let world = ring(&radii);
        let mut planner = Planner::new(PlannerConfig::with_budget(90_000));
        let mut sent = vec![0u32; world.len()];
        let ticks = 300;
        for tick in 0..ticks {
            for id in states_of(&planner.plan(&observer_at_origin(), &world, tick)) {
                sent[id as usize] += 1;
            }
        }
        for (id, n) in sent.iter().enumerate().take(41).skip(1) {
            assert_eq!(*n, ticks, "near player {id}");
        }
        // nobody starves: every far player is sent now and then
        for (id, n) in sent.iter().enumerate().skip(41) {
            assert!(*n >= 5, "player {id} was sent {n} times in {ticks} ticks");
        }
    }

    #[test]
    fn farther_players_are_sent_less_often() {
        let mut radii: Vec<f32> = (0..200).map(|i| 12.0 + (i as f32 * 0.31) % 20.0).collect();
        radii.extend((0..200).map(|i| 70.0 + (i as f32 * 0.17) % 30.0));
        let world = ring(&radii);
        let mut planner = Planner::new(PlannerConfig::with_budget(30_000));
        let (mut mid, mut far) = (0u32, 0u32);
        for tick in 0..300 {
            for id in states_of(&planner.plan(&observer_at_origin(), &world, tick)) {
                if id <= 200 {
                    mid += 1;
                } else {
                    far += 1;
                }
            }
        }
        assert!(mid > 2 * far, "mid-range got {mid} states, far got {far}");
    }

    #[test]
    fn players_in_front_of_the_recipient_are_sent_about_twice_as_often() {
        // equal distance: one cluster straight ahead (+x), one straight behind
        let mut world = vec![entry(0, [0.0; 3])];
        for i in 0..100u16 {
            let d = 40.0 + (i % 10) as f32;
            world.push(entry(1 + i, [d, (i / 10) as f32 - 5.0, 0.0]));
            world.push(entry(101 + i, [-d, (i / 10) as f32 - 5.0, 0.0]));
        }
        let mut planner = Planner::new(PlannerConfig::with_budget(12_000));
        let (mut ahead, mut behind) = (0u32, 0u32);
        for tick in 0..600 {
            for id in states_of(&planner.plan(&observer_at_origin(), &world, tick)) {
                if id <= 100 {
                    ahead += 1;
                } else {
                    behind += 1;
                }
            }
        }
        let ratio = ahead as f32 / behind as f32;
        assert!((1.6..=2.4).contains(&ratio), "ahead {ahead}, behind {behind}, ratio {ratio}");
    }

    #[test]
    fn a_tick_with_no_budget_still_sends_an_empty_snapshot_so_the_tick_is_seen() {
        let world = ring(&[5.0, 6.0]);
        let mut planner = Planner::new(PlannerConfig::with_budget(0));
        for tick in 0..5 {
            let plan = planner.plan(&observer_at_origin(), &world, tick);
            assert_eq!(plan.datagrams.len(), 1);
            assert_eq!(
                ServerMessage::decode(&plan.datagrams[0]),
                Some(ServerMessage::Snapshot(crate::Snapshot { tick, states: vec![] }))
            );
        }
    }

    #[test]
    fn many_states_split_into_datagrams_in_priority_order() {
        let radii: Vec<f32> = (0..150).map(|i| 1.0 + i as f32 * 0.05).collect();
        let world = ring(&radii);
        let mut planner = Planner::new(PlannerConfig::with_budget(400_000));
        let plan = planner.plan(&observer_at_origin(), &world, 3);
        assert_eq!(plan.states, 150);
        assert_eq!(plan.datagrams.len(), 3, "74 + 74 + 2");
        assert_eq!(states_of(&plan).len(), 150);
    }
}
