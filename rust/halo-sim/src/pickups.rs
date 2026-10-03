//! Pickups: who takes which item, and what it does for them. The engine's own
//! (`source/game/players.c`: `player_examine_nearby_item`,
//! `player_handle_powerup_equipment`, `player_handle_weapon_swap`;
//! `source/items/weapons.c`: `weapon_handle_potential_inventory_item`;
//! `source/units/units.c`: `unit_add_weapon_to_inventory`,
//! `unit_approve_weapon_swap`) are the reference; [`crate::items`] has the
//! items themselves, and [`resolve`] is run by its [`crate::items::tick`].
//!
//! A client's engine decides nothing here: it says only that its player
//! pressed the action button ([`Request`], with the weapon slot the player
//! has in hand), and the server decides what the player takes, from where the
//! players and the items are.
//!
//! # What reaches an item
//!
//! A player reaches an item when the bounding sphere of the player's biped
//! and the item's touch (`objects_in_sphere`): the tags' radii, summed
//! ([`halo_map::items::Reach`], [`halo_map::items::ItemDef`]).
//!
//! # What a player takes
//!
//! - **A weapon they hold** (same tag): the rounds of the item, up to the
//!   weapon's most in reserve, without pressing anything; the item is gone
//!   when it has none left. A player at the most takes nothing.
//! - **A powerup**, without pressing anything: an overshield (not if the
//!   shield is beyond full already: the shield is overcharged,
//!   [`crate::damage::Vitals::overcharge`]), a health pack (not if health is
//!   full: it is made full) or active camouflage (not while already
//!   camouflaged: it lasts the tag's `powerup_time`). A player who is not
//!   given it leaves it where it is.
//! - **A weapon**, with no weapon in hand, without pressing anything. With one
//!   (of another kind) the action button takes it as the second; with two it
//!   swaps it for the one in the slot the player has in hand, which falls to
//!   the ground. A player who holds a weapon of the kind cannot swap for it.
//!
//! # Who gets it
//!
//! Every pair of a player in reach of an item is considered, nearest first
//! (ties by the lower item and then the lower player), and each is applied
//! against the state the ones before it left. So an item goes to the nearest
//! player who can take it, and exactly one gets it; two players equally near
//! the same item on the same tick are told apart by their ids, and a player
//! who can take two things (a health pack and an overshield) takes both. A
//! player's action button takes one weapon a tick.
//!
//! Nothing here is the client's to decide: a dead player takes nothing, and a
//! player who put a weapon down cannot take it until it has come to rest.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use halo_map::items::powerup;

use crate::combat::{CombatStore, Loadout, NO_WEAPON};
use crate::items::{drop_item, Ammo, Item, ItemId, ItemStore, Kit};
use crate::map::MapData;
use crate::math::{sin_cos, Vec3};
use crate::rng::Rng;
use crate::rules::GameStore;
use crate::state::{Player, PlayerId, Store};
use crate::TICKS_PER_SECOND;

/// A player pressed the action button this tick: the weapon slot they have in
/// hand (0 or 1) is what a swap puts down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request {
    pub player: PlayerId,
    pub slot: u8,
}

/// What a player took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pickup {
    /// A weapon, into this slot of the loadout.
    Weapon {
        slot: u8,
    },
    /// The rounds of a weapon of a kind they hold, into this slot's reserve.
    Ammo {
        slot: u8,
        rounds: i16,
    },
    Overshield,
    Camouflage,
    Health,
}

/// What the items did in a tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ItemEvent {
    /// A placement made an item.
    Spawned {
        item: ItemId,
        tag: u16,
        position: [f32; 3],
    },
    /// An item that was falling came to rest.
    Rested {
        item: ItemId,
    },
    /// An item is gone: nobody held it for 30 seconds, the next of its
    /// placement was made, it fell out of the map, or there were too many.
    Purged {
        item: ItemId,
    },
    PickedUp {
        player: PlayerId,
        item: ItemId,
        tag: u16,
        what: Pickup,
    },
    /// A player put a weapon down, as this item.
    Dropped {
        player: PlayerId,
        item: ItemId,
        tag: u16,
    },
    /// A player's active camouflage ran out.
    CamouflageEnded {
        player: PlayerId,
    },
}

/// The most rounds a weapon holds in reserve, and in its magazine.
fn limits(map: &MapData, tag: u16) -> (i16, i16) {
    map.combat
        .weapon(tag)
        .and_then(|w| w.magazines.first())
        .map_or((0, 0), |m| (m.rounds_loaded_maximum, m.rounds_total_maximum))
}

fn is_alive(game: &impl GameStore, combat: &impl CombatStore, id: PlayerId) -> bool {
    game.contestant(id).is_some_and(|c| c.is_alive()) && combat.fighter(id).is_some_and(|f| !f.vitals.is_dead())
}

/// A player's bounding sphere's centre: the biped's offset, turned with the
/// way the player faces.
fn player_center(map: &MapData, player: &Player) -> Vec3 {
    let o = map.items.player.bounding_offset;
    let (s, c) = sin_cos(player.yaw);
    [player.position[0] + o[0] * c - o[1] * s, player.position[1] + o[0] * s + o[1] * c, player.position[2] + o[2]]
}

/// What a player takes, if anything, and the tick's other details.
struct Tick<'a, I: ItemStore, C: CombatStore> {
    items: &'a mut I,
    combat: &'a mut C,
    map: &'a MapData,
    rng: &'a mut Rng,
    tick: u64,
    working: &'a mut BTreeMap<ItemId, Item>,
    events: &'a mut Vec<ItemEvent>,
}

impl<I: ItemStore, C: CombatStore> Tick<'_, I, C> {
    fn take(&mut self, item: &Item) {
        self.items.remove_item(item.id);
        self.working.remove(&item.id);
    }

    fn put(&mut self, item: Item) {
        self.items.update_item(item);
        self.working.insert(item.id, item);
    }

    /// Whether `player` takes `item`, and what it does. `asked` is the weapon
    /// slot of the player's action button, while they have not used it.
    fn attempt(&mut self, player: &Player, item: Item, asked: &mut BTreeMap<PlayerId, u8>) {
        let Some(def) = self.map.items.def(item.tag) else { return };
        let Some(mut fighter) = self.combat.fighter(player.id) else { return };
        let mut kit = self.items.kit(player.id);
        let tick = self.tick;
        if def.is_weapon {
            let weapons = fighter.loadout.weapons;
            // the same kind: its rounds, and nothing else (the engine will not swap for one held)
            if let Some(slot) = (0..2).find(|s| weapons[*s] == item.tag) {
                let (_, most) = limits(self.map, item.tag);
                let held = kit.ammo[slot].reserve;
                if held >= most {
                    return;
                }
                let rounds = item.reserve.min(most - held).max(0);
                if rounds > 0 {
                    kit.ammo[slot].reserve = held + rounds;
                    kit.version += 1;
                    self.items.set_kit(kit);
                    let left = item.reserve - rounds;
                    if left == 0 {
                        self.take(&item);
                    } else {
                        self.put(Item { reserve: left, ..item });
                    }
                    self.events.push(ItemEvent::PickedUp {
                        player: player.id,
                        item: item.id,
                        tag: item.tag,
                        what: Pickup::Ammo { slot: slot as u8, rounds },
                    });
                }
                return;
            }
            let carried = weapons.iter().filter(|w| **w != NO_WEAPON).count();
            let slot = match carried {
                // nothing in hand: it is taken at once
                0 => 0,
                _ => {
                    let Some(&hand) = asked.get(&player.id) else { return };
                    asked.remove(&player.id);
                    match weapons.iter().position(|w| *w == NO_WEAPON) {
                        // a free slot: a second weapon
                        Some(free) => free,
                        // both full: the one in hand goes
                        None => {
                            let slot = (hand & 1) as usize;
                            let drop_tag = weapons[slot];
                            let dropped = drop_item(
                                self.items,
                                self.map,
                                self.rng,
                                tick,
                                player.id,
                                player.position,
                                player.yaw,
                                player.pitch,
                                drop_tag,
                                kit.ammo[slot],
                            );
                            fighter.loadout.drop_weapon(drop_tag, tick);
                            self.events.push(ItemEvent::Dropped { player: player.id, item: dropped, tag: drop_tag });
                            slot
                        }
                    }
                }
            };
            fighter.loadout.weapons[slot] = item.tag;
            kit.ammo[slot] = Ammo { loaded: item.loaded, reserve: item.reserve };
            kit.version += 1;
            self.combat.set_fighter(fighter);
            self.items.set_kit(kit);
            self.take(&item);
            self.events.push(ItemEvent::PickedUp {
                player: player.id,
                item: item.id,
                tag: item.tag,
                what: Pickup::Weapon { slot: slot as u8 },
            });
            return;
        }

        // an equipment: a powerup that lasts, or does something at once (`player_handle_powerup_equipment`)
        let duration = (def.powerup_time * TICKS_PER_SECOND as f32) as i32;
        if duration <= 0 {
            return;
        }
        let map = self.map;
        let what = match def.powerup_type {
            powerup::OVERSHIELD => {
                let mut vitals = fighter.vitals_at(map, tick);
                if !vitals.overcharge() {
                    return;
                }
                fighter.vitals = vitals;
                fighter.tick = tick;
                self.combat.set_fighter(fighter);
                Pickup::Overshield
            }
            powerup::HEALTH => {
                let mut vitals = fighter.vitals_at(map, tick);
                if vitals.is_dead() || vitals.body >= 1.0 {
                    return;
                }
                vitals.body = 1.0;
                fighter.vitals = vitals;
                fighter.tick = tick;
                self.combat.set_fighter(fighter);
                Pickup::Health
            }
            powerup::ACTIVE_CAMOUFLAGE => {
                if kit.is_camouflaged(tick) {
                    return;
                }
                kit.camo_until = tick + duration as u64;
                self.items.set_kit(kit);
                Pickup::Camouflage
            }
            _ => return,
        };
        self.take(&item);
        self.events.push(ItemEvent::PickedUp { player: player.id, item: item.id, tag: item.tag, what });
    }
}

/// Apply the tick's pickups to `working`, the items as they are now (which
/// the store has as of the tick's start, falling ones aside): see the module's
/// rules. An item that is taken goes from the store and from `working`.
#[allow(clippy::too_many_arguments)]
pub fn resolve(
    items: &mut impl ItemStore,
    combat: &mut impl CombatStore,
    store: &impl Store,
    game: &impl GameStore,
    map: &MapData,
    rng: &mut Rng,
    tick: u64,
    requests: &[Request],
    working: &mut BTreeMap<ItemId, Item>,
    events: &mut Vec<ItemEvent>,
) {
    if working.is_empty() {
        return;
    }
    // the players, by how far along x they are, so that an item looks only at those near it
    let mut bodies: Vec<(f32, Player)> = store.players().into_iter().map(|p| (player_center(map, &p)[0], p)).collect();
    if bodies.is_empty() {
        return;
    }
    bodies.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.id.cmp(&b.1.id)));

    // every pair of a player and an item they reach
    let mut pairs: Vec<(f32, ItemId, usize)> = Vec::new();
    for item in working.values() {
        let Some(def) = map.items.def(item.tag) else { continue };
        let center = [
            item.position[0] + def.bounding_offset[0],
            item.position[1] + def.bounding_offset[1],
            item.position[2] + def.bounding_offset[2],
        ];
        let reach = map.items.player.bounding_radius + def.bounding_radius;
        let from = bodies.partition_point(|b| b.0 < center[0] - reach);
        for (index, (x, player)) in bodies.iter().enumerate().skip(from) {
            if *x > center[0] + reach {
                break;
            }
            let c = player_center(map, player);
            let d = [c[0] - center[0], c[1] - center[1], c[2] - center[2]];
            let distance_squared = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
            if distance_squared <= reach * reach {
                pairs.push((distance_squared, item.id, index));
            }
        }
    }
    if pairs.is_empty() {
        return;
    }
    pairs.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).then(bodies[a.2].1.id.cmp(&bodies[b.2].1.id)));

    let mut asked: BTreeMap<PlayerId, u8> = BTreeMap::new();
    for r in requests {
        asked.entry(r.player).or_insert(r.slot);
    }
    let mut alive: BTreeMap<PlayerId, bool> = BTreeMap::new();
    let mut run = Tick { items, combat, map, rng, tick, working, events };
    for (_, item_id, index) in pairs {
        let Some(item) = run.working.get(&item_id).copied() else { continue };
        let player = &bodies[index].1;
        if item.ignore == player.id {
            continue;
        }
        let living = *alive.entry(player.id).or_insert_with(|| is_alive(game, &*run.combat, player.id));
        if living {
            run.attempt(player, item, &mut asked);
        }
    }
}

/// End the camouflage that has run its time.
pub fn expire(items: &mut impl ItemStore, tick: u64, events: &mut Vec<ItemEvent>) {
    for player in items.camouflaged() {
        let mut kit = items.kit(player);
        if kit.camo_until != 0 && tick >= kit.camo_until {
            kit.camo_until = 0;
            items.set_kit(kit);
            events.push(ItemEvent::CamouflageEnded { player });
        }
    }
}

/// A player has spawned: the rounds of the weapons they carry are the tags'
/// initial ones, and they have no camouflage (the engine zeroes a spawned
/// player's powerups).
pub fn on_spawn(items: &mut impl ItemStore, map: &MapData, player: PlayerId, loadout: &Loadout) {
    let mut kit = items.kit(player);
    kit.ammo = [Ammo::default(); 2];
    for (slot, tag) in loadout.weapons.iter().enumerate() {
        if *tag != NO_WEAPON {
            let (loaded, reserve) = crate::items::initial_rounds(map, *tag);
            kit.ammo[slot] = Ammo { loaded, reserve };
        }
    }
    kit.camo_until = 0;
    kit.version += 1;
    items.set_kit(kit);
}

/// A player has died: every weapon they carried falls where they were (each
/// is still theirs for a while, for the shot in flight), and they are no longer
/// camouflaged.
#[allow(clippy::too_many_arguments)]
pub fn on_death(
    items: &mut impl ItemStore,
    combat: &mut impl CombatStore,
    store: &impl Store,
    map: &MapData,
    rng: &mut Rng,
    tick: u64,
    player: PlayerId,
    events: &mut Vec<ItemEvent>,
) {
    let (Some(mut fighter), Some(body)) = (combat.fighter(player), store.player(player)) else { return };
    let mut kit = items.kit(player);
    for slot in 0..2 {
        let tag = fighter.loadout.weapons[slot];
        if tag == NO_WEAPON {
            continue;
        }
        let item = drop_item(items, map, rng, tick, player, body.position, body.yaw, body.pitch, tag, kit.ammo[slot]);
        fighter.loadout.drop_weapon(tag, tick);
        events.push(ItemEvent::Dropped { player, item, tag });
    }
    combat.set_fighter(fighter);
    kit.ammo = [Ammo::default(); 2];
    kit.camo_until = 0;
    items.set_kit(kit);
}

/// What the player's client says it holds, for the rounds the server tracks
/// (the client fires its weapon and counts them; the server needs them for
/// what a swap puts down and for how many an ammunition pickup can give):
/// kept within the weapons' own limits, for no weapon in a slot none. Does
/// not count as the server changing them (the kit's version is not changed).
pub fn report_ammo(
    items: &mut impl ItemStore,
    combat: &impl CombatStore,
    map: &MapData,
    player: PlayerId,
    ammo: [Ammo; 2],
) -> bool {
    let Some(fighter) = combat.fighter(player) else { return false };
    let mut kit = items.kit(player);
    for (slot, a) in ammo.iter().enumerate() {
        let tag = fighter.loadout.weapons[slot];
        kit.ammo[slot] = if tag == NO_WEAPON {
            Ammo::default()
        } else {
            let (loaded_most, total_most) = limits(map, tag);
            Ammo { loaded: a.loaded.clamp(0, loaded_most), reserve: a.reserve.clamp(0, total_most) }
        };
    }
    items.set_kit(kit);
    true
}

/// Where a kit says a player is camouflaged: for a client that shows it.
pub fn is_camouflaged(kit: &Kit, tick: u64) -> bool {
    kit.is_camouflaged(tick)
}
