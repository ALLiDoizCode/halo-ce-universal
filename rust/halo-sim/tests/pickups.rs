//! Items and pickups at the simulation step: a world of a flat floor, players
//! who stand where the test puts them, and `halo_sim::items::tick` called a
//! tick at a time, asserting on what the stores say after it and on the events
//! it returns. The rules are the engine's (see `halo_sim::pickups`); the
//! numbers are the fixtures', which are the tags' of Blood Gulch where it
//! matters (a shield for 60 seconds, camouflage for 45).

use halo_sim::combat::{CombatStore, Fighter, Loadout, MemoryCombat, NO_WEAPON};
use halo_sim::damage::{Vitals, DEAD, SHIELD_OVER_CHARGING};
use halo_sim::fixtures::{
    item_defs, items_map, placement, with_placements, CAMOUFLAGE, FRAG_GRENADE, HEALTH_PACK, OVERSHIELD, PISTOL, RIFLE,
};
use halo_sim::items::{self, Ammo, Item, ItemId, ItemStore, MemoryItems, NO_PLACEMENT, NO_PLAYER, PURGE_TICKS};
use halo_sim::pickups::{self, ItemEvent, Pickup, Request};
use halo_sim::rules::{self, GameStore, MemoryGame, Rules};
use halo_sim::{MapData, MemoryStore, Player, PlayerId, Rng, Store, TICKS_PER_SECOND};

struct World {
    map: MapData,
    items: MemoryItems,
    combat: MemoryCombat,
    store: MemoryStore,
    game: MemoryGame,
    rng: Rng,
    tick: u64,
}

impl World {
    /// A floor with no placements: items are put where the test says.
    fn bare() -> World {
        World::on(with_placements(items_map(), &[]))
    }

    fn on(map: MapData) -> World {
        let mut game = MemoryGame::new(Rules::slayer());
        rules::begin(&mut game, 0);
        World {
            map,
            items: MemoryItems::new(),
            combat: MemoryCombat::new(),
            store: MemoryStore::new(),
            game,
            rng: Rng::seeded(11),
            tick: 0,
        }
    }

    /// A player standing at `(x, y)` on the floor, alive, with the pistol.
    fn add(&mut self, id: PlayerId, x: f32, y: f32) {
        self.store.set_player(Player::new(id, [x, y, 0.0], 0.0, 0.0));
        rules::enter_placed(&mut self.game, id, (id % 2) as u8, [x, y, 0.0], 0.0);
        let loadout = Loadout::with(PISTOL);
        self.combat.set_fighter(Fighter {
            id,
            vitals: Vitals::full(&self.map.combat.resistance),
            tick: self.tick,
            loadout,
            hurt_tick: 0,
            hurt_by: PlayerId::MAX,
            hurt_count: 0,
        });
        pickups::on_spawn(&mut self.items, &self.map, id, &loadout);
    }

    /// A player with no weapon.
    fn add_unarmed(&mut self, id: PlayerId, x: f32, y: f32) {
        self.add(id, x, y);
        self.set_weapons(id, [NO_WEAPON; 2]);
    }

    fn set_weapons(&mut self, id: PlayerId, weapons: [u16; 2]) {
        let mut f = self.fighter(id);
        f.loadout.weapons = weapons;
        self.combat.set_fighter(f);
        pickups::on_spawn(&mut self.items, &self.map, id, &f.loadout);
    }

    fn fighter(&self, id: PlayerId) -> Fighter {
        self.combat.fighter(id).unwrap()
    }

    /// An item lying on the floor at `(x, y)`.
    fn put(&mut self, tag: u16, x: f32, y: f32) -> ItemId {
        let (loaded, reserve) = items::initial_rounds(&self.map, tag);
        self.items.insert_item(Item {
            id: 0,
            tag,
            position: [x, y, 0.0],
            velocity: [0.0; 3],
            tick: self.tick,
            resting: true,
            placement: NO_PLACEMENT,
            loaded,
            reserve,
            last_owned: self.tick,
            ignore: NO_PLAYER,
        })
    }

    fn step(&mut self, requests: &[Request]) -> Vec<ItemEvent> {
        self.tick += 1;
        items::tick(
            &mut self.items,
            &mut self.combat,
            &self.store,
            &self.game,
            &self.map,
            &mut self.rng,
            self.tick,
            requests,
        )
    }

    fn run(&mut self, ticks: u64) {
        for _ in 0..ticks {
            self.step(&[]);
        }
    }

    fn loose(&self) -> Vec<Item> {
        self.items.items()
    }

    fn weapons(&self, id: PlayerId) -> [u16; 2] {
        self.fighter(id).loadout.weapons
    }

    fn vitals(&self, id: PlayerId) -> Vitals {
        self.fighter(id).vitals_at(&self.map, self.tick)
    }

    fn kill(&mut self, id: PlayerId) -> Vec<ItemEvent> {
        let mut events = Vec::new();
        pickups::on_death(
            &mut self.items,
            &mut self.combat,
            &self.store,
            &self.map,
            &mut self.rng,
            self.tick,
            id,
            &mut events,
        );
        let mut f = self.fighter(id);
        f.vitals.flags |= DEAD;
        self.combat.set_fighter(f);
        let mut c = self.game.contestant(id).unwrap();
        c.life = rules::Life::Dead { due: self.tick + 90 };
        self.game.set_contestant(c);
        events
    }
}

fn press(player: PlayerId, slot: u8) -> Request {
    Request { player, slot }
}

fn picked(events: &[ItemEvent]) -> Vec<(PlayerId, Pickup)> {
    events
        .iter()
        .filter_map(|e| match e {
            ItemEvent::PickedUp { player, what, .. } => Some((*player, *what)),
            _ => None,
        })
        .collect()
}

// ---------- two players, one item

#[test]
fn two_players_reaching_for_one_item_on_the_same_tick_exactly_one_gets_it_the_nearer() {
    let mut w = World::bare();
    w.add(1, 0.2, 0.0);
    w.add(2, -0.35, 0.0);
    w.put(OVERSHIELD, 0.0, 0.0);
    let events = w.step(&[]);
    assert_eq!(picked(&events), [(1, Pickup::Overshield)]);
    assert!(w.loose().is_empty(), "the item is gone");
    assert_ne!(w.vitals(1).flags & SHIELD_OVER_CHARGING, 0);
    assert_eq!(w.vitals(2).flags & SHIELD_OVER_CHARGING, 0, "the other got nothing");
}

#[test]
fn players_equally_near_the_same_item_are_told_apart_by_their_ids_and_not_by_the_order_they_are_in() {
    for (a, b) in [(3, 7), (7, 3)] {
        let mut w = World::bare();
        // (the same distance from the item, on either side)
        w.add(a, 0.3, 0.0);
        w.add(b, -0.3, 0.0);
        w.put(CAMOUFLAGE, 0.0, 0.0);
        let events = w.step(&[]);
        assert_eq!(picked(&events), [(3, Pickup::Camouflage)], "the lower id wins: {a} then {b} added");
    }
}

#[test]
fn however_many_reach_for_an_item_one_gets_it_and_never_two() {
    for crowd in 2..=30u16 {
        let mut w = World::bare();
        for p in 0..crowd {
            // (all within reach: 0.5 of the item, around it)
            let angle = p as f32 * 0.7;
            w.add(p, 0.4 * angle.cos(), 0.4 * angle.sin());
        }
        w.put(HEALTH_PACK, 0.0, 0.0);
        // a health pack heals only the hurt: everyone is
        for p in 0..crowd {
            let mut f = w.fighter(p);
            f.vitals.body = 0.2;
            w.combat.set_fighter(f);
        }
        let events = w.step(&[]);
        assert_eq!(picked(&events).len(), 1, "{crowd} players, one item");
        let healed = (0..crowd).filter(|p| w.vitals(*p).body == 1.0).count();
        assert_eq!(healed, 1);
    }
}

#[test]
fn two_players_who_both_ask_for_a_weapon_to_swap_for_one_get_one_of_it_and_the_other_keeps_theirs() {
    let mut w = World::bare();
    w.add(1, 0.1, 0.0);
    w.add(2, -0.3, 0.0);
    w.set_weapons(1, [PISTOL, PISTOL + 1]);
    w.set_weapons(2, [PISTOL, PISTOL + 1]);
    w.put(RIFLE, 0.0, 0.0);
    let events = w.step(&[press(1, 0), press(2, 0)]);
    assert_eq!(picked(&events), [(1, Pickup::Weapon { slot: 0 })]);
    assert_eq!(w.weapons(1), [RIFLE, PISTOL + 1]);
    assert_eq!(w.weapons(2), [PISTOL, PISTOL + 1], "the other player's weapons are as they were");
    // the weapon that was put down is on the ground, and one item is the whole of what is there
    let ground = w.loose();
    assert_eq!(ground.len(), 1);
    assert_eq!(ground[0].tag, PISTOL);
}

// ---------- weapons

#[test]
fn a_player_with_no_weapon_picks_up_one_in_reach_and_not_one_out_of_it() {
    let mut w = World::bare();
    w.add_unarmed(1, 0.0, 0.0);
    // (a rifle's reach is 0.42 + 0.6)
    let near = w.put(RIFLE, 0.9, 0.0);
    let far = w.put(RIFLE, 1.2, 0.0);
    let events = w.step(&[]);
    assert_eq!(picked(&events), [(1, Pickup::Weapon { slot: 0 })]);
    assert_eq!(w.weapons(1), [RIFLE, NO_WEAPON]);
    assert!(w.items.item(near).is_none());
    assert!(w.items.item(far).is_some(), "out of reach: still there");
    // with the rounds the weapon had
    let kit = w.items.kit(1);
    assert_eq!(kit.ammo[0], Ammo { loaded: 32, reserve: 64 });
}

#[test]
fn a_player_with_a_weapon_needs_the_action_button_to_take_another_and_has_it_as_the_second() {
    let mut w = World::bare();
    w.add(1, 0.0, 0.0);
    w.put(RIFLE, 0.1, 0.0);
    assert!(picked(&w.step(&[])).is_empty(), "walking over it does nothing");
    assert_eq!(w.weapons(1), [PISTOL, NO_WEAPON]);
    let events = w.step(&[press(1, 0)]);
    assert_eq!(picked(&events), [(1, Pickup::Weapon { slot: 1 })]);
    assert_eq!(w.weapons(1), [PISTOL, RIFLE]);
    assert!(w.loose().is_empty(), "nothing was put down");
}

#[test]
fn a_player_with_two_weapons_swaps_the_one_in_hand_which_falls_to_the_ground_with_its_rounds() {
    let mut w = World::bare();
    w.add(1, 0.0, 0.0);
    w.set_weapons(1, [PISTOL, RIFLE]);
    w.items.set_kit({
        let mut k = w.items.kit(1);
        k.ammo[1] = Ammo { loaded: 5, reserve: 7 };
        k
    });
    // a pistol is all there is to pick up, and the player holds one: use the second kind
    w.map.items.defs.push(halo_map::items::ItemDef { tag_index: 900, ..item_defs()[0].clone() });
    w.map.combat.weapons.push({
        let mut shotgun = halo_sim::fixtures::rifle();
        shotgun.tag_index = 900;
        shotgun
    });
    w.put(900, 0.1, 0.0);
    let events = w.step(&[press(1, 1)]);
    assert!(events.iter().any(|e| matches!(e, ItemEvent::Dropped { player: 1, tag, .. } if *tag == RIFLE)));
    assert_eq!(w.weapons(1), [PISTOL, 900], "the weapon in hand (slot 1) was swapped");
    let ground = w.loose();
    assert_eq!(ground.len(), 1);
    assert_eq!((ground[0].tag, ground[0].loaded, ground[0].reserve), (RIFLE, 5, 7), "with the rounds it had");
    assert!(!ground[0].resting, "it falls");
    assert_eq!(ground[0].ignore, 1, "and the player who put it down cannot take it until it rests");
    // and the player still owns the weapon they put down for the shots that are in flight
    assert!(w.fighter(1).loadout.owns(RIFLE, w.tick));
}

#[test]
fn a_weapon_of_a_kind_the_player_holds_gives_its_rounds_up_to_the_most_and_is_gone_when_it_has_none() {
    let mut w = World::bare();
    w.add(1, 0.0, 0.0);
    // the pistol: 120 in reserve at most; this player has 100
    w.items.set_kit({
        let mut k = w.items.kit(1);
        k.ammo[0] = Ammo { loaded: 12, reserve: 100 };
        k
    });
    let item = w.put(PISTOL, 0.05, 0.0); // 48 in reserve
    let events = w.step(&[]);
    assert_eq!(picked(&events), [(1, Pickup::Ammo { slot: 0, rounds: 20 })]);
    assert_eq!(w.items.kit(1).ammo[0], Ammo { loaded: 12, reserve: 120 });
    assert_eq!(w.items.item(item).unwrap().reserve, 28, "what was not taken stays");
    // full: nothing more, and not a weapon to swap for either
    let events = w.step(&[press(1, 0)]);
    assert!(picked(&events).is_empty());
    assert_eq!(w.weapons(1), [PISTOL, NO_WEAPON]);
    // down to nothing: it goes
    w.items.set_kit({
        let mut k = w.items.kit(1);
        k.ammo[0].reserve = 0;
        k
    });
    w.items.update_item(Item { reserve: 10, ..w.items.item(item).unwrap() });
    let events = w.step(&[]);
    assert_eq!(picked(&events), [(1, Pickup::Ammo { slot: 0, rounds: 10 })]);
    assert!(w.items.item(item).is_none());
}

#[test]
fn the_player_who_put_a_weapon_down_cannot_pick_it_up_while_it_falls_but_can_when_it_rests() {
    let mut w = World::bare();
    w.add_unarmed(1, 0.0, 0.0);
    w.add(2, 0.0, 0.2);
    // a weapon that was dropped by player 1, falling through the player
    let id = w.items.insert_item(Item {
        id: 0,
        tag: RIFLE,
        position: [0.0, 0.0, 0.3],
        velocity: [0.0; 3],
        tick: w.tick,
        resting: false,
        placement: NO_PLACEMENT,
        loaded: 1,
        reserve: 1,
        last_owned: w.tick,
        ignore: 1,
    });
    let events = w.step(&[]);
    assert!(picked(&events).is_empty(), "it is falling past player 1, who put it down, and player 2 has a weapon");
    // it comes to rest, and then player 1 takes it
    let mut taken = Vec::new();
    for _ in 0..200 {
        taken = picked(&w.step(&[]));
        if !taken.is_empty() {
            break;
        }
    }
    assert_eq!(taken, [(1, Pickup::Weapon { slot: 0 })]);
    assert!(w.items.item(id).is_none());
}

#[test]
fn a_dead_player_takes_nothing() {
    let mut w = World::bare();
    w.add_unarmed(1, 0.0, 0.0);
    w.put(RIFLE, 0.1, 0.0);
    w.kill(1);
    assert!(picked(&w.step(&[press(1, 0)])).is_empty());
    assert_eq!(w.loose().len(), 1);
}

#[test]
fn a_player_who_dies_drops_every_weapon_where_they_stood_and_loses_their_camouflage() {
    let mut w = World::bare();
    w.add(1, 4.0, 5.0);
    w.set_weapons(1, [PISTOL, RIFLE]);
    w.items.set_kit({
        let mut k = w.items.kit(1);
        k.camo_until = 5000;
        k
    });
    let events = w.kill(1);
    assert_eq!(events.iter().filter(|e| matches!(e, ItemEvent::Dropped { player: 1, .. })).count(), 2);
    let ground = w.loose();
    assert_eq!(ground.len(), 2);
    for item in &ground {
        assert!((item.position[0] - 4.0).abs() < 0.5 && (item.position[1] - 5.0).abs() < 0.5, "{:?}", item.position);
        assert!(!item.resting);
    }
    assert_eq!(w.weapons(1), [NO_WEAPON; 2]);
    assert_eq!(w.items.kit(1).camo_until, 0);
    // they fall and rest, and are the ones on the floor
    w.run(300);
    assert!(w.loose().iter().all(|i| i.resting && i.position[2].abs() < 1e-3));
}

// ---------- powerups

#[test]
fn an_overshield_overcharges_the_shield_to_three_and_it_wears_off_to_a_full_one() {
    let mut w = World::bare();
    w.add(1, 0.0, 0.0);
    w.put(OVERSHIELD, 0.0, 0.0);
    assert_eq!(picked(&w.step(&[])), [(1, Pickup::Overshield)]);
    assert_ne!(w.vitals(1).flags & SHIELD_OVER_CHARGING, 0);
    // it comes on at a thirtieth of a shield a tick, 60 ticks to three...
    w.run(60);
    assert!(w.vitals(1).shield >= 2.99, "three full shields: {}", w.vitals(1).shield);
    // ...and goes down again, slowly: 2 shields at 0.00074 a tick is 2,700 ticks, 90 seconds
    w.run(1500);
    let half_way = w.vitals(1).shield;
    assert!(half_way > 1.5 && half_way < 2.5, "{half_way}");
    w.run(1300);
    assert_eq!(w.vitals(1).shield, 1.0, "a full shield again");
}

#[test]
fn an_overshield_is_left_for_a_player_whose_shield_is_beyond_full_already() {
    let mut w = World::bare();
    w.add(1, 0.0, 0.0);
    w.put(OVERSHIELD, 0.0, 0.0);
    w.step(&[]);
    w.run(100);
    // a second one, while the first is on
    let second = w.put(OVERSHIELD, 0.0, 0.0);
    assert!(picked(&w.step(&[])).is_empty());
    assert!(w.items.item(second).is_some(), "it stays where it is");
    // a player who is not so charged takes it
    w.add(2, 0.1, 0.0);
    assert_eq!(picked(&w.step(&[])), [(2, Pickup::Overshield)]);
}

#[test]
fn camouflage_lasts_the_tags_45_seconds_to_the_tick_and_is_not_taken_twice() {
    let mut w = World::bare();
    w.add(1, 0.0, 0.0);
    w.put(CAMOUFLAGE, 0.0, 0.0);
    let at = w.tick + 1;
    assert_eq!(picked(&w.step(&[])), [(1, Pickup::Camouflage)]);
    let until = w.items.kit(1).camo_until;
    assert_eq!(until, at + 45 * TICKS_PER_SECOND as u64, "45 seconds from the tick it was taken");
    // another while it is on is left
    let second = w.put(CAMOUFLAGE, 0.0, 0.0);
    assert!(picked(&w.step(&[])).is_empty());
    assert!(w.items.item(second).is_some());
    // it runs out on its tick and not before
    while w.tick + 1 < until {
        let events = w.step(&[]);
        assert!(!events.iter().any(|e| matches!(e, ItemEvent::CamouflageEnded { .. })));
        assert!(w.items.kit(1).is_camouflaged(w.tick));
    }
    let events = w.step(&[]);
    assert_eq!(w.tick, until);
    assert!(events.iter().any(|e| matches!(e, ItemEvent::CamouflageEnded { player: 1 })));
    assert!(!w.items.kit(1).is_camouflaged(w.tick));
}

#[test]
fn a_health_pack_makes_health_full_and_is_left_for_a_player_whose_health_is_full() {
    let mut w = World::bare();
    w.add(1, 0.0, 0.0);
    let pack = w.put(HEALTH_PACK, 0.0, 0.0);
    assert!(picked(&w.step(&[])).is_empty(), "full health: it stays");
    assert!(w.items.item(pack).is_some());
    let mut f = w.fighter(1);
    f.vitals.body = 0.3;
    w.combat.set_fighter(f);
    assert_eq!(picked(&w.step(&[])), [(1, Pickup::Health)]);
    assert_eq!(w.vitals(1).body, 1.0);
    assert!(w.items.item(pack).is_none());
}

// ---------- spawns

#[test]
fn items_spawn_at_the_placements_locations_at_the_start_and_fall_to_rest_on_the_floor_there() {
    let mut w = World::on(items_map());
    w.step(&[]);
    let at_start: Vec<Item> = w.loose();
    // the rifle (every 10 s), the overshield, camouflage and health pack: not the grenade, not the
    // capture-the-flag rifle; the pistol made at rest in the air
    let tags: Vec<u16> = at_start.iter().map(|i| i.tag).collect();
    assert_eq!(tags.len(), 5, "{tags:?}");
    assert!(!tags.contains(&FRAG_GRENADE));
    for item in &at_start {
        let placement = &w.map.items.placements[item.placement as usize];
        assert_eq!(item.position[..2], placement.position[..2], "where the placement says");
    }
    let rested = at_start.iter().find(|i| i.tag == PISTOL).unwrap();
    assert!(rested.resting && rested.position[2] == 2.0, "made at rest, in the air");
    // the others fall from 0.2 above the floor and rest on it
    w.run(60);
    for item in w.loose() {
        assert!(item.resting);
        if item.tag != PISTOL {
            assert!(item.position[2].abs() < 1e-3, "on the floor: {:?}", item.position);
        }
    }
}

#[test]
fn an_item_comes_back_at_the_next_multiple_of_its_period_after_it_was_taken_not_a_period_after() {
    // the rifle's period is 10 seconds: 300 ticks
    let map = with_placements(items_map(), &[placement(RIFLE, 10.0, 0.0, 10, 0)]);
    let mut w = World::on(map);
    w.add_unarmed(1, 10.0, 0.0);
    w.step(&[]);
    // (taken at once, as it is made)
    w.run(5);
    assert_eq!(w.weapons(1), [RIFLE, NO_WEAPON]);
    assert!(w.loose().is_empty());
    // (and goes away: a player of the same weapon would take its rounds)
    w.store.set_player(Player::new(1, [-20.0, 0.0, 0.0], 0.0, 0.0));
    // not back until game time 300: tick 301
    while w.tick < 300 {
        w.step(&[]);
        assert!(w.loose().is_empty(), "tick {}", w.tick);
    }
    let events = w.step(&[]);
    assert_eq!(w.tick, 301);
    assert!(events.iter().any(|e| matches!(e, ItemEvent::Spawned { tag, .. } if *tag == RIFLE)));
    assert_eq!(w.loose().len(), 1);
}

#[test]
fn an_item_nobody_took_is_replaced_by_the_next_and_a_placement_never_has_two() {
    let map = with_placements(items_map(), &[placement(RIFLE, 10.0, 0.0, 10, 0)]);
    let mut w = World::on(map);
    w.step(&[]);
    let first = w.loose()[0].id;
    w.run(299);
    assert_eq!(w.loose()[0].id, first, "the same one a tick before the period");
    w.run(1);
    assert_eq!(w.loose().len(), 1);
    assert_ne!(w.loose()[0].id, first, "a new one at the period");
}

#[test]
fn a_placements_period_is_its_own_spawn_time_else_its_collections_else_30_seconds() {
    let map = items_map();
    let periods: Vec<u64> = map.items.placements.iter().map(items::period_ticks).collect();
    // rifle 10 s; overshield (collection) 60 s; camouflage (collection) 45 s; health pack (none) 30 s
    assert_eq!(periods[..4], [300, 1800, 1350, 900]);
}

#[test]
fn a_placement_for_another_game_type_makes_nothing_and_a_game_of_all_makes_it() {
    let mut w = World::on(items_map());
    w.step(&[]);
    // the capture-the-flag rifle is the placement at index 4
    assert!(w.loose().iter().all(|i| i.placement != 4));
    assert!(w.loose().iter().any(|i| i.placement == 0));
}

// ---------- items last 30 seconds

#[test]
fn a_dropped_weapon_is_taken_away_30_seconds_after_it_was_put_down() {
    let mut w = World::bare();
    w.add(1, 0.0, 0.0);
    w.set_weapons(1, [PISTOL, RIFLE]);
    w.kill(1);
    let dropped_at = w.tick;
    assert_eq!(w.loose().len(), 2);
    while w.tick < dropped_at + PURGE_TICKS {
        w.step(&[]);
        assert_eq!(w.loose().len(), 2, "still there at tick {}", w.tick);
    }
    w.step(&[]);
    assert!(w.loose().is_empty(), "gone after 30 seconds");
}

// ---------- the client's rounds

#[test]
fn the_rounds_a_client_reports_are_kept_to_what_the_weapon_holds() {
    let mut w = World::bare();
    w.add(1, 0.0, 0.0);
    assert!(pickups::report_ammo(
        &mut w.items,
        &w.combat,
        &w.map,
        1,
        [Ammo { loaded: 9, reserve: 30 }, Ammo { loaded: 100, reserve: 5000 }]
    ));
    let kit = w.items.kit(1);
    assert_eq!(kit.ammo[0], Ammo { loaded: 9, reserve: 30 });
    assert_eq!(kit.ammo[1], Ammo::default(), "no weapon in the second slot");
    let before = kit.version;
    pickups::report_ammo(&mut w.items, &w.combat, &w.map, 1, [Ammo { loaded: 500, reserve: 500 }, Ammo::default()]);
    assert_eq!(w.items.kit(1).ammo[0], Ammo { loaded: 12, reserve: 120 }, "the pistol holds 12 and 120");
    assert_eq!(w.items.kit(1).version, before, "a report is not the server changing them");
    assert!(!pickups::report_ammo(&mut w.items, &w.combat, &w.map, 77, [Ammo::default(); 2]), "no such player");
}

// ---------- the same every time

#[test]
fn the_same_ticks_give_the_same_items() {
    fn play() -> Vec<u8> {
        let mut w = World::on(items_map());
        for p in 0..6u16 {
            w.add(p, 10.0 + p as f32 * 0.1, 0.0);
        }
        for t in 0..2000u64 {
            let requests: Vec<Request> =
                if t % 7 == 0 { (0..6).map(|p| press(p, (t % 2) as u8)).collect() } else { Vec::new() };
            w.step(&requests);
            if t == 700 {
                w.kill(2);
            }
        }
        let ids: Vec<PlayerId> = (0..6).collect();
        items::snapshot_items(&w.items, &ids)
    }
    assert_eq!(play(), play());
}
