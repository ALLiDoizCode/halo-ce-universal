//! Items and pickups in the match module, against a real local SpacetimeDB:
//! the placements' items appear and come back on their periods, a dropped
//! weapon falls and rests (and a client works its fall out from the row alone,
//! to the bit), two players who reach for one item get one between them, and
//! the powerups behave as the tags say. The rules are tested at the step in
//! `rust/halo-sim/tests/pickups.rs`; these check that the module keeps them in
//! its tables and its tick and that they work from a client's call.
//!
//! They need `HALO_STDB_BIN` (a SpacetimeDB 2.10.x release directory) and skip
//! themselves without it; no game data: the map is a flat floor with the
//! fixtures' weapons and powerups.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use halo_match_driver::module_bindings::report_ammo as _;
use halo_match_driver::server::{build_module, stdb_bin_dir, Server};
use halo_match_driver::{ItemRow, MatchClient, PlayerClient};
use halo_sim::damage::{Vitals, SHIELD_OVER_CHARGING};
use halo_sim::fixtures::{
    items_map, placement, rifle, start_at, with_placements, with_starts, CAMOUFLAGE, OVERSHIELD, PISTOL, RIFLE,
};
use halo_sim::items::{Item, NO_PLACEMENT, NO_PLAYER};
use halo_sim::rules::Rules;
use halo_sim::MapData;

const WAIT: Duration = Duration::from_secs(30);
const STATE_ALIVE: u8 = 0;
const NO_WEAPON: u16 = u16::MAX;
/// A third kind of weapon, for a swap.
const SHOTGUN: u16 = 900;

fn wasm() -> &'static PathBuf {
    static WASM: OnceLock<PathBuf> = OnceLock::new();
    WASM.get_or_init(build_module)
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn item_of(row: &ItemRow) -> Item {
    Item {
        id: row.id,
        tag: row.tag,
        position: [row.x, row.y, row.z],
        velocity: [row.vx, row.vy, row.vz],
        tick: row.tick,
        resting: row.resting,
        placement: row.placement,
        loaded: row.loaded,
        reserve: row.reserve,
        last_owned: row.last_owned,
        ignore: row.ignore,
    }
}

struct Arena {
    _server: Server,
    owner: MatchClient,
    players: Vec<PlayerClient>,
    map: MapData,
}

/// The fixtures' map with a third weapon, these placements and nobody else's, and two players
/// standing at `(10, 0)` and `(10.3, 0)`, alive and with the pistol.
fn arena(name: &str, placements: &[halo_map::items::Placement], tweak: impl FnOnce(&mut MapData)) -> Option<Arena> {
    let Some(bin) = stdb_bin_dir() else {
        eprintln!("HALO_STDB_BIN is not set: skipping, this test needs a SpacetimeDB 2.10.x release");
        return None;
    };
    let server = Server::start(&bin);
    server.publish(wasm(), name);
    let owner = server.connect(name);
    let mut map = with_starts(with_placements(items_map(), placements), &[start_at(10.0, 0.0, -1)]);
    map.combat.weapons.push({
        let mut shotgun = rifle();
        shotgun.tag_index = SHOTGUN;
        shotgun.name = "weapons\\shotgun\\shotgun.weap".into();
        shotgun
    });
    map.items.defs.push(halo_map::items::ItemDef {
        tag_index: SHOTGUN,
        name: "weapons\\shotgun\\shotgun.weap".into(),
        ..map.items.defs.iter().find(|d| d.tag_index == RIFLE).unwrap().clone()
    });
    tweak(&mut map);
    owner.load_map(map.to_bytes()).unwrap();
    owner.set_game(&Rules { suicide_penalty_ticks: 0, respawn_ticks: 60, ..Rules::slayer() }).unwrap();
    owner
        .set_spawn_points(&[
            halo_sim::PlayerInput { player: 0, position: [10.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, flags: 0 },
            halo_sim::PlayerInput { player: 1, position: [10.3, 0.0, 0.0], yaw: 0.0, pitch: 0.0, flags: 0 },
        ])
        .unwrap();
    owner.start();
    let players: Vec<PlayerClient> = (0..2u8)
        .map(|id| {
            let client = PlayerClient::connect_unsubscribed(&server.uri(), name, &server.new_account().token);
            client.join([id + 1; 32]).unwrap();
            client
        })
        .collect();
    wait_until("both players spawned", || {
        owner.standings().values().filter(|s| s.state == STATE_ALIVE).count() == 2 && owner.fighters().len() == 2
    });
    Some(Arena { _server: server, owner, players, map })
}

impl Arena {
    fn tick(&self) -> u64 {
        self.owner.marker().unwrap().tick
    }

    fn weapons(&self, player: u16) -> (u16, u16) {
        let f = &self.owner.fighters()[&player];
        (f.weapon_0, f.weapon_1)
    }

    fn items_of(&self, tag: u16) -> Vec<ItemRow> {
        self.owner.items().into_values().filter(|i| i.tag == tag).collect()
    }

    fn vitals(&self, player: u16) -> Vitals {
        let f = &self.owner.fighters()[&player];
        Vitals { shield: f.shield, body: f.body, shield_stun_ticks: f.shield_stun_ticks, flags: f.flags }
    }
}

#[test]
fn the_placements_items_appear_at_their_locations_fall_to_rest_and_come_back_on_their_period() {
    // a rifle every two seconds (60 ticks), far from the players
    let Some(a) = arena("items-period", &[placement(RIFLE, 30.0, 4.0, 2, 0)], |_| {}) else { return };
    wait_until("the first item", || !a.items_of(RIFLE).is_empty());
    let first = a.items_of(RIFLE).remove(0);
    assert_eq!((first.x, first.y), (30.0, 4.0), "where the placement is");
    wait_until("it comes to rest", || a.items_of(RIFLE).first().is_some_and(|i| i.resting));
    let rest = a.items_of(RIFLE).remove(0);
    assert!(rest.z.abs() < 1e-3, "on the floor: {}", rest.z);
    assert_eq!((rest.vx, rest.vy, rest.vz), (0.0, 0.0, 0.0));
    assert_eq!(rest.placement, 0);
    assert_eq!((rest.loaded, rest.reserve), (32, 64), "with the tags rounds");
    // the next replaces it on the period: 60 ticks after, to the tick
    wait_until("the next one", || a.items_of(RIFLE).first().is_some_and(|i| i.id != first.id));
    let second = a.items_of(RIFLE).remove(0);
    assert_eq!(a.items_of(RIFLE).len(), 1, "a placement never has two");
    // (a placement's item is last held at its spawn tick, less the 30 seconds its period does not add)
    assert_eq!(second.last_owned - first.last_owned, 60, "a period of two seconds apart");
}

#[test]
fn the_module_works_out_a_fall_the_way_a_client_does_from_the_row_of_its_drop() {
    // a shotgun to swap for, near the players, once (a period of 100 seconds)
    let Some(a) = arena("items-swap", &[placement(SHOTGUN, 10.1, 0.0, 100, 0)], |_| {}) else { return };
    a.owner.set_loadout(0, PISTOL, RIFLE).unwrap();
    wait_until("the loadout", || a.weapons(0) == (PISTOL, RIFLE));
    wait_until("the shotgun on the floor", || a.items_of(SHOTGUN).first().is_some_and(|i| i.resting));
    // player 0 swaps the weapon in hand, the rifle (slot 1), for it
    a.players[0].use_item(1).unwrap();
    // (the weapon put down is an item that falls: catch its row while it does)
    let mut falling: Option<ItemRow> = None;
    wait_until("the rifle put down", || {
        if let Some(row) = a.items_of(RIFLE).into_iter().find(|i| i.placement == NO_PLACEMENT) {
            falling.get_or_insert(row);
            true
        } else {
            false
        }
    });
    let falling = falling.unwrap();
    assert_eq!(a.weapons(0), (PISTOL, SHOTGUN), "the shotgun is the second now");
    assert!(a.items_of(SHOTGUN).is_empty(), "taken");
    assert_eq!(falling.ignore, 0, "the player who put it down cannot take it until it rests");
    assert!(!falling.resting, "it was thrown and falls: {falling:?}");
    assert!(falling.vx != 0.0 || falling.vy != 0.0 || falling.vz != 0.0, "thrown");
    // it comes to rest on the floor, and the rest the module wrote is what working the fall out from
    // the row of the drop gives: that is how a client sees it fall without a row a tick
    wait_until("it rests", || a.items_of(RIFLE).first().is_some_and(|i| i.resting));
    let rest = a.items_of(RIFLE).remove(0);
    assert!(rest.z.abs() < 1e-3, "{}", rest.z);
    assert_eq!(rest.ignore, NO_PLAYER, "and now anyone may take it");
    let (worked_out, flight) = item_of(&falling).advanced_to(&a.map, rest.tick + 100);
    assert_eq!(flight, halo_sim::items::Flight::Rested);
    assert_eq!(worked_out.position, [rest.x, rest.y, rest.z], "the same to the bit");
    assert_eq!(worked_out.tick, rest.tick, "and at the same tick");
    // the rounds the weapon had when it was put down are the server's (the starting ones of the rifle)
    assert_eq!((rest.loaded, rest.reserve), (32, 64));
}

#[test]
fn two_players_who_press_for_one_weapon_on_the_same_tick_get_one_between_them_the_nearer() {
    let Some(a) = arena("items-contention", &[placement(SHOTGUN, 10.2, 0.0, 100, 0)], |_| {}) else { return };
    wait_until("the shotgun on the floor", || a.items_of(SHOTGUN).first().is_some_and(|i| i.resting));
    // both stand within reach of it (0.2 and 0.1 away, the second nearer); each presses before the tick runs
    a.owner.stop_and_wait().unwrap();
    a.players[0].use_item(0).unwrap();
    a.players[1].use_item(0).unwrap();
    a.owner.start();
    wait_until("the shotgun to be taken", || a.items_of(SHOTGUN).is_empty());
    let (zero, one) = (a.weapons(0), a.weapons(1));
    let holders = [zero, one].iter().filter(|w| w.0 == SHOTGUN || w.1 == SHOTGUN).count();
    assert_eq!(holders, 1, "exactly one of them has it: {zero:?} and {one:?}");
    assert_eq!(one, (PISTOL, SHOTGUN), "the nearer, player 1, took it as the second weapon: {one:?}");
    assert_eq!(zero, (PISTOL, NO_WEAPON), "the other kept theirs");
}

#[test]
fn camouflage_is_a_row_for_the_tags_time_and_gone_on_its_tick() {
    // (the tag's 45 seconds, for a test that is not a minute long: two)
    let Some(a) = arena("items-camo", &[placement(CAMOUFLAGE, 10.0, 0.0, 100, 0)], |m| {
        m.items.defs.iter_mut().find(|d| d.tag_index == CAMOUFLAGE).unwrap().powerup_time = 2.0;
    }) else {
        return;
    };
    wait_until("camouflage", || a.owner.powerups().contains_key(&0) || a.owner.powerups().contains_key(&1));
    let (player, row) = a.owner.powerups().into_iter().next().unwrap();
    assert_eq!(player, 0, "the nearer of the two took it");
    let taken = a.owner.items().len();
    assert_eq!(taken, 0, "the item is gone");
    assert!(row.camo_until > 0);
    // it lasts two seconds from the tick it was taken: 60 ticks, so the row is there until then
    wait_until("it to end", || !a.owner.powerups().contains_key(&0));
    assert!(a.tick() >= row.camo_until, "the row goes at tick {}: it is {}", row.camo_until, a.tick());
    assert!(a.tick() <= row.camo_until + 5, "and not long after");
}

#[test]
fn an_overshield_is_in_the_fighters_row_and_wears_off_by_the_engines_decay() {
    let Some(a) = arena("items-overshield", &[placement(OVERSHIELD, 10.0, 0.0, 100, 0)], |_| {}) else { return };
    wait_until("the overshield", || a.owner.fighters()[&0].flags & SHIELD_OVER_CHARGING != 0);
    let map = &a.map;
    let mut v = a.vitals(0);
    // (a client counts forward from the row: it comes on in 60 ticks...)
    v.advance(&map.combat.resistance, 61);
    assert!(v.shield >= 2.95, "{}", v.shield);
    // ...and has worn off to a full shield 2,700 ticks after (the engine's rate)
    v.advance(&map.combat.resistance, 2800);
    assert_eq!(v.shield, 1.0);
    assert!(a.owner.items().is_empty(), "taken");
    assert_eq!(a.owner.fighters()[&1].flags & SHIELD_OVER_CHARGING, 0, "the other got nothing");
}

#[test]
fn a_player_who_dies_puts_their_weapons_down_and_the_rounds_reported_are_kept() {
    let Some(a) = arena("items-death", &[], |_| {}) else { return };
    a.owner.set_loadout(0, PISTOL, RIFLE).unwrap();
    wait_until("the loadout", || a.weapons(0) == (PISTOL, RIFLE));
    // the client says how many rounds it has: eight bytes, kept within what the weapons hold
    a.players[0].report_ammo([3, 40, 9, 5000]).unwrap();
    wait_until("the rounds", || a.owner.kits().get(&0).is_some_and(|k| k.reserve_0 == 40));
    let kit = a.owner.kits()[&0].clone();
    assert_eq!((kit.loaded_0, kit.reserve_0, kit.loaded_1, kit.reserve_1), (3, 40, 9, 288), "the rifle holds 288");
    // (the other player holds a third kind, so that nothing put down is rounds for them to take)
    a.owner.set_loadout(1, SHOTGUN, NO_WEAPON).unwrap();
    wait_until("the other loadout", || a.weapons(1) == (SHOTGUN, NO_WEAPON));
    a.owner.report_death(0, None).unwrap();
    wait_until("both weapons on the ground", || a.owner.items().len() == 2);
    let items = a.owner.items();
    let pistol = items.values().find(|i| i.tag == PISTOL).unwrap();
    let dropped_rifle = items.values().find(|i| i.tag == RIFLE).unwrap();
    assert_eq!((pistol.loaded, pistol.reserve), (3, 40), "with the rounds the player had");
    assert_eq!((dropped_rifle.loaded, dropped_rifle.reserve), (9, 288));
    assert!(items.values().all(|i| (i.x - 10.0).abs() < 1.0 && i.ignore == 0));
    wait_until("both at rest", || a.owner.items().values().all(|i| i.resting));
    assert!(a.owner.items().values().all(|i| i.z.abs() < 1e-3), "on the floor");
    assert_eq!(a.weapons(0), (NO_WEAPON, NO_WEAPON));
    // and a player who respawns carries the starting weapon again, with its rounds
    wait_until("the respawn", || a.owner.standings()[&0].state == STATE_ALIVE && a.weapons(0).0 == PISTOL);
    wait_until("the fresh rounds", || a.owner.kits()[&0].version > kit.version);
    let kit = a.owner.kits()[&0].clone();
    assert_eq!((kit.loaded_0, kit.reserve_0), (12, 48));
}

#[test]
fn only_a_seated_player_may_press_and_report_and_a_report_must_be_whole() {
    let Some(a) = arena("items-guards", &[], |_| {}) else { return };
    let stranger = PlayerClient::connect_unsubscribed(&a._server.uri(), "items-guards", &a._server.new_account().token);
    assert!(stranger.use_item(0).unwrap_err().contains("seat"));
    assert!(stranger.report_ammo([1, 2, 3, 4]).unwrap_err().contains("seat"));
    let short = halo_match_driver::call_reducer("report_ammo", |cb| {
        a.players[0].conn.reducers.report_ammo_then(vec![1, 2, 3], cb)
    });
    assert!(short.unwrap_err().contains("not 8"));
}
