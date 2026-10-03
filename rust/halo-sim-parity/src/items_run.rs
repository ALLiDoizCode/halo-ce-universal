//! The items' scenario: a crowd standing about on a floor with the fixtures'
//! placements, reaching for what the placements make, pressing the action
//! button, being hurt, dying and spawning again, putting weapons down that fall
//! and come to rest, picking up what they find. The scenario chains every tick's
//! events, items and fighters into a hash, so that any difference between the
//! native and WebAssembly builds shows (see the crate's docs).

use halo_sim::combat::{CombatStore, Fighter, Loadout, MemoryCombat};
use halo_sim::damage::Vitals;
use halo_sim::fixtures::{items_map, PISTOL};
use halo_sim::items::{self, snapshot_items, Ammo, MemoryItems};
use halo_sim::pickups::{self, ItemEvent, Request};
use halo_sim::rules::{self, GameStore, Life, MemoryGame, Rules};
use halo_sim::{Player, PlayerId, Rng, Store};

use crate::Fnv;

pub const ITEM_PLAYERS: u16 = 24;

/// Kinds of thing [`run_items`] counts, in the order of [`item_counts`]: items made, items come to
/// rest, items taken away, picked up (weapon, rounds, overshield, camouflage, health), put down,
/// camouflage ended.
pub const ITEM_KINDS: usize = 11;

fn kind_of(event: &ItemEvent) -> usize {
    match event {
        ItemEvent::Spawned { .. } => 0,
        ItemEvent::Rested { .. } => 1,
        ItemEvent::Purged { .. } => 2,
        ItemEvent::PickedUp { what, .. } => match what {
            pickups::Pickup::Weapon { .. } => 3,
            pickups::Pickup::Ammo { .. } => 4,
            pickups::Pickup::Overshield => 5,
            pickups::Pickup::Camouflage => 6,
            pickups::Pickup::Health => 7,
        },
        ItemEvent::Dropped { .. } => 8,
        ItemEvent::CamouflageEnded { .. } => 9,
    }
}

/// Play the scenario for `ticks` ticks; the result is the final items and kits, a hash chaining every
/// tick, and the counts of [`item_counts`].
pub fn run_items(seed: u64, ticks: u32) -> Vec<u8> {
    let map = items_map();
    let mut rng = Rng::seeded(seed);
    let mut store = halo_sim::MemoryStore::new();
    let mut game = MemoryGame::new(Rules { respawn_ticks: 0, ..Rules::slayer() });
    let mut combat = MemoryCombat::new();
    let mut items = MemoryItems::new();
    rules::begin(&mut game, 0);
    let spots: Vec<[f32; 3]> = map.items.placements.iter().map(|p| p.position).collect();
    let ids: Vec<PlayerId> = (0..ITEM_PLAYERS).collect();
    for &id in &ids {
        let at = spots[id as usize % spots.len()];
        store.set_player(Player::new(id, [at[0], at[1], 0.0], 0.0, 0.0));
        rules::enter_placed(&mut game, id, (id % 2) as u8, [at[0], at[1], 0.0], 0.0);
        let loadout = Loadout::with(PISTOL);
        combat.set_fighter(Fighter {
            id,
            vitals: Vitals::full(&map.combat.resistance),
            tick: 0,
            loadout,
            hurt_tick: 0,
            hurt_by: PlayerId::MAX,
            hurt_count: 0,
        });
        pickups::on_spawn(&mut items, &map, id, &loadout);
    }

    let mut chain = Fnv(0xCBF2_9CE4_8422_2325);
    let mut counts = [0u32; ITEM_KINDS];
    for tick in 1..=ticks as u64 {
        // the crowd moves about among the placements: a step, or to a new spot
        for &id in &ids {
            let mut p = store.player(id).unwrap();
            if rng.next_u32().is_multiple_of(25) {
                let at = spots[rng.next_u32() as usize % spots.len()];
                p.position = [at[0] + rng.next_f32() - 0.5, at[1] + rng.next_f32() - 0.5, 0.0];
            }
            p.yaw = rng.next_f32() * 6.0 - 3.0;
            p.pitch = rng.next_f32() * 0.6 - 0.3;
            store.set_player(p);
        }
        let mut events = Vec::new();
        // deaths, with the weapons they put down, and respawns
        for &id in &ids {
            let c = game.contestant(id).unwrap();
            match c.life {
                Life::Alive if rng.next_u32().is_multiple_of(1200) => {
                    pickups::on_death(&mut items, &mut combat, &store, &map, &mut rng, tick, id, &mut events);
                    let mut f = combat.fighter(id).unwrap();
                    f.vitals.flags |= halo_sim::damage::DEAD;
                    combat.set_fighter(f);
                    let mut c = c;
                    c.life = Life::Dead { due: tick + 60 };
                    game.set_contestant(c);
                }
                Life::Dead { due } if due <= tick => {
                    let mut c = c;
                    c.life = Life::Alive;
                    game.set_contestant(c);
                    let loadout = Loadout::with(PISTOL);
                    combat.set_fighter(Fighter {
                        id,
                        vitals: Vitals::full(&map.combat.resistance),
                        tick,
                        loadout,
                        hurt_tick: 0,
                        hurt_by: PlayerId::MAX,
                        hurt_count: 0,
                    });
                    pickups::on_spawn(&mut items, &map, id, &loadout);
                }
                Life::Alive => {
                    // hurt now and then (so that a health pack has something to heal), and say how many rounds are left
                    if rng.next_u32().is_multiple_of(50) {
                        let mut f = combat.fighter(id).unwrap();
                        let mut v = f.vitals_at(&map, tick);
                        v.body = 0.4 + rng.next_f32() * 0.5;
                        v.shield = 0.0;
                        f.vitals = v;
                        f.tick = tick;
                        combat.set_fighter(f);
                    }
                    if rng.next_u32().is_multiple_of(60) {
                        let r = |rng: &mut Rng| (rng.next_u32() % 40) as i16;
                        let ammo = [Ammo { loaded: r(&mut rng), reserve: r(&mut rng) }, Ammo { loaded: 1, reserve: 2 }];
                        pickups::report_ammo(&mut items, &combat, &map, id, ammo);
                    }
                }
                _ => {}
            }
        }
        // the action button, now and then
        let mut requests: Vec<Request> = Vec::new();
        for &player in &ids {
            if rng.next_u32().is_multiple_of(9) {
                requests.push(Request { player, slot: (rng.next_u32() % 2) as u8 });
            }
        }
        events.extend(items::tick(&mut items, &mut combat, &store, &game, &map, &mut rng, tick, &requests));
        for event in &events {
            counts[kind_of(event)] += 1;
            chain.bytes(format!("{event:?}").as_bytes());
        }
        chain.bytes(&snapshot_items(&items, &ids));
        for &id in &ids {
            chain.bytes(format!("{:?}", combat.fighter(id)).as_bytes());
        }
    }

    let mut out = snapshot_items(&items, &ids);
    out.extend_from_slice(&chain.0.to_le_bytes());
    for c in counts {
        out.extend_from_slice(&c.to_le_bytes());
    }
    out
}

/// How many of each kind of thing [`run_items`] had (see [`ITEM_KINDS`]).
pub fn item_counts(output: &[u8]) -> [u32; ITEM_KINDS] {
    let tail = &output[output.len() - ITEM_KINDS * 4..];
    std::array::from_fn(|i| u32::from_le_bytes(tail[i * 4..i * 4 + 4].try_into().unwrap()))
}

/// For the WebAssembly host: run [`run_items`] and return the length of the
/// result, which [`crate::parity_output`] points to.
#[no_mangle]
pub extern "C" fn parity_items_run(seed_low: u32, seed_high: u32, ticks: u32) -> u32 {
    let bytes = run_items(seed_low as u64 | (seed_high as u64) << 32, ticks).into_boxed_slice();
    let len = bytes.len() as u32;
    crate::OUTPUT.store(Box::leak(bytes).as_ptr() as usize, std::sync::atomic::Ordering::SeqCst);
    len
}
