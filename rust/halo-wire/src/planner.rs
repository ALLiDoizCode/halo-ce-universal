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
//! Datagrams get lost, and a state that was sent and lost must not be treated
//! as sent. Every Snapshot carries a number, the recipient says which numbers
//! it has received ([`Ack`], in every Input), and the planner keeps what each
//! recent Snapshot held: a Snapshot that a newer one was acknowledged past
//! without it is *lost*, and its players are put back (their priority is
//! restored and they are marked *urgent*, ranked above everyone, so they are
//! resent in the next tick's datagrams). Independently, a player not sent to
//! this recipient for `max_stale_ticks` becomes urgent too, so that no
//! player's state stays unsent for longer than that while the budget can pay
//! for it (about `players x 16 bytes / max_stale_ticks` a tick). A new
//! recipient has been sent no one, so everyone is urgent to it at first and
//! it is brought up to date, nearest and facing first, within its budget.
//!
//! The budget counts bytes on the wire: a datagram's payload plus
//! [`IP_UDP_OVERHEAD`]. Every tick a recipient is sent at least one datagram,
//! empty if need be, so that it can tell no tick was lost; those few bytes
//! may overdraw the credit, which later ticks pay back.

use std::collections::VecDeque;

use halo_sim::TICKS_PER_SECOND;

use crate::datagram::{
    append_state, begin_snapshot, next_snapshot_seq, seq16_newer, Ack, IP_UDP_OVERHEAD, MAX_DATAGRAM,
    MAX_STATES_PER_SNAPSHOT, SNAPSHOT_HEADER,
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
    /// A player not sent to the recipient for this many ticks is sent before
    /// anyone else (when the budget can pay for it).
    pub max_stale_ticks: u32,
}

impl PlannerConfig {
    pub fn with_budget(budget_bytes_per_second: u32) -> PlannerConfig {
        PlannerConfig {
            budget_bytes_per_second,
            near_radius: 10.0,
            far_floor: 0.06,
            facing_degrees: 60.0,
            facing_boost: 2.0,
            max_stale_ticks: 15,
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
    /// For ranking.
    scratch: Vec<Ranked>,
    /// The number of the next Snapshot.
    next_seq: u16,
    /// The most recent Snapshots sent and not yet known to have arrived or
    /// been lost, oldest first.
    in_flight: VecDeque<Sent>,
    /// `tick + 1` of the last tick each player was sent in, by id; 0 if never.
    last_sent: Vec<u32>,
    /// Marked for sending first: lost, and not sent since.
    urgent: Vec<bool>,
    /// `tick + 1` of the last tick the player was in the world, by id.
    present: Vec<u32>,
}

/// Snapshots remembered for loss detection: the acknowledgement's window is 32.
const IN_FLIGHT_MAX: usize = 64;

fn remember(in_flight: &mut VecDeque<Sent>, sent: Sent) {
    if in_flight.len() == IN_FLIGHT_MAX {
        in_flight.pop_front();
    }
    in_flight.push_back(sent);
}

/// One Snapshot sent: what it held, to put back if it is lost.
#[derive(Debug, Clone)]
struct Sent {
    seq: u16,
    tick: u32,
    /// (player, the priority the player had when it was sent).
    states: Vec<(u16, f32)>,
}

#[derive(Debug, Clone, Copy)]
struct Ranked {
    urgent: bool,
    near: bool,
    priority: f32,
    /// Index into the world.
    index: u32,
}

impl Planner {
    pub fn new(config: PlannerConfig) -> Planner {
        let per_tick = config.budget_bytes_per_second as f32 / TICKS_PER_SECOND as f32;
        Planner {
            config,
            per_tick,
            credit: per_tick,
            priority: Vec::new(),
            scratch: Vec::new(),
            next_seq: 1,
            in_flight: VecDeque::new(),
            last_sent: Vec::new(),
            urgent: Vec::new(),
            present: Vec::new(),
        }
    }

    /// Take in what the recipient says it has received. Call before `plan`
    /// each tick with the newest [`Ack`] the recipient has sent (repeating
    /// one is harmless). A Snapshot that the acknowledgement has gone past
    /// without it is lost: the players in it are put back to be sent first.
    pub fn acknowledge(&mut self, ack: Ack) {
        if ack.newest == 0 {
            return;
        }
        let mut i = 0;
        while i < self.in_flight.len() {
            let seq = self.in_flight[i].seq;
            if ack.has(seq) {
                self.in_flight.remove(i);
            } else if seq16_newer(ack.newest, seq) {
                let lost = self.in_flight.remove(i).expect("in range");
                for (player, priority) in lost.states {
                    let p = player as usize;
                    // sent again since: that one's fate is what counts now
                    if self.last_sent.get(p) == Some(&(lost.tick + 1)) {
                        self.priority[p] += priority;
                        self.urgent[p] = true;
                    }
                }
            } else {
                i += 1;
            }
        }
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
            self.last_sent.resize(max_id, 0);
            self.urgent.resize(max_id, false);
            self.present.resize(max_id, 0);
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
            let slot = id as usize;
            self.present[slot] = tick + 1;
            let overdue = self.last_sent[slot] == 0 || tick + 1 - self.last_sent[slot] >= cfg.max_stale_ticks;
            let p = &mut self.priority[slot];
            *p += weight;
            self.scratch.push(Ranked {
                urgent: self.urgent[slot] || overdue,
                near: d2 <= near2,
                priority: *p,
                index: index as u32,
            });
        }

        // a player who left, and whoever takes their id, starts from nothing
        for id in 0..self.present.len() {
            if self.present[id] != 0 && self.present[id] != tick + 1 {
                self.present[id] = 0;
                (self.priority[id], self.last_sent[id], self.urgent[id]) = (0.0, 0, false);
            }
        }

        let take = affordable(self.credit, self.scratch.len());
        // urgent players first, then near ones, then by accumulated priority, then by world order
        let by_priority = |a: &Ranked, b: &Ranked| {
            b.urgent
                .cmp(&a.urgent)
                .then(b.near.cmp(&a.near))
                .then(b.priority.total_cmp(&a.priority))
                .then(a.index.cmp(&b.index))
        };
        if take > 0 && take < self.scratch.len() {
            self.scratch.select_nth_unstable_by(take - 1, by_priority);
        }
        self.scratch[..take].sort_unstable_by(by_priority);

        let mut datagrams = Vec::new();
        let mut buf = Vec::with_capacity(MAX_DATAGRAM);
        for chunk in self.scratch[..take].chunks(MAX_STATES_PER_SNAPSHOT) {
            let seq = self.next_seq;
            self.next_seq = next_snapshot_seq(seq);
            begin_snapshot(&mut buf, tick, seq);
            let mut sent = Sent { seq, tick, states: Vec::with_capacity(chunk.len()) };
            for ranked in chunk {
                let entry = &world[ranked.index as usize];
                let slot = entry.player() as usize;
                append_state(&mut buf, &entry.packed);
                sent.states.push((entry.player(), self.priority[slot]));
                self.priority[slot] = 0.0;
                self.urgent[slot] = false;
                self.last_sent[slot] = tick + 1;
            }
            remember(&mut self.in_flight, sent);
            datagrams.push(buf.clone());
        }
        if datagrams.is_empty() {
            let seq = self.next_seq;
            self.next_seq = next_snapshot_seq(seq);
            begin_snapshot(&mut buf, tick, seq);
            remember(&mut self.in_flight, Sent { seq, tick, states: Vec::new() });
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
    fn when_the_near_players_do_not_all_fit_they_take_turns() {
        // 60 players within 10 units, a budget for about 20 of them a tick
        let radii: Vec<f32> = (0..60).map(|i| 1.0 + i as f32 * 0.1).collect();
        let world = ring(&radii);
        let mut planner = Planner::new(PlannerConfig::with_budget(11_000));
        let mut sent = vec![0u32; world.len()];
        for tick in 0..300 {
            for id in states_of(&planner.plan(&observer_at_origin(), &world, tick)) {
                sent[id as usize] += 1;
            }
        }
        let near = &sent[1..];
        let (lo, hi) = (near.iter().min().unwrap(), near.iter().max().unwrap());
        assert!(*lo > 0 && *hi <= *lo * 3, "uneven (those facing get double weight): least sent {lo}, most {hi}");
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
                Some(ServerMessage::Snapshot(crate::Snapshot { tick, seq: tick as u16 + 1, states: vec![] }))
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

    /// A recipient over a lossy link: what arrives is acknowledged, one tick
    /// later (the acknowledgement rides an Input, which may itself be lost).
    struct Link {
        planner: Planner,
        rng: halo_sim::Rng,
        loss: f32,
        /// What the recipient has received.
        ack: Ack,
        /// The newest acknowledgement that reached the planner.
        heard: Ack,
        acknowledge: bool,
        /// Last tick each player was received in.
        last: Vec<Option<u32>>,
        /// The longest a received player's state has gone without an update (ticks), by player.
        longest: Vec<u32>,
    }

    impl Link {
        fn new(config: PlannerConfig, players: usize, loss: f32, acknowledge: bool) -> Link {
            Link {
                planner: Planner::new(config),
                rng: halo_sim::Rng::seeded(99),
                loss,
                ack: Ack::NONE,
                heard: Ack::NONE,
                acknowledge,
                last: vec![None; players],
                longest: vec![0; players],
            }
        }

        fn tick(&mut self, world: &[Entry], tick: u32) -> Vec<u16> {
            if self.acknowledge {
                self.planner.acknowledge(self.heard);
            }
            let plan = self.planner.plan(&observer_at_origin(), world, tick);
            let mut arrived = Vec::new();
            for d in &plan.datagrams {
                if self.rng.next_f32() < self.loss {
                    continue;
                }
                let Some(ServerMessage::Snapshot(s)) = ServerMessage::decode(d) else { panic!("not a snapshot") };
                self.ack.record(s.seq);
                arrived.extend(s.states.iter().map(|st| st.player()));
            }
            // the Input carrying the acknowledgement goes up now and reaches the planner for the next tick
            if self.rng.next_f32() >= self.loss {
                self.heard = self.ack;
            }
            for id in &arrived {
                if let Some(before) = self.last[*id as usize] {
                    self.longest[*id as usize] = self.longest[*id as usize].max(tick - before);
                }
                self.last[*id as usize] = Some(tick);
            }
            arrived
        }

        fn longest_gap(&self) -> u32 {
            self.longest.iter().copied().max().unwrap()
        }
    }

    #[test]
    fn a_snapshot_the_recipient_did_not_get_is_resent_at_once_not_a_cycle_later() {
        // 100 players far away and a budget for about 10 states a tick: a cycle is about ten ticks
        let radii: Vec<f32> = (0..100).map(|i| 40.0 + i as f32 * 0.3).collect();
        let world = ring(&radii);
        let config = PlannerConfig { max_stale_ticks: 1000, ..PlannerConfig::with_budget(6_000) };
        let mut planner = Planner::new(config);
        let (mut ack, mut lost) = (Ack::NONE, Vec::new());
        for tick in 0..40 {
            planner.acknowledge(ack);
            let plan = planner.plan(&observer_at_origin(), &world, tick);
            let seq = match ServerMessage::decode(&plan.datagrams[0]) {
                Some(ServerMessage::Snapshot(s)) => s.seq,
                _ => unreachable!(),
            };
            if tick == 20 {
                // this tick's datagram never arrives
                lost = states_of(&plan);
                assert!(lost.len() >= 5, "the budget buys a few states a tick");
            } else {
                ack.record(seq);
            }
            if tick == 22 {
                // two ticks on, the acknowledgement has gone past the lost one: it is resent now
                let again = states_of(&plan);
                for id in &lost {
                    assert!(again.contains(id), "player {id} lost at tick 20 was not resent by tick 22: {again:?}");
                }
            }
        }
    }

    #[test]
    fn under_loss_acknowledgements_shorten_the_longest_wait() {
        let radii: Vec<f32> = (0..300).map(|i| 30.0 + (i as f32 * 0.37) % 70.0).collect();
        let world = ring(&radii);
        let config = PlannerConfig { max_stale_ticks: 1000, ..PlannerConfig::with_budget(30_000) };
        let (mut with, mut without) = (Link::new(config, 301, 0.05, true), Link::new(config, 301, 0.05, false));
        for tick in 0..2000 {
            with.tick(&world, tick);
            without.tick(&world, tick);
        }
        println!("longest wait: {} ticks with acknowledgements, {} without", with.longest_gap(), without.longest_gap());
        assert!(with.longest_gap() < without.longest_gap(), "{} vs {}", with.longest_gap(), without.longest_gap());
    }

    #[test]
    fn no_player_waits_longer_than_the_cap_plus_what_loss_costs() {
        // 500 players, the budget of the design (90 KB/s), 5% loss each way
        let radii: Vec<f32> = (0..499).map(|i| 3.0 + (i as f32 * 0.37) % 90.0).collect();
        let world = ring(&radii);
        let config = PlannerConfig::with_budget(90_000);
        let mut link = Link::new(config, 500, 0.05, true);
        for tick in 0..3000 {
            link.tick(&world, tick);
        }
        println!("500 players, 5% loss: longest wait {} ticks", link.longest_gap());
        assert!(link.longest_gap() <= 20, "{} ticks", link.longest_gap());
    }

    #[test]
    fn a_player_not_sent_for_the_cap_is_sent_before_nearer_ones() {
        // 60 near players that the budget cannot all keep up with, and one far one that falls due
        let mut radii: Vec<f32> = (0..60).map(|i| 1.0 + i as f32 * 0.1).collect();
        radii.push(95.0);
        let world = ring(&radii);
        let config = PlannerConfig { max_stale_ticks: 12, ..PlannerConfig::with_budget(8_000) };
        let mut planner = Planner::new(config);
        let mut last_far = None;
        for tick in 0..200 {
            let got = states_of(&planner.plan(&observer_at_origin(), &world, tick));
            if got.contains(&61) {
                if let Some(before) = last_far {
                    assert!(tick - before <= 12, "the far player waited {} ticks", tick - before);
                }
                last_far = Some(tick);
            }
        }
        assert!(last_far.is_some());
    }

    #[test]
    fn a_new_recipient_is_sent_everyone_nearest_first() {
        let radii: Vec<f32> = (0..200).map(|i| 5.0 + i as f32 * 0.5).collect();
        let world = ring(&radii);
        let mut planner = Planner::new(PlannerConfig::with_budget(20_000));
        let first = states_of(&planner.plan(&observer_at_origin(), &world, 500));
        let second = states_of(&planner.plan(&observer_at_origin(), &world, 501));
        assert!(!first.is_empty() && first.len() < 200, "the budget is a tick's worth of it");
        let mut seen: Vec<u16> = first.iter().chain(&second).copied().collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), first.len() + second.len(), "nobody twice before everyone once");
        // the nearest players come first (the facing bonus makes the order of the rest uneven)
        assert!(first.iter().filter(|id| **id <= 20).count() >= 15, "{first:?}");
    }

    #[test]
    fn a_player_who_leaves_and_an_id_that_is_reused_start_afresh() {
        let mut world = ring(&[5.0, 6.0]);
        let mut planner = Planner::new(PlannerConfig::with_budget(90_000));
        for tick in 0..5 {
            planner.plan(&observer_at_origin(), &world, tick);
        }
        // player 2 leaves for a while, then a new player takes the id
        let gone = world.remove(2);
        for tick in 5..8 {
            assert_eq!(states_of(&planner.plan(&observer_at_origin(), &world, tick)), [1]);
        }
        world.push(gone);
        let got = states_of(&planner.plan(&observer_at_origin(), &world, 8));
        assert!(got.contains(&2), "the newcomer is sent at once");
    }
}
