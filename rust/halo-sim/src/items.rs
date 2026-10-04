//! Items on the ground: where they spawn and how often, how a loose one falls
//! and comes to rest on the map, and how long it lasts. [`crate::pickups`] has
//! who may take one and what it does for them; [`tick`] here runs both once a
//! match tick.
//!
//! The server owns every item (a client decides only its own player's
//! movement and hits): [`Item`]s are rows of the match module's public `item`
//! table, which every client subscribes to, and the match module calls [`tick`]
//! after [`crate::rules::play`]. The engine's own (`source/game/game_engine.c`:
//! `game_engine_update_item_spawn`, `game_engine_update_purge`;
//! `source/items/items.c`: `item_update`) are the reference, and the numbers
//! are the map's tags' (`halo_map::items`).
//!
//! # Spawns and respawns
//!
//! Each of the scenario's netgame equipment placements that lists the game
//! (Slayer, here) makes an item when the game's time is a multiple of its
//! period: its own spawn time if it has one, else its item collection's, else
//! 30 seconds, and the item is one of the collection's, picked by weight. The
//! game's time is the ticks since the match began, the first tick being 0, so
//! every item is there at the start of a match. An item nobody has taken is
//! taken away as the next is made (the engine's purge deletes an item 30
//! seconds after it was last held, and a placement's item is given the rest of
//! its period besides), so a placement has at most one item; one that was taken
//! comes back at the next multiple of the period, not a period after it was
//! taken. That is the engine's own rule, and the intervals are the tags':
//! `halo_map::items::Placement`.
//!
//! An item is made where the placement says, and falls to the ground unless
//! the placement says it is made at rest ([`PLACEMENT_CREATED_AT_REST`]).
//! Grenades, which are items too (an equipment of powerup type grenade), are
//! the grenades' ticket's and are not made; neither are the powerups that
//! only bear on vehicles and speed (double speed, full-spectrum vision).
//!
//! # How an item falls
//!
//! [`Item::step`] is the engine's `item_update` for an item that is not at
//! rest, against the map's collision BSP: gravity ([`crate::walk::GRAVITY`]) is
//! added to its velocity, and if the straight path of the tick meets a surface
//! it comes to rest there when the surface is flat enough (its normal's
//! z above 0.7071) and it is not going into it faster than 0.05 world units a
//! tick, and otherwise bounces off with 0.4 of its speed into the surface,
//! kept 0.05 off it. The engine's item also collides with other objects (a
//! player, a vehicle); here only the map is hit, and the engine's rolling
//! (an item's spin) is not simulated at all.
//!
//! The fall is **a function of the item's drop** (where it began, its
//! velocity, the tick), so the server writes an item's row twice, when it
//! begins to fall and when it comes to rest, and every client that has the map
//! works out where it is in between with the same code ([`Item::advanced_to`]),
//! the native and WebAssembly builds agreeing to the bit. Nothing per tick
//! crosses the network for an item that falls.
//!
//! # Drops
//!
//! A weapon a player swaps for another, and each weapon a player dies holding
//! (the engine's `unit_drop_current_weapon` and `unit_drop_inventory_weapons`),
//! becomes an item at the player, thrown within 22.5 degrees of where they aim
//! at 0.8 to 1.2 world units a second (the engine's), with the rounds the
//! weapon had; the player who put it down cannot take it until it has come to
//! rest (the engine's `ignore_object_index`).

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use halo_map::collision::{TEST_BACK_FACING, TEST_FRONT_FACING, TEST_IGNORE_INVISIBLE};
use halo_map::game_type;
use halo_map::items::{powerup, Placement, PLACEMENT_CREATED_AT_REST};

use crate::combat::CombatStore;
use crate::map::MapData;
use crate::math::{add, along, cross, dot, normalize, scale, sin_cos, sub, Vec3};
use crate::pickups::{self, ItemEvent, Request};
use crate::rng::Rng;
use crate::rules::GameStore;
use crate::state::{PlayerId, Store};
use crate::walk::GRAVITY;
use crate::weapon::Hands;
use crate::TICKS_PER_SECOND;

pub type ItemId = u32;

/// [`Item::placement`] of an item that no placement made: a dropped weapon.
pub const NO_PLACEMENT: u16 = u16::MAX;
/// [`Item::ignore`] for no one.
pub const NO_PLAYER: PlayerId = PlayerId::MAX;

/// Ticks after an item was last held that it is taken away (the engine's
/// `game_engine_update_purge`: 900, 30 seconds).
pub const PURGE_TICKS: u64 = 30 * TICKS_PER_SECOND as u64;
/// The period of a placement that says none and whose collection says none
/// (the engine's).
pub const DEFAULT_PERIOD_TICKS: u64 = 30 * TICKS_PER_SECOND as u64;
/// The most items on the ground: past it the oldest dropped ones go, so that a
/// crowd cannot fill the match's table.
pub const MAX_ITEMS: usize = 2048;

/// A surface this flat (its normal's z above this) can hold an item
/// (`item_update`'s 0.7071).
#[allow(clippy::approx_constant)]
const REST_NORMAL_Z: f32 = 0.7071;
/// An item going into a surface slower than this, a tick's worth, stays on it.
const REST_SPEED: f32 = 0.05;
/// What a bounce keeps of the speed into the surface is `BOUNCE - 1`; the
/// engine's `-1.4 * dot(normal, velocity)`.
const BOUNCE: f32 = 1.4;
/// How far off a surface an item that bounced is put.
const PUSH_OFF: f32 = 0.05;
/// How far below the map an item may fall before it is gone.
const FALL_OUT_DEPTH: f32 = 50.0;

/// What `item_update` tests the path of an item against: the structure's
/// front-facing, visible surfaces (the engine's `ITEM_UPDATE_COLLISION_TEST_FLAGS`).
const PATH_FLAGS: u32 = TEST_FRONT_FACING | TEST_IGNORE_INVISIBLE;

/// An item on the ground, as of the tick `tick`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Item {
    pub id: ItemId,
    /// The tag index of the weapon or equipment (`halo_map::items::ItemDef::tag_index`).
    pub tag: u16,
    pub position: [f32; 3],
    /// World units a *tick* (the engine's unit); zero at rest.
    pub velocity: [f32; 3],
    /// The tick `position` and `velocity` are the item's at. An item at rest
    /// is where it is whatever the tick.
    pub tick: u64,
    pub resting: bool,
    /// The netgame equipment placement (an index of `halo_map::items::Items::placements`)
    /// that made the item, or [`NO_PLACEMENT`].
    pub placement: u16,
    /// A weapon's rounds: in its magazine, and in reserve.
    pub loaded: i16,
    pub reserve: i16,
    /// The tick the item was last held, or made: it is taken away [`PURGE_TICKS`] after.
    pub last_owned: u64,
    /// The player who put it down, who cannot take it until it rests.
    pub ignore: PlayerId,
}

/// What a tick of [`Item::step`] came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flight {
    Falling,
    Rested,
    /// It fell out of the map: it is gone.
    Lost,
}

impl Item {
    /// Bytes in [`snapshot_items`].
    pub const SNAPSHOT_SIZE: usize = 4 + 2 + 6 * 4 + 8 + 1 + 2 + 2 + 2 + 8 + 2;

    /// One tick of the engine's `item_update` for an item that is not at rest.
    pub fn step(&mut self, map: &MapData) -> Flight {
        self.tick += 1;
        let mut velocity = self.velocity;
        velocity[2] -= GRAVITY;
        let Some(hit) = map.collision.test_vector(PATH_FLAGS, self.position, velocity, 1.0) else {
            self.position = add(&self.position, &velocity);
            self.velocity = velocity;
            return if self.position[2] < map.world_bounds[4] - FALL_OUT_DEPTH {
                Flight::Lost
            } else {
                Flight::Falling
            };
        };
        let point = along(&self.position, &velocity, hit.t);
        let mut normal = usize::try_from(hit.surface_index)
            .ok()
            .and_then(|s| map.collision.surface_plane(s))
            .map_or([0.0, 0.0, 1.0], |p| p.n);
        // (a front-facing hit has the surface looking back along the path)
        if dot(&normal, &velocity) > 0.0 {
            normal = scale(&normal, -1.0);
        }
        let into = -dot(&normal, &velocity);
        if normal[2] > REST_NORMAL_Z && into < REST_SPEED {
            self.position = point;
            self.velocity = [0.0; 3];
            self.resting = true;
            self.ignore = NO_PLAYER;
            return Flight::Rested;
        }
        self.velocity = add(&velocity, &scale(&normal, BOUNCE * into));
        self.position = along(&point, &normal, PUSH_OFF);
        Flight::Falling
    }

    /// The item as it is at `tick`: stepped from where it was, if it was not
    /// at rest (an item that came to rest, or fell out of the map, on the way
    /// says so). Both the server and a client run this on the row the server
    /// wrote and agree to the bit.
    pub fn advanced_to(mut self, map: &MapData, tick: u64) -> (Item, Flight) {
        if self.resting {
            return (self, Flight::Rested);
        }
        let mut flight = Flight::Falling;
        while self.tick < tick && flight == Flight::Falling {
            flight = self.step(map);
        }
        (self, flight)
    }

    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.id.to_le_bytes());
        out.extend_from_slice(&self.tag.to_le_bytes());
        for v in self.position.iter().chain(&self.velocity) {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&self.tick.to_le_bytes());
        out.push(self.resting as u8);
        out.extend_from_slice(&self.placement.to_le_bytes());
        out.extend_from_slice(&self.loaded.to_le_bytes());
        out.extend_from_slice(&self.reserve.to_le_bytes());
        out.extend_from_slice(&self.last_owned.to_le_bytes());
        out.extend_from_slice(&self.ignore.to_le_bytes());
    }
}

/// What a player has beside their weapons' tags: the rounds of the weapons they
/// carry, and how long they are camouflaged for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kit {
    pub player: PlayerId,
    /// The rounds of the weapon in each slot of the player's loadout.
    pub ammo: [Ammo; 2],
    /// The tick the player's active camouflage runs out at; 0 when they have none.
    pub camo_until: u64,
    /// Counts the times the server changed the rounds (a pickup, a spawn): a
    /// client takes the server's rounds when it changes, and keeps its own
    /// (which it counts as it fires) otherwise.
    pub version: u32,
}

/// The rounds of a weapon held: in its magazine and in reserve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Ammo {
    pub loaded: i16,
    pub reserve: i16,
}

impl Kit {
    pub fn new(player: PlayerId) -> Kit {
        Kit { player, ammo: [Ammo::default(); 2], camo_until: 0, version: 0 }
    }

    pub fn is_camouflaged(&self, tick: u64) -> bool {
        self.camo_until > tick
    }

    /// Bytes in [`snapshot_items`].
    pub const SNAPSHOT_SIZE: usize = 2 + 8 + 8 + 4;

    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.player.to_le_bytes());
        for a in &self.ammo {
            out.extend_from_slice(&a.loaded.to_le_bytes());
            out.extend_from_slice(&a.reserve.to_le_bytes());
        }
        out.extend_from_slice(&self.camo_until.to_le_bytes());
        out.extend_from_slice(&self.version.to_le_bytes());
    }
}

/// Where the items keep their state: the server's tables, or memory.
pub trait ItemStore {
    /// Every item as last written, in id order.
    fn items(&self) -> Vec<Item>;
    fn item(&self, id: ItemId) -> Option<Item>;
    /// Add an item, whose `id` is ignored: the store gives it one (the next,
    /// in order) and returns it.
    fn insert_item(&mut self, item: Item) -> ItemId;
    /// Replace the item with the same id.
    fn update_item(&mut self, item: Item);
    fn remove_item(&mut self, id: ItemId) -> bool;
    /// Where an item that is falling is now, if the store has stepped it since
    /// it was written: kept in memory, never in the item's row, so that a fall
    /// writes nothing. A store may forget it (a fresh module) and the tick works
    /// it out again from the row.
    fn flight(&self, id: ItemId) -> Option<Item>;
    fn set_flight(&mut self, item: Item);
    fn clear_flight(&mut self, id: ItemId);
    /// A player's kit (a fresh one for a player with none: see [`Kit::new`]).
    fn kit(&self, player: PlayerId) -> Kit;
    fn set_kit(&mut self, kit: Kit);
    fn remove_kit(&mut self, player: PlayerId) -> bool;
    /// The players whose camouflage has not been taken away, in id order.
    fn camouflaged(&self) -> Vec<PlayerId>;
}

/// An [`ItemStore`] in memory. Iteration is in id order, so it is deterministic.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemoryItems {
    items: BTreeMap<ItemId, Item>,
    flights: BTreeMap<ItemId, Item>,
    kits: BTreeMap<PlayerId, Kit>,
    next: ItemId,
}

impl MemoryItems {
    pub fn new() -> MemoryItems {
        MemoryItems::default()
    }
}

impl ItemStore for MemoryItems {
    fn items(&self) -> Vec<Item> {
        self.items.values().copied().collect()
    }
    fn item(&self, id: ItemId) -> Option<Item> {
        self.items.get(&id).copied()
    }
    fn insert_item(&mut self, mut item: Item) -> ItemId {
        item.id = self.next;
        self.next += 1;
        self.items.insert(item.id, item);
        item.id
    }
    fn update_item(&mut self, item: Item) {
        self.items.insert(item.id, item);
    }
    fn remove_item(&mut self, id: ItemId) -> bool {
        self.flights.remove(&id);
        self.items.remove(&id).is_some()
    }
    fn flight(&self, id: ItemId) -> Option<Item> {
        self.flights.get(&id).copied()
    }
    fn set_flight(&mut self, item: Item) {
        self.flights.insert(item.id, item);
    }
    fn clear_flight(&mut self, id: ItemId) {
        self.flights.remove(&id);
    }
    fn kit(&self, player: PlayerId) -> Kit {
        self.kits.get(&player).copied().unwrap_or_else(|| Kit::new(player))
    }
    fn set_kit(&mut self, kit: Kit) {
        self.kits.insert(kit.player, kit);
    }
    fn remove_kit(&mut self, player: PlayerId) -> bool {
        self.kits.remove(&player).is_some()
    }
    fn camouflaged(&self) -> Vec<PlayerId> {
        self.kits.values().filter(|k| k.camo_until != 0).map(|k| k.player).collect()
    }
}

/// The whole of an [`ItemStore`] as bytes, as [`crate::snapshot`] is for a
/// [`Store`]: every item (as the store has written it) and every kit that has
/// something in it. Two stores are equal exactly when their snapshots are.
pub fn snapshot_items(store: &impl ItemStore, players: &[PlayerId]) -> Vec<u8> {
    let mut out = Vec::new();
    for item in store.items() {
        item.write(&mut out);
    }
    for &p in players {
        store.kit(p).write(&mut out);
    }
    out
}

// ---------- spawns

/// What the engine's `match_game_type` makes of an entry of a game type list
/// for Slayer (and Team Slayer, which is Slayer with teams).
pub fn is_for_slayer(game_types: &[i16; 4]) -> bool {
    game_types
        .iter()
        .any(|t| matches!(*t, game_type::SLAYER | game_type::ALL | game_type::ALL_NON_CTF | game_type::ALL_NORMAL))
}

/// Ticks between the spawns of a placement.
pub fn period_ticks(placement: &Placement) -> u64 {
    let seconds = if placement.spawn_time != 0 { placement.spawn_time } else { placement.collection_spawn_time };
    if seconds > 0 {
        seconds as u64 * TICKS_PER_SECOND as u64
    } else {
        DEFAULT_PERIOD_TICKS
    }
}

/// Whether an item of the tag is one the game makes: a weapon (not a vehicle's
/// own: the placements do not name those), and of the equipment the powerups
/// that heal, shield and hide a player.
pub fn is_spawnable(map: &MapData, tag: u16) -> bool {
    map.items.def(tag).is_some_and(|d| {
        d.is_weapon || matches!(d.powerup_type, powerup::OVERSHIELD | powerup::ACTIVE_CAMOUFLAGE | powerup::HEALTH)
    })
}

/// The engine's `random_item`: one of a collection by weight (`None` for
/// none), from the match's random numbers.
fn choose(placement: &Placement, rng: &mut Rng) -> Option<u16> {
    let total: u32 = placement.permutations.iter().fold(0u32, |sum, (w, _)| sum + *w as u32);
    if total == 0 {
        return None;
    }
    let mut remaining = (rng.next_u32() % total) as i64;
    for (weight, tag) in &placement.permutations {
        remaining -= *weight as i64;
        if remaining < 0 {
            return Some(*tag);
        }
    }
    None
}

/// The rounds a weapon item of this tag is made with: the tag's initial ones.
pub fn initial_rounds(map: &MapData, tag: u16) -> (i16, i16) {
    map.combat.weapon(tag).map_or((0, 0), |w| {
        let hands = Hands::new(w);
        (hands.rounds_loaded, hands.rounds_total)
    })
}

fn spawn_placements(
    items: &mut impl ItemStore,
    map: &MapData,
    rng: &mut Rng,
    tick: u64,
    time: u64,
    events: &mut Vec<ItemEvent>,
) {
    for (index, placement) in map.items.placements.iter().enumerate() {
        if !is_for_slayer(&placement.game_types) || !time.is_multiple_of(period_ticks(placement)) {
            continue;
        }
        let Some(tag) = choose(placement, rng) else { continue };
        if !is_spawnable(map, tag) {
            continue;
        }
        // the item nobody took goes as the next is made
        let placement_index = index as u16;
        for old in items.items().into_iter().filter(|i| i.placement == placement_index) {
            items.remove_item(old.id);
            events.push(ItemEvent::Purged { item: old.id });
        }
        let (loaded, reserve) = initial_rounds(map, tag);
        let rests = placement.flags & PLACEMENT_CREATED_AT_REST != 0;
        let item = Item {
            id: 0,
            tag,
            position: placement.position,
            velocity: [0.0; 3],
            tick,
            resting: rests,
            placement: placement_index,
            loaded,
            reserve,
            // (the engine gives a placement's item the rest of its period besides the 30 seconds)
            last_owned: tick + period_ticks(placement).saturating_sub(PURGE_TICKS),
            ignore: NO_PLAYER,
        };
        let id = items.insert_item(item);
        events.push(ItemEvent::Spawned { item: id, tag, position: placement.position });
    }
}

// ---------- drops

/// Put a weapon down: an item at the player, thrown as the engine throws one,
/// with the rounds it had. `from` is where the player is (feet), `yaw` and
/// `pitch` where they aim. Returns the item's id.
#[allow(clippy::too_many_arguments)]
pub fn drop_item(
    items: &mut impl ItemStore,
    map: &MapData,
    rng: &mut Rng,
    tick: u64,
    player: PlayerId,
    from: Vec3,
    yaw: f32,
    pitch: f32,
    tag: u16,
    ammo: Ammo,
) -> ItemId {
    let (sy, cy) = sin_cos(yaw);
    let (sp, cp) = sin_cos(pitch);
    let aim = [cy * cp, sy * cp, sp];
    let velocity = scale(&throw_direction(aim, rng), 0.026_666_667 + (0.040_000_003 - 0.026_666_667) * rng.next_f32());
    // (from the player's middle, unless a wall is between it and the hand)
    let middle = [from[0], from[1], from[2] + 0.5 * map.movement.collision_height_standing];
    let hand = along(&middle, &[cy, sy, 0.0], 0.25);
    let position = if map
        .collision
        .test_vector(TEST_FRONT_FACING | TEST_BACK_FACING, middle, sub(&hand, &middle), 1.0)
        .is_some()
    {
        middle
    } else {
        hand
    };
    items.insert_item(Item {
        id: 0,
        tag,
        position,
        velocity,
        tick,
        resting: false,
        placement: NO_PLACEMENT,
        loaded: ammo.loaded,
        reserve: ammo.reserve,
        last_owned: tick,
        ignore: player,
    })
}

/// `random_vector_in_cone3d(aim, 0, 22.5 degrees)`: a direction within the
/// cone around `aim`, uniform over its cap.
fn throw_direction(aim: Vec3, rng: &mut Rng) -> Vec3 {
    const MAX_ANGLE: f32 = core::f32::consts::FRAC_PI_8;
    let (_, cos_max) = sin_cos(MAX_ANGLE);
    let cos_theta = 1.0 - rng.next_f32() * (1.0 - cos_max);
    let sin_theta = crate::math::sqrt((1.0 - cos_theta * cos_theta).max(0.0));
    let (sin_phi, cos_phi) = sin_cos(rng.next_f32() * 2.0 * core::f32::consts::PI);
    // two vectors across the aim
    let mut across = cross(&aim, &[0.0, 0.0, 1.0]);
    if normalize(&mut across) == 0.0 {
        across = [1.0, 0.0, 0.0];
    }
    let up = cross(&across, &aim);
    let mut out =
        add(&scale(&aim, cos_theta), &add(&scale(&across, sin_theta * cos_phi), &scale(&up, sin_theta * sin_phi)));
    if normalize(&mut out) == 0.0 {
        out = aim;
    }
    out
}

// ---------- the tick

/// Advance the items by one tick (1/30 s), at match tick `tick`: spawn what
/// the placements spawn now, take away what has lasted its time, step each
/// item that is falling, apply the pickups (see [`crate::pickups`]; `requests`
/// are the players who pressed the action button this tick) and end the
/// camouflage that has run its time. Does nothing for the spawns once the match
/// is over. The one place where the items and the match meet; the match module
/// calls it after [`crate::rules::play`].
#[allow(clippy::too_many_arguments)]
pub fn tick(
    items: &mut impl ItemStore,
    combat: &mut impl CombatStore,
    store: &impl Store,
    game: &impl GameStore,
    map: &MapData,
    rng: &mut Rng,
    tick: u64,
    requests: &[Request],
) -> Vec<ItemEvent> {
    let mut events = Vec::new();
    let state = game.game();

    // the first tick of a match is time 0
    if state.ending.is_none() && tick > state.started_tick {
        spawn_placements(items, map, rng, tick, tick - state.started_tick - 1, &mut events);
    }

    // what nobody has held for long enough goes; so do the oldest of too many
    // (what is gone is what was removed here: `all` is what the store held, and the store is not
    // asked again for each item of it, which for the server's tables is a call to the host each)
    let mut all = items.items();
    let mut gone: Vec<ItemId> = Vec::new();
    for item in &all {
        if tick > item.last_owned.saturating_add(PURGE_TICKS) {
            items.remove_item(item.id);
            gone.push(item.id);
            events.push(ItemEvent::Purged { item: item.id });
        }
    }
    // (`gone` is in the order of `all`, which is in id order)
    let mut next = 0;
    all.retain(|i| {
        let removed = gone.get(next) == Some(&i.id);
        next += removed as usize;
        !removed
    });
    if all.len() > MAX_ITEMS {
        let mut dropped: Vec<&Item> = all.iter().filter(|i| i.placement == NO_PLACEMENT).collect();
        dropped.sort_by_key(|i| (i.last_owned, i.id));
        let mut too_many: Vec<ItemId> = Vec::new();
        for item in dropped.into_iter().take(all.len() - MAX_ITEMS) {
            items.remove_item(item.id);
            too_many.push(item.id);
            events.push(ItemEvent::Purged { item: item.id });
        }
        all.retain(|i| !too_many.contains(&i.id));
    }

    // every item as it is now: a falling one is stepped a tick
    let mut working: BTreeMap<ItemId, Item> = BTreeMap::new();
    for row in all {
        if row.resting {
            working.insert(row.id, row);
            continue;
        }
        // one put down this very tick has begun to fall at its end, and is not stepped yet
        if row.tick >= tick {
            working.insert(row.id, row);
            continue;
        }
        // (where it was at the end of the last tick: the store's, or from the row)
        let before = match items.flight(row.id) {
            Some(known) if known.tick + 1 >= tick => known,
            _ => row.advanced_to(map, tick - 1).0,
        };
        let mut now = before;
        match now.step(map) {
            Flight::Falling => {
                items.set_flight(now);
                working.insert(now.id, now);
            }
            Flight::Rested => {
                items.clear_flight(now.id);
                items.update_item(now);
                events.push(ItemEvent::Rested { item: now.id });
                working.insert(now.id, now);
            }
            Flight::Lost => {
                items.remove_item(now.id);
                events.push(ItemEvent::Purged { item: now.id });
            }
        }
    }

    pickups::resolve(items, combat, store, game, map, rng, tick, requests, &mut working, &mut events);
    pickups::expire(items, tick, &mut events);
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{flat_floor_map, items_map, PISTOL, RIFLE};
    use crate::math::magnitude;

    fn falling(position: [f32; 3], velocity: [f32; 3]) -> Item {
        Item {
            id: 0,
            tag: PISTOL,
            position,
            velocity,
            tick: 0,
            resting: false,
            placement: NO_PLACEMENT,
            loaded: 12,
            reserve: 48,
            last_owned: 0,
            ignore: NO_PLAYER,
        }
    }

    #[test]
    fn an_item_let_go_of_above_the_floor_falls_with_gravity_and_comes_to_rest_on_it() {
        let map = flat_floor_map();
        let mut item = falling([1.0, 2.0, 1.0], [0.0; 3]);
        let mut ticks = 0;
        // (a free fall of 1 world unit takes about 24 ticks: v = g t, d = g t^2 / 2)
        let mut last_z = item.position[2];
        while item.step(&map) == Flight::Falling {
            ticks += 1;
            assert!(item.position[2] < last_z || item.velocity[2] > 0.0, "it only goes down until it bounces");
            last_z = item.position[2];
            assert!(ticks < 1000, "it comes to rest");
        }
        assert!(item.resting);
        assert_eq!(item.velocity, [0.0; 3]);
        assert!(item.position[2].abs() < 1e-4, "on the floor: {}", item.position[2]);
        assert_eq!((item.position[0], item.position[1]), (1.0, 2.0), "straight down");
        assert!(ticks > 24, "after the free fall of a unit, and a bounce or two");
        assert_eq!(item.advanced_to(&map, 100_000).0, item, "and stays there");
    }

    #[test]
    fn a_fast_item_bounces_off_a_floor_with_0_4_of_its_speed_into_it() {
        let map = flat_floor_map();
        let mut item = falling([0.0, 0.0, 0.1], [0.0, 0.0, -0.2]);
        // one tick: the path ends below the floor
        assert_eq!(item.step(&map), Flight::Falling);
        let into = 0.2 + GRAVITY;
        assert!(
            (item.velocity[2] - 0.4 * into).abs() < 1e-5,
            "it goes back up at 0.4 of the speed: {:?}",
            item.velocity
        );
        assert!((item.position[2] - PUSH_OFF).abs() < 1e-5, "kept 0.05 off the floor: {:?}", item.position);
    }

    #[test]
    fn a_fall_is_the_same_whether_stepped_a_tick_at_a_time_or_worked_out_from_where_it_began() {
        let map = flat_floor_map();
        let begin = falling([3.0, -4.0, 2.0], [0.02, -0.01, 0.03]);
        let mut by_ticks = begin;
        for _ in 0..40 {
            by_ticks.step(&map);
        }
        assert_eq!(begin.advanced_to(&map, 40).0, by_ticks, "what a client works out is what the server has");
    }

    #[test]
    fn an_item_that_falls_out_of_the_map_is_lost() {
        // (a map with no floor under where it is let go)
        let mut map = flat_floor_map();
        map.collision.bsp3d_nodes.clear();
        let mut item = falling([0.0, 0.0, 0.0], [0.0; 3]);
        let mut flight = Flight::Falling;
        for _ in 0..2000 {
            flight = item.step(&map);
            if flight != Flight::Falling {
                break;
            }
        }
        assert_eq!(flight, Flight::Lost);
    }

    #[test]
    fn a_thrown_weapon_leaves_within_the_cone_and_at_the_engines_speed() {
        let map = flat_floor_map();
        let mut items = MemoryItems::new();
        let mut rng = Rng::seeded(7);
        for _ in 0..200 {
            let id = drop_item(
                &mut items,
                &map,
                &mut rng,
                5,
                3,
                [0.0, 0.0, 0.0],
                0.0,
                0.0,
                PISTOL,
                Ammo { loaded: 1, reserve: 2 },
            );
            let item = items.item(id).unwrap();
            let speed = magnitude(&item.velocity);
            assert!((0.0266..=0.0401).contains(&speed), "{speed}");
            // along +x within 22.5 degrees
            assert!(item.velocity[0] / speed >= 0.9238, "{:?}", item.velocity);
            assert_eq!((item.ignore, item.loaded, item.reserve, item.resting), (3, 1, 2, false));
        }
    }

    #[test]
    fn a_placement_makes_an_item_at_the_start_at_its_period_and_the_one_nobody_took_goes_as_it_does() {
        let map = items_map();
        let mut items = MemoryItems::new();
        let mut combat = crate::combat::MemoryCombat::new();
        let store = crate::MemoryStore::new();
        let mut game = crate::rules::MemoryGame::new(crate::rules::Rules::slayer());
        crate::rules::begin(&mut game, 0);
        let mut rng = Rng::seeded(1);
        // the fixture's first placement: a rifle, every 10 seconds (300 ticks)
        let mut spawned_at = Vec::new();
        for t in 1..=1000 {
            let events = tick(&mut items, &mut combat, &store, &game, &map, &mut rng, t, &[]);
            if events.iter().any(|e| matches!(e, ItemEvent::Spawned { tag, .. } if *tag == RIFLE)) {
                spawned_at.push(t);
            }
            assert!(items.items().iter().filter(|i| i.placement == 0).count() <= 1, "one item for a placement");
        }
        // game time 0 is the first tick after the clock began: tick 1; then every 300
        assert_eq!(spawned_at, [1, 301, 601, 901]);
    }

    /// A resting dropped item, last held at `last_owned`.
    fn resting_drop(last_owned: u64) -> Item {
        Item { resting: true, last_owned, ..falling([0.0, 0.0, 0.0], [0.0; 3]) }
    }

    fn tick_of(items: &mut MemoryItems, tick: u64) -> Vec<ItemEvent> {
        let map = flat_floor_map();
        let mut combat = crate::combat::MemoryCombat::new();
        let store = crate::MemoryStore::new();
        let mut game = crate::rules::MemoryGame::new(crate::rules::Rules::slayer());
        crate::rules::begin(&mut game, 0);
        super::tick(items, &mut combat, &store, &game, &map, &mut Rng::seeded(1), tick, &[])
    }

    #[test]
    fn an_item_nobody_has_held_for_thirty_seconds_goes_and_the_rest_stay() {
        let mut items = MemoryItems::new();
        // held at ticks 100, 5000, 200, 6000, 300: at tick 1000 + PURGE_TICKS + 250 those held before 1250 are old
        let ids: Vec<ItemId> =
            [100, 5000, 200, 6000, 300].iter().map(|t| items.insert_item(resting_drop(*t))).collect();
        let at = PURGE_TICKS + 250;
        let events = tick_of(&mut items, at);
        let purged: Vec<ItemId> =
            events.iter().filter_map(|e| if let ItemEvent::Purged { item } = e { Some(*item) } else { None }).collect();
        assert_eq!(purged, [ids[0], ids[2]], "the two held at 100 and 200, in id order");
        assert_eq!(items.items().iter().map(|i| i.id).collect::<Vec<_>>(), [ids[1], ids[3], ids[4]]);
    }

    #[test]
    fn beyond_the_most_items_the_longest_unheld_dropped_ones_go() {
        let mut items = MemoryItems::new();
        // MAX_ITEMS + 3 drops, the first three put down latest (held last at the highest ticks): so the
        // ones that go are the three held longest ago, not the first three made
        let mut ids = Vec::new();
        for i in 0..MAX_ITEMS + 3 {
            let last_owned = if i < 3 { 1_000 + i as u64 } else { 10 + i as u64 };
            ids.push(items.insert_item(resting_drop(last_owned)));
        }
        let events = tick_of(&mut items, 20);
        let purged: Vec<ItemId> =
            events.iter().filter_map(|e| if let ItemEvent::Purged { item } = e { Some(*item) } else { None }).collect();
        assert_eq!(purged, [ids[3], ids[4], ids[5]], "the three with the least last_owned");
        assert_eq!(items.items().len(), MAX_ITEMS);
    }
}
