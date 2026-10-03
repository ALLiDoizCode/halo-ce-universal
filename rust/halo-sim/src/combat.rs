//! Fighting: what players carry, and how the server turns the hits their
//! clients report into damage and deaths.
//!
//! As with movement, each client decides its own player's hits (its engine
//! fires the weapon and sees what the shot hit), and the server validates
//! them and owns the damage: [`resolve`] takes the tick's reports and answers
//! with the [`Death`]s that [`crate::rules::play`] applies, and the events of
//! what happened. Nothing here reads a clock or does I/O.
//!
//! # The checks
//!
//! They are the existing netcode's (`port/linux/game/network_damage.c`, whose
//! numbers these are), cheapest first, and a report that fails any is
//! rejected and counted ([`Reject`], [`Shooter::rejected`]):
//!
//! 1. the numbers are numbers; the shooter and the target are in the match,
//!    alive, and not the same player (the client hides the bodies of the dead,
//!    which the gateway still sends, so a hit on a body is a lie or a lag);
//! 2. **the shooter owns the weapon**: it is in their [`Loadout`], or was in
//!    the last [`RECENT_WEAPON_TICKS`] (what a shot still in flight needs);
//! 3. **the report is recent**: made at a tick that is not ahead of the
//!    server's and not more than [`REPORT_MAXIMUM_AGE_TICKS`] (3 seconds) behind
//!    it, as a burst of reports held back by a lost network is not;
//! 4. the impact is at the target: within the player's height and
//!    [`IMPACT_TOLERANCE`] of where the shooter says they saw the target;
//! 5. **the rate of fire is possible**: a shooter's hits draw from a bucket of
//!    [`BURST_SECONDS`] seconds of fire that refills at a second a second, a
//!    hit taking what one of the weapon's projectiles takes to fire at
//!    [`RATE_MARGIN`] times the weapon's fastest rate (a shotgun's pellets
//!    each count). It is drawn from before the history is looked through, so
//!    that a flood of false reports costs its sender no more than the hits it
//!    pays for;
//! 6. the shooter was within the weapon's reach of the impact, where the tags
//!    bound it (the projectile's range), at a tick since the shot was fired,
//!    by where the server saw them ([`RANGE_TOLERANCE`] and a few ticks of
//!    their speed more);
//! 7. **the target was within reach of where the server saw it recently**:
//!    at a tick as far back as the report was made, within
//!    [`HISTORY_TOLERANCE`] and a few ticks of its speed of where the shooter
//!    says it was. There is no check of line of sight: a wall between is not
//!    looked for, as the existing netcode does not.
//!
//! What the server saw ([`Trails`]) is kept in the module's memory, not its
//! tables (500 players for a second is 16,000 positions a tick that no one
//! reads but this check). A fresh module has none, and then a target is
//! checked against where it is now, with [`TARGET_TOLERANCE`] and 15 ticks of
//! its speed to spare, as the existing netcode checks one it has no history of.
//!
//! A report that passes deals its damage: a number between the tags' bounds
//! ([`crate::damage::roll`], on the match's random numbers) to the target's
//! shield and then health ([`crate::damage::Vitals::hit`]); a death of the
//! target is a [`Death`] with the shooter as the killer.
//!
//! # What a player carries
//!
//! A [`Fighter`] is a player's health and shields (as of a tick: the shield's
//! recharge is counted when someone looks, see [`crate::damage`]) and their
//! [`Loadout`]. A player who spawns carries the match's starting weapon
//! ([`starting_weapon`]).

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use halo_map::combat::{Damage, Weapon};

use crate::damage::{roll, Hurt, Vitals};
use crate::map::MapData;
use crate::math::sqrt;
use crate::rng::Rng;
use crate::rules::{Death, GameStore};
use crate::state::{PlayerId, Store};
use crate::weapon::fastest_rate;
use crate::TICKS_PER_SECOND;

/// A reported hit is refused if the report was made more than this many ticks
/// ago (3 seconds): a burst held back by a lost network is not honoured.
pub const REPORT_MAXIMUM_AGE_TICKS: u64 = 3 * TICKS_PER_SECOND as u64;
/// How long a weapon is still the shooter's after they let go of it.
pub const RECENT_WEAPON_TICKS: u64 = 10 * TICKS_PER_SECOND as u64;
/// World units: the impact is within the target's height (the tags') and this far of where the shooter saw the target.
pub const IMPACT_TOLERANCE: f32 = 2.0;
/// The seconds of fire a shooter's bucket holds, and how many times the weapon's fastest rate it is read at.
pub const BURST_SECONDS: f32 = 3.0;
pub const RATE_MARGIN: f32 = 2.0;
/// World units: how far the shooter may have been from the impact, beyond the
/// weapon's reach, and (with how far they move in a few ticks) the target from
/// where the shooter says it was.
pub const RANGE_TOLERANCE: f32 = 6.0;
pub const RANGE_LEAD_TICKS: f32 = 6.0;
pub const HISTORY_TOLERANCE: f32 = 2.0;
pub const HISTORY_LEAD_TICKS: f32 = 3.0;
/// ... and for a target the server has no history of: where it is now.
pub const TARGET_TOLERANCE: f32 = 3.0;
pub const TARGET_LEAD_TICKS: f32 = 15.0;
/// The ticks of positions the server remembers (a power of two: about a second), and how far
/// past the tick of a report the look-back goes (the frame drawn a tick behind, the report's own tick).
pub const TRAIL_TICKS: usize = 32;
pub const HISTORY_SLACK_TICKS: u64 = 3;

/// A weapon slot with no weapon.
pub const NO_WEAPON: u16 = u16::MAX;

/// The name of the weapon a player starts with (see [`starting_weapon`]).
pub const STARTING_WEAPON_NAME: &str = "weapons\\pistol\\pistol.weap";

/// What a player carries: up to two weapons by their tag index, and the ones
/// they put down lately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Loadout {
    pub weapons: [u16; 2],
    /// A weapon the player put down (swapped for another, dropped), and the tick.
    pub dropped: [(u16, u64); 2],
}

impl Loadout {
    pub const EMPTY: Loadout = Loadout { weapons: [NO_WEAPON; 2], dropped: [(NO_WEAPON, 0); 2] };

    /// One weapon in hand.
    pub fn with(weapon: u16) -> Loadout {
        Loadout { weapons: [weapon, NO_WEAPON], ..Loadout::EMPTY }
    }

    /// Whether the player carries the weapon, or put it down in the last [`RECENT_WEAPON_TICKS`].
    pub fn owns(&self, weapon: u16, tick: u64) -> bool {
        weapon != NO_WEAPON
            && (self.weapons.contains(&weapon)
                || self.dropped.iter().any(|(w, at)| *w == weapon && tick.saturating_sub(*at) <= RECENT_WEAPON_TICKS))
    }

    /// Pick a weapon up into a free slot; `false` if both are taken.
    pub fn give(&mut self, weapon: u16) -> bool {
        match self.weapons.iter_mut().find(|w| **w == NO_WEAPON) {
            Some(slot) => {
                *slot = weapon;
                true
            }
            None => false,
        }
    }

    /// Put a weapon down: the player still owns it for a while.
    pub fn drop_weapon(&mut self, weapon: u16, tick: u64) {
        if let Some(slot) = self.weapons.iter_mut().find(|w| **w == weapon) {
            *slot = NO_WEAPON;
            // (the oldest of the two remembered goes)
            let oldest = if self.dropped[0].1 <= self.dropped[1].1 { 0 } else { 1 };
            self.dropped[oldest] = (weapon, tick);
        }
    }
}

/// A player's health and shields, and what they carry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fighter {
    pub id: PlayerId,
    /// As of the tick `tick`: the shield's recharge since is [`Fighter::vitals_at`]'s to count.
    pub vitals: Vitals,
    pub tick: u64,
    pub loadout: Loadout,
    /// The tick of the last hit that hurt the player, who it was by (`NO_WEAPON`
    /// for none), and how many hits have hurt them since they spawned: what a
    /// client shows the player is hit by.
    pub hurt_tick: u64,
    pub hurt_by: PlayerId,
    pub hurt_count: u32,
}

impl Fighter {
    /// The vitals as they are at `tick`.
    pub fn vitals_at(&self, map: &MapData, tick: u64) -> Vitals {
        let mut v = self.vitals;
        v.advance(&map.combat.resistance, tick.saturating_sub(self.tick));
        v
    }
}

/// A shooter's record: the bucket their hits draw from, and what became of their reports.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shooter {
    pub id: PlayerId,
    /// Seconds of fire in the bucket as of `hit_seconds_tick`.
    pub hit_seconds: f32,
    pub hit_seconds_tick: u64,
    pub accepted: u64,
    pub rejected: u64,
    /// Why the latest rejection was ([`Reject::code`]) and when.
    pub last_reject: u8,
    pub last_reject_tick: u64,
}

impl Shooter {
    pub fn new(id: PlayerId) -> Shooter {
        Shooter {
            id,
            hit_seconds: BURST_SECONDS,
            hit_seconds_tick: 0,
            accepted: 0,
            rejected: 0,
            last_reject: 0,
            last_reject_tick: 0,
        }
    }
}

/// Where the fighting keeps its state: the server's tables, or memory.
pub trait CombatStore {
    fn fighter(&self, id: PlayerId) -> Option<Fighter>;
    /// Insert the fighter, or replace the one with the same id.
    fn set_fighter(&mut self, fighter: Fighter);
    fn remove_fighter(&mut self, id: PlayerId) -> bool;
    /// The shooter's record (a new one for a player with none).
    fn shooter(&self, id: PlayerId) -> Shooter;
    fn set_shooter(&mut self, shooter: Shooter);
    fn remove_shooter(&mut self, id: PlayerId) -> bool;
}

/// A [`CombatStore`] in memory.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemoryCombat {
    fighters: BTreeMap<PlayerId, Fighter>,
    shooters: BTreeMap<PlayerId, Shooter>,
}

impl MemoryCombat {
    pub fn new() -> MemoryCombat {
        MemoryCombat::default()
    }
}

impl CombatStore for MemoryCombat {
    fn fighter(&self, id: PlayerId) -> Option<Fighter> {
        self.fighters.get(&id).copied()
    }
    fn set_fighter(&mut self, fighter: Fighter) {
        self.fighters.insert(fighter.id, fighter);
    }
    fn remove_fighter(&mut self, id: PlayerId) -> bool {
        self.fighters.remove(&id).is_some()
    }
    fn shooter(&self, id: PlayerId) -> Shooter {
        self.shooters.get(&id).copied().unwrap_or_else(|| Shooter::new(id))
    }
    fn set_shooter(&mut self, shooter: Shooter) {
        self.shooters.insert(shooter.id, shooter);
    }
    fn remove_shooter(&mut self, id: PlayerId) -> bool {
        self.shooters.remove(&id).is_some()
    }
}

/// The weapon a player starts with: [`STARTING_WEAPON_NAME`] if the map has
/// it. (The engine's own starting weapon in a game with no teams is the plasma
/// pistol on the maps as they ship, a weapon of charged shots and heat that is
/// for the weapons' ticket; the pistol is the engine's sidearm in capture the
/// flag, and the weapon of the simplest whole path from trigger to death.)
/// A map without it starts its players with the first weapon its tags have
/// that fires.
pub fn starting_weapon(map: &MapData) -> Option<u16> {
    let weapons = &map.combat.weapons;
    weapons
        .iter()
        .find(|w| w.name == STARTING_WEAPON_NAME)
        .or_else(|| weapons.iter().find(|w| w.triggers.first().is_some_and(|t| t.projectile.is_some())))
        .map(|w| w.tag_index)
}

/// A player has spawned: full health and shields, the starting weapon, a
/// fresh bucket's worth of hits kept (their record of reports is kept), and nothing
/// remembered of where they were.
pub fn spawn(combat: &mut impl CombatStore, trails: &mut Trails, map: &MapData, id: PlayerId, tick: u64) {
    combat.set_fighter(Fighter {
        id,
        vitals: Vitals::full(&map.combat.resistance),
        tick,
        loadout: starting_weapon(map).map_or(Loadout::EMPTY, Loadout::with),
        hurt_tick: 0,
        hurt_by: PlayerId::MAX,
        hurt_count: 0,
    });
    trails.forget(id);
}

/// A player has left the match.
pub fn leave(combat: &mut impl CombatStore, trails: &mut Trails, id: PlayerId) {
    combat.remove_fighter(id);
    combat.remove_shooter(id);
    trails.forget(id);
}

// ---------- what the server saw

/// Where the server saw the players over the last [`TRAIL_TICKS`] ticks.
#[derive(Debug, Clone)]
pub struct Trails {
    slots: Vec<Slot>,
}

#[derive(Debug, Clone, Default)]
struct Slot {
    /// The tick these positions are of, if any are.
    tick: Option<u64>,
    /// By player id.
    players: Vec<(PlayerId, [f32; 3])>,
}

/// Whether a target was seen near a place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seen {
    Near,
    Far,
    /// The server has no positions of the player in the ticks asked about.
    Unknown,
}

impl Default for Trails {
    fn default() -> Trails {
        Trails::new()
    }
}

impl Trails {
    pub fn new() -> Trails {
        Trails { slots: (0..TRAIL_TICKS).map(|_| Slot::default()).collect() }
    }

    /// Note where the players were at the end of `tick`.
    pub fn record(&mut self, tick: u64, players: impl IntoIterator<Item = (PlayerId, [f32; 3])>) {
        let slot = &mut self.slots[(tick % TRAIL_TICKS as u64) as usize];
        slot.tick = Some(tick);
        slot.players.clear();
        slot.players.extend(players);
        slot.players.sort_unstable_by_key(|(id, _)| *id);
    }

    /// Forget a player's positions (they have spawned elsewhere, or left).
    pub fn forget(&mut self, id: PlayerId) {
        for slot in &mut self.slots {
            if let Ok(i) = slot.players.binary_search_by_key(&id, |(p, _)| *p) {
                slot.players.remove(i);
            }
        }
    }

    /// Where the player was at the end of `tick`, if the server remembers.
    pub fn at(&self, id: PlayerId, tick: u64) -> Option<[f32; 3]> {
        let slot = &self.slots[(tick % TRAIL_TICKS as u64) as usize];
        if slot.tick != Some(tick) {
            return None;
        }
        slot.players.binary_search_by_key(&id, |(p, _)| *p).ok().map(|i| slot.players[i].1)
    }

    /// Whether the player was within `tolerance` and `lead` ticks of their
    /// speed of `position` at any tick from `from` to `to` (inclusive).
    pub fn near(&self, id: PlayerId, position: [f32; 3], from: u64, to: u64, tolerance: f32, lead: f32) -> Seen {
        let mut known = false;
        let mut tick = to;
        loop {
            if let Some(at) = self.at(id, tick) {
                known = true;
                let speed = match tick.checked_sub(1).and_then(|t| self.at(id, t)) {
                    Some(before) => distance(at, before),
                    None => 0.0,
                };
                let reach = tolerance + lead * speed;
                if distance_squared(at, position) <= reach * reach {
                    return Seen::Near;
                }
            }
            if tick <= from {
                break;
            }
            tick -= 1;
        }
        if known {
            Seen::Far
        } else {
            Seen::Unknown
        }
    }
}

fn distance_squared(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (dx, dy, dz) = (a[0] - b[0], a[1] - b[1], a[2] - b[2]);
    dx * dx + dy * dy + dz * dz
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    sqrt(distance_squared(a, b))
}

// ---------- a report

/// What a client says of a hit: the shooter is the player whose connection
/// reported it (the server knows who that is), the rest is theirs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HitReport {
    pub target: PlayerId,
    /// The weapon's tag index (`halo_map::combat::Weapon::tag_index`).
    pub weapon: u16,
    /// The part of the target that was hit: an index of the body's materials,
    /// -1 for none.
    pub material: i16,
    /// The server's tick the client had last heard of when it made the report:
    /// where the server looks back from.
    pub host_tick: u32,
    /// Where the shot hit.
    pub origin: [f32; 3],
    /// Where the shooter saw the target.
    pub target_position: [f32; 3],
}

/// Why a report was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    /// A number of the report is not one.
    NotFinite,
    /// The shooter or the target is not in the match.
    UnknownPlayer,
    /// The shooter is not alive.
    ShooterNotAlive,
    /// The target is not alive: a hit on a body, or on a player who has died this tick.
    TargetNotAlive,
    /// The shooter hit themselves.
    SelfHit,
    /// The shooter does not carry the weapon, nor did lately; or it is no weapon that deals a hit.
    WeaponNotOwned,
    /// The report was made at a tick the server has not reached.
    FromTheFuture,
    /// The report was made more than [`REPORT_MAXIMUM_AGE_TICKS`] ago.
    TooOld,
    /// The impact is not at the target.
    ImpactNotAtTarget,
    /// More hits than the weapon fires.
    TooFast,
    /// The shooter was not within the weapon's reach of the impact.
    OutOfReach,
    /// The server had the target nowhere near where the shooter says they saw it.
    TargetNotWhereSeen,
}

impl Reject {
    /// The code a table keeps (0 is for none).
    pub fn code(self) -> u8 {
        match self {
            Reject::NotFinite => 1,
            Reject::UnknownPlayer => 2,
            Reject::ShooterNotAlive => 3,
            Reject::TargetNotAlive => 4,
            Reject::SelfHit => 5,
            Reject::WeaponNotOwned => 6,
            Reject::FromTheFuture => 7,
            Reject::TooOld => 8,
            Reject::ImpactNotAtTarget => 9,
            Reject::TooFast => 10,
            Reject::OutOfReach => 11,
            Reject::TargetNotWhereSeen => 12,
        }
    }
}

/// What [`resolve`] did with a report.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HitEvent {
    Hit { shooter: PlayerId, target: PlayerId, hurt: Hurt },
    Rejected { shooter: PlayerId, reason: Reject },
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct HitOutcome {
    /// The deaths the hits caused, for [`crate::rules::play`].
    pub deaths: Vec<Death>,
    pub events: Vec<HitEvent>,
}

/// The damage of a weapon's hit, its rate and its reach, from its tags.
struct Dealing<'a> {
    damage: &'a Damage,
    /// Hits a second the shooter's bucket is drawn at.
    rate: f32,
    /// How far the weapon's projectile flies, and how many ticks it takes, where the tags bound them.
    reach: Option<(f32, u64)>,
}

fn dealing(weapon: &Weapon) -> Option<Dealing<'_>> {
    let trigger = weapon.triggers.first()?;
    let projectile = trigger.projectile.as_ref()?;
    let damage = projectile.impact_damage.as_ref()?;
    let rate = fastest_rate(weapon) * f32::from(trigger.projectiles_per_shot.max(1)) * RATE_MARGIN;
    let reach = if projectile.maximum_range > 0.0 {
        let slowest = projectile.initial_velocity.min(projectile.final_velocity);
        let mut ticks = (TRAIL_TICKS - 1) as f32;
        // (twice as long: a lob's arc)
        if slowest > 0.0 && 2.0 * projectile.maximum_range / slowest < ticks {
            ticks = 2.0 * projectile.maximum_range / slowest;
        }
        Some((projectile.maximum_range, ticks as u64 + 1))
    } else {
        None
    };
    Some(Dealing { damage, rate, reach })
}

/// Validate the tick's hit reports (`tick` is the tick being run: the
/// positions in `trails` are of ticks before it, and `store` has where
/// everyone is as of the end of the one before) and deal the damage of those
/// that pass. Each report is a player's, by the connection they came on.
#[allow(clippy::too_many_arguments)]
pub fn resolve(
    store: &impl Store,
    game: &impl GameStore,
    combat: &mut impl CombatStore,
    trails: &Trails,
    map: &MapData,
    rng: &mut Rng,
    tick: u64,
    reports: &[(PlayerId, HitReport)],
) -> HitOutcome {
    let mut outcome = HitOutcome::default();
    let teams = game.game().rules.teams;
    for (shooter_id, report) in reports {
        let shooter_id = *shooter_id;
        let verdict = judge(store, game, combat, trails, map, tick, shooter_id, report);
        let mut record = combat.shooter(shooter_id);
        match verdict {
            Err(reason) => {
                record.rejected += 1;
                record.last_reject = reason.code();
                record.last_reject_tick = tick;
                outcome.events.push(HitEvent::Rejected { shooter: shooter_id, reason });
                combat.set_shooter(record);
                continue;
            }
            Ok(weapon) => {
                record.accepted += 1;
                combat.set_shooter(record);
                let Some(dealing) = dealing(weapon) else { continue };
                let (Some(mut target), Some(victim)) = (combat.fighter(report.target), game.contestant(report.target))
                else {
                    continue;
                };
                let friendly = teams && game.contestant(shooter_id).is_some_and(|s| s.team == victim.team);
                let resistance = &map.combat.resistance;
                target.vitals.advance(resistance, tick.saturating_sub(target.tick));
                target.tick = tick;
                let total = roll(dealing.damage, 1.0, 1.0, rng);
                if let Some(hurt) = target.vitals.hit(resistance, dealing.damage, report.material, friendly, total) {
                    target.hurt_tick = tick;
                    target.hurt_by = shooter_id;
                    target.hurt_count += 1;
                    if hurt.killed {
                        outcome.deaths.push(Death { victim: report.target, killer: Some(shooter_id) });
                    }
                    outcome.events.push(HitEvent::Hit { shooter: shooter_id, target: report.target, hurt });
                }
                combat.set_fighter(target);
            }
        }
    }
    outcome
}

/// Whether the report passes the checks, and the weapon it is of if it does.
#[allow(clippy::too_many_arguments)]
fn judge<'a>(
    store: &impl Store,
    game: &impl GameStore,
    combat: &mut impl CombatStore,
    trails: &Trails,
    map: &'a MapData,
    tick: u64,
    shooter_id: PlayerId,
    report: &HitReport,
) -> Result<&'a Weapon, Reject> {
    if !report.origin.iter().chain(&report.target_position).all(|v| v.is_finite()) {
        return Err(Reject::NotFinite);
    }
    let (Some(shooter), Some(target)) = (game.contestant(shooter_id), game.contestant(report.target)) else {
        return Err(Reject::UnknownPlayer);
    };
    let (Some(shooter_fighter), Some(target_fighter)) = (combat.fighter(shooter_id), combat.fighter(report.target))
    else {
        return Err(Reject::UnknownPlayer);
    };
    if !shooter.is_alive() {
        return Err(Reject::ShooterNotAlive);
    }
    if !target.is_alive() || target_fighter.vitals.is_dead() {
        return Err(Reject::TargetNotAlive);
    }
    if shooter_id == report.target {
        return Err(Reject::SelfHit);
    }
    let weapon = shooter_fighter
        .loadout
        .owns(report.weapon, tick)
        .then(|| map.combat.weapon(report.weapon))
        .flatten()
        .ok_or(Reject::WeaponNotOwned)?;
    let dealing = dealing(weapon).ok_or(Reject::WeaponNotOwned)?;

    let host_tick = u64::from(report.host_tick);
    if host_tick > tick {
        return Err(Reject::FromTheFuture);
    }
    if tick - host_tick > REPORT_MAXIMUM_AGE_TICKS {
        return Err(Reject::TooOld);
    }

    let reach = map.movement.collision_height_standing + IMPACT_TOLERANCE;
    if distance_squared(report.origin, report.target_position) > reach * reach {
        return Err(Reject::ImpactNotAtTarget);
    }

    // the rate of fire, paid for before the history is looked through
    let mut record = combat.shooter(shooter_id);
    let elapsed = tick.saturating_sub(record.hit_seconds_tick) as f32 / TICKS_PER_SECOND as f32;
    record.hit_seconds = (record.hit_seconds + elapsed).min(BURST_SECONDS);
    record.hit_seconds_tick = tick;
    let cost = 1.0 / dealing.rate;
    if record.hit_seconds < cost {
        combat.set_shooter(record);
        return Err(Reject::TooFast);
    }
    record.hit_seconds -= cost;
    combat.set_shooter(record);

    // how far back to look: as far as the report was made, and the flight of what was fired
    let back = (tick - host_tick + HISTORY_SLACK_TICKS).min(TRAIL_TICKS as u64 - 1);
    let to = tick.saturating_sub(1);
    if let Some((range, flight)) = dealing.reach {
        let from = tick.saturating_sub((back + flight).min(TRAIL_TICKS as u64 - 1));
        if trails.near(shooter_id, report.origin, from, to, range + RANGE_TOLERANCE, RANGE_LEAD_TICKS) == Seen::Far {
            return Err(Reject::OutOfReach);
        }
    }
    let from = tick.saturating_sub(back);
    match trails.near(report.target, report.target_position, from, to, HISTORY_TOLERANCE, HISTORY_LEAD_TICKS) {
        Seen::Near => {}
        Seen::Far => return Err(Reject::TargetNotWhereSeen),
        Seen::Unknown => {
            // no history of the target (a fresh module, a player just spawned): where they are now
            let Some(now) = store.player(report.target) else { return Err(Reject::UnknownPlayer) };
            let allowance = TARGET_TOLERANCE + TARGET_LEAD_TICKS * (map.max_move_speed() / TICKS_PER_SECOND as f32);
            if distance_squared(now.position, report.target_position) > allowance * allowance {
                return Err(Reject::TargetNotWhereSeen);
            }
        }
    }
    Ok(weapon)
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;
    use crate::fixtures::{combat_fixture, flat_floor_map, PISTOL};
    use crate::rules::{self, MemoryGame, Rules};
    use crate::state::{MemoryStore, Player};

    /// Two players in a Slayer match, alive, `gap` apart along x, with their trails.
    struct Fight {
        store: MemoryStore,
        game: MemoryGame,
        combat: MemoryCombat,
        trails: Trails,
        map: MapData,
        rng: Rng,
        tick: u64,
    }

    const SHOOTER: PlayerId = 1;
    const TARGET: PlayerId = 2;

    fn fight(gap: f32) -> Fight {
        let mut map = flat_floor_map();
        map.combat = combat_fixture();
        let mut store = MemoryStore::new();
        let mut game = MemoryGame::new(Rules::slayer());
        let mut combat = MemoryCombat::new();
        let mut trails = Trails::new();
        for (id, x) in [(SHOOTER, 0.0), (TARGET, gap)] {
            store.set_player(Player::new(id, [x, 0.0, 0.0], 0.0, 0.0));
            rules::enter_placed(&mut game, id, id as u8, [x, 0.0, 0.0], 0.0);
            spawn(&mut combat, &mut trails, &map, id, 0);
        }
        // a second of the server seeing them there
        for t in 1..=40 {
            trails.record(t, [(SHOOTER, [0.0, 0.0, 0.0]), (TARGET, [gap, 0.0, 0.0])]);
        }
        Fight { store, game, combat, trails, map, rng: Rng::seeded(1), tick: 41 }
    }

    impl Fight {
        fn report(&self) -> HitReport {
            HitReport {
                target: TARGET,
                weapon: PISTOL,
                material: 1,
                host_tick: (self.tick - 1) as u32,
                origin: [self.map_gap(), 0.0, 0.3],
                target_position: [self.map_gap(), 0.0, 0.0],
            }
        }

        fn map_gap(&self) -> f32 {
            self.store.player(TARGET).unwrap().position[0]
        }

        fn shoot(&mut self, report: HitReport) -> HitOutcome {
            self.shoot_as(SHOOTER, report)
        }

        fn shoot_as(&mut self, shooter: PlayerId, report: HitReport) -> HitOutcome {
            resolve(
                &self.store,
                &self.game,
                &mut self.combat,
                &self.trails,
                &self.map,
                &mut self.rng,
                self.tick,
                &[(shooter, report)],
            )
        }

        fn reasons(outcome: &HitOutcome) -> Vec<Reject> {
            outcome
                .events
                .iter()
                .filter_map(|e| match e {
                    HitEvent::Rejected { reason, .. } => Some(*reason),
                    _ => None,
                })
                .collect()
        }
    }

    #[test]
    fn a_report_that_passes_every_check_hurts_the_target_and_is_counted() {
        let mut f = fight(5.0);
        let report = f.report();
        let outcome = f.shoot(report);
        assert!(matches!(outcome.events[..], [HitEvent::Hit { shooter: SHOOTER, target: TARGET, .. }]), "{outcome:?}");
        assert!(outcome.deaths.is_empty());
        let target = f.combat.fighter(TARGET).unwrap();
        assert!(target.vitals.shield < 1.0);
        assert_eq!((target.hurt_by, target.hurt_count, target.hurt_tick), (SHOOTER, 1, 41));
        let shooter = f.combat.shooter(SHOOTER);
        assert_eq!((shooter.accepted, shooter.rejected), (1, 0));
    }

    #[test]
    fn a_report_of_a_weapon_the_shooter_does_not_own_is_rejected_and_counted() {
        let mut f = fight(5.0);
        let mut report = f.report();
        report.weapon = 999;
        let outcome = f.shoot(report);
        assert_eq!(Fight::reasons(&outcome), [Reject::WeaponNotOwned]);
        assert_eq!(f.combat.fighter(TARGET).unwrap().vitals.shield, 1.0, "no damage");
        let shooter = f.combat.shooter(SHOOTER);
        assert_eq!((shooter.accepted, shooter.rejected, shooter.last_reject), (0, 1, Reject::WeaponNotOwned.code()));
        // a weapon put down lately is still theirs, for a while
        let mut fighter = f.combat.fighter(SHOOTER).unwrap();
        fighter.loadout.drop_weapon(PISTOL, f.tick);
        f.combat.set_fighter(fighter);
        f.tick += RECENT_WEAPON_TICKS;
        assert!(f
            .shoot(HitReport { host_tick: (f.tick - 1) as u32, ..report_for(&f) })
            .events
            .iter()
            .all(|e| !matches!(e, HitEvent::Rejected { reason: Reject::WeaponNotOwned, .. })));
        f.tick += 2;
        assert_eq!(
            Fight::reasons(&f.shoot(HitReport { host_tick: (f.tick - 1) as u32, ..report_for(&f) })),
            [Reject::WeaponNotOwned]
        );
    }

    fn report_for(f: &Fight) -> HitReport {
        f.report()
    }

    #[test]
    fn a_target_out_of_reach_of_where_the_server_saw_it_is_rejected() {
        let mut f = fight(5.0);
        let mut report = f.report();
        // the shooter says they saw the target 3 units from anywhere it was (and put the impact there too)
        report.target_position[1] += 3.5;
        report.origin[1] += 3.5;
        assert_eq!(Fight::reasons(&f.shoot(report)), [Reject::TargetNotWhereSeen]);
        // ... but where it was a few ticks ago counts: the target has since moved
        let mut f = fight(5.0);
        f.trails.record(40, [(SHOOTER, [0.0; 3]), (TARGET, [5.0, 6.0, 0.0])]);
        f.store.set_player(Player::new(TARGET, [5.0, 6.0, 0.0], 0.0, 0.0));
        let mut report = f.report();
        report.host_tick = 38;
        report.target_position = [5.0, 0.0, 0.0];
        report.origin = [5.0, 0.0, 0.3];
        assert!(matches!(f.shoot(report).events[..], [HitEvent::Hit { .. }]), "a tick it was there");
    }

    #[test]
    fn a_target_the_server_has_no_history_of_is_checked_against_where_it_is() {
        let mut f = fight(5.0);
        f.trails = Trails::new();
        let report = f.report();
        assert!(matches!(f.shoot(report).events[..], [HitEvent::Hit { .. }]));
        let mut far = f.report();
        far.target_position[0] += 20.0;
        far.origin[0] += 20.0;
        assert_eq!(Fight::reasons(&f.shoot(far)), [Reject::TargetNotWhereSeen]);
    }

    #[test]
    fn hits_faster_than_the_weapon_could_fire_are_rejected() {
        let mut f = fight(5.0);
        // the bucket holds 3 seconds of fire; a hit costs 1 / (2 x 3.5) of one
        let report = f.report();
        let mut accepted = 0;
        let mut rejected = 0;
        for _ in 0..40 {
            let outcome = f.shoot(report);
            // (the target must stay alive to be shot at: heal it)
            let mut t = f.combat.fighter(TARGET).unwrap();
            t.vitals = Vitals::full(&f.map.combat.resistance);
            f.combat.set_fighter(t);
            for e in &outcome.events {
                match e {
                    HitEvent::Hit { .. } => accepted += 1,
                    HitEvent::Rejected { reason, .. } => {
                        assert_eq!(*reason, Reject::TooFast);
                        rejected += 1;
                    }
                }
            }
        }
        // 3 s at 7 a second is 21 hits at once (the last of them depends on how a float rounds), and nothing else that same tick
        assert!((20..=21).contains(&accepted), "{accepted}");
        assert_eq!(accepted + rejected, 40);
        // a second later, 7 more
        f.tick += TICKS_PER_SECOND as u64;
        let report = HitReport { host_tick: (f.tick - 1) as u32, ..f.report() };
        let accepted_later = (0..14)
            .filter(|_| {
                let outcome = f.shoot(report);
                let mut t = f.combat.fighter(TARGET).unwrap();
                t.vitals = Vitals::full(&f.map.combat.resistance);
                f.combat.set_fighter(t);
                matches!(outcome.events[..], [HitEvent::Hit { .. }])
            })
            .count();
        assert!((6..=7).contains(&accepted_later), "{accepted_later}");
    }

    #[test]
    fn a_report_that_is_too_old_or_from_the_future_is_rejected() {
        let mut f = fight(5.0);
        f.tick = 200;
        f.trails.record(199, [(SHOOTER, [0.0; 3]), (TARGET, [5.0, 0.0, 0.0])]);
        let mut report = f.report();
        report.host_tick = 200 - REPORT_MAXIMUM_AGE_TICKS as u32;
        assert!(matches!(f.shoot(report).events[..], [HitEvent::Hit { .. }]), "exactly three seconds old is honoured");
        report.host_tick -= 1;
        assert_eq!(Fight::reasons(&f.shoot(report)), [Reject::TooOld]);
        report.host_tick = 201;
        assert_eq!(Fight::reasons(&f.shoot(report)), [Reject::FromTheFuture]);
    }

    #[test]
    fn a_hit_on_a_target_who_is_dead_is_rejected() {
        let mut f = fight(5.0);
        let victim = f.game.contestant(TARGET).unwrap();
        // the rules have killed the target
        let mut game = f.game.clone();
        rules::play(
            &mut f.store,
            &mut game,
            &f.map,
            &mut Rng::seeded(1),
            41,
            &[Death { victim: TARGET, killer: Some(SHOOTER) }],
            &[],
        );
        assert!(victim.is_alive() && !game.contestant(TARGET).unwrap().is_alive());
        f.game = game;
        let report = f.report();
        assert_eq!(Fight::reasons(&f.shoot(report)), [Reject::TargetNotAlive]);
        // ... and a shooter who is dead cannot hit anyone
        let mut f = fight(5.0);
        let mut game = f.game.clone();
        rules::play(
            &mut f.store,
            &mut game,
            &f.map,
            &mut Rng::seeded(1),
            41,
            &[Death { victim: SHOOTER, killer: None }],
            &[],
        );
        f.game = game;
        let report = f.report();
        assert_eq!(Fight::reasons(&f.shoot(report)), [Reject::ShooterNotAlive]);
    }

    #[test]
    fn a_report_that_is_nonsense_is_rejected_and_hurts_nobody() {
        let mut f = fight(5.0);
        let mut report = f.report();
        report.origin[0] = f32::NAN;
        assert_eq!(Fight::reasons(&f.shoot(report)), [Reject::NotFinite]);
        let mut report = f.report();
        report.target = 77;
        assert_eq!(Fight::reasons(&f.shoot(report)), [Reject::UnknownPlayer]);
        let mut report = f.report();
        report.target = SHOOTER;
        assert_eq!(Fight::reasons(&f.shoot(report)), [Reject::SelfHit]);
        let mut report = f.report();
        report.origin[2] = 40.0;
        assert_eq!(Fight::reasons(&f.shoot(report)), [Reject::ImpactNotAtTarget]);
        assert_eq!(f.combat.fighter(TARGET).unwrap().vitals, Vitals::full(&f.map.combat.resistance));
        assert_eq!(f.combat.shooter(SHOOTER).rejected, 4);
    }

    #[test]
    fn a_shooter_out_of_reach_of_the_impact_is_rejected() {
        let mut f = fight(100.0);
        // the shooter and the target 100 apart: the pistol's 40 does not reach
        let report = f.report();
        assert_eq!(Fight::reasons(&f.shoot(report)), [Reject::OutOfReach]);
        let mut f = fight(30.0);
        let report = f.report();
        assert!(matches!(f.shoot(report).events[..], [HitEvent::Hit { .. }]));
    }

    #[test]
    fn enough_hits_kill_the_target_and_credit_the_shooter() {
        let mut f = fight(5.0);
        let report = f.report();
        let mut deaths = Vec::new();
        for i in 0..6 {
            f.tick += 10;
            for t in f.tick - 10..f.tick {
                f.trails.record(t, [(SHOOTER, [0.0; 3]), (TARGET, [5.0, 0.0, 0.0])]);
            }
            let outcome = f.shoot(HitReport { host_tick: (f.tick - 1) as u32, ..report });
            deaths.extend(outcome.deaths.iter().copied());
            if !deaths.is_empty() {
                assert_eq!(i, 4, "three shots for the shield and two for the body");
                break;
            }
        }
        assert_eq!(deaths, [Death { victim: TARGET, killer: Some(SHOOTER) }]);
    }

    #[test]
    fn a_hit_in_a_team_game_on_a_teammate_is_friendly_and_counted_as_a_betrayal_by_the_rules() {
        let mut f = fight(5.0);
        f.game = MemoryGame::new(Rules::team_slayer());
        // both on team 1
        for id in [SHOOTER, TARGET] {
            rules::enter_placed(&mut f.game, id, 0, [0.0; 3], 0.0);
        }
        let mut resistance_map = f.map.clone();
        resistance_map.combat.resistance.friendly_damage_resistance = 1.0;
        f.map = resistance_map;
        let mut t = f.combat.fighter(TARGET).unwrap();
        t.vitals.shield = 0.0;
        f.combat.set_fighter(t);
        let report = f.report();
        f.shoot(report);
        assert_eq!(f.combat.fighter(TARGET).unwrap().vitals.body, 1.0, "a teammate's hit costs the body nothing here");
    }

    #[test]
    fn a_spawned_player_carries_the_starting_weapon_and_full_vitals() {
        let map = {
            let mut m = flat_floor_map();
            m.combat = combat_fixture();
            m
        };
        let mut combat = MemoryCombat::new();
        let mut trails = Trails::new();
        trails.record(3, vec![(4, [1.0, 2.0, 3.0])]);
        spawn(&mut combat, &mut trails, &map, 4, 9);
        let f = combat.fighter(4).unwrap();
        assert_eq!(f.loadout, Loadout::with(PISTOL));
        assert_eq!(f.vitals, Vitals::full(&map.combat.resistance));
        assert_eq!(trails.at(4, 3), None, "nothing remembered of where they were");
        leave(&mut combat, &mut trails, 4);
        assert!(combat.fighter(4).is_none());
    }

    #[test]
    fn a_loadout_holds_two_weapons_and_remembers_those_put_down() {
        let mut l = Loadout::EMPTY;
        assert!(l.give(5) && l.give(6) && !l.give(7));
        assert!(l.owns(5, 0) && l.owns(6, 0) && !l.owns(7, 0));
        l.drop_weapon(5, 100);
        assert!(l.owns(5, 100 + RECENT_WEAPON_TICKS) && !l.owns(5, 101 + RECENT_WEAPON_TICKS));
        assert!(l.give(7));
        assert!(!l.owns(NO_WEAPON, 0));
    }

    #[test]
    fn trails_remember_a_second_of_positions() {
        let mut t = Trails::new();
        for tick in 1..=100u64 {
            t.record(tick, [(1, [tick as f32, 0.0, 0.0])]);
        }
        assert_eq!(t.at(1, 100), Some([100.0, 0.0, 0.0]));
        assert_eq!(t.at(1, 100 - TRAIL_TICKS as u64), None, "overwritten");
        assert_eq!(t.near(1, [90.0, 0.0, 0.0], 95, 100, 1.0, 0.0), Seen::Far);
        assert_eq!(t.near(1, [96.0, 0.0, 0.0], 95, 100, 0.5, 0.0), Seen::Near);
        assert_eq!(t.near(2, [0.0; 3], 95, 100, 100.0, 0.0), Seen::Unknown);
        // a moving player's speed widens the reach
        assert_eq!(t.near(1, [100.0 + 2.9, 0.0, 0.0], 100, 100, 0.0, 3.0), Seen::Near);
    }
}
