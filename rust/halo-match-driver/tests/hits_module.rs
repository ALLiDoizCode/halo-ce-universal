//! Hit reports in the match module, against a real local SpacetimeDB: a
//! client reports the hits its engine saw over its own connection, and the
//! module's tick judges each report (the checks of `halo_sim::combat`), deals
//! the damage of those that pass to the target's shield and health, counts
//! those that do not, and turns a death into the rules' death (a score, a
//! respawn). The checks and the damage are tested at the step in
//! `rust/halo-sim`; these check that the module keeps them in its tables and
//! its tick, and that they work from a client's call.
//!
//! They need `HALO_STDB_BIN` (a SpacetimeDB 2.10.x release directory) and
//! skip themselves without it; no game data: the map is a flat floor with
//! the pistol and the player's body the fixtures have.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use halo_match_driver::module_bindings::{report_hits as _, set_loadout as _};
use halo_match_driver::server::{build_module, stdb_bin_dir, Server};
use halo_match_driver::{FighterRow, MatchClient, PlayerClient};
use halo_sim::combat::HitReport;
use halo_sim::damage::{Vitals, DEAD};
use halo_sim::fixtures::{
    combat_fixture, flat_floor_map, needler, plasma_pistol, plasma_rifle, rocket_launcher, shotgun, sniper_rifle,
    start_at, with_starts, NEEDLER, NEEDLER_ATTACHED_DAMAGE, NEEDLER_BLAST, PISTOL, PISTOL_DAMAGE, PLASMA_PISTOL,
    PLASMA_PISTOL_CHARGED_DAMAGE, PLASMA_PISTOL_DAMAGE, PLASMA_RIFLE, PLASMA_RIFLE_DAMAGE, ROCKET_BLAST,
    ROCKET_LAUNCHER, SHOTGUN, SHOTGUN_DAMAGE, SNIPER_RIFLE, SNIPER_RIFLE_DAMAGE,
};
use halo_sim::rules::Rules;
use halo_sim::TICKS_PER_SECOND;

const WAIT: Duration = Duration::from_secs(20);
const STATE_ALIVE: u8 = 0;
const STATE_DEAD: u8 = 1;
const SHOOTER: u16 = 0;
const TARGET: u16 = 1;

fn wasm() -> &'static PathBuf {
    static WASM: OnceLock<PathBuf> = OnceLock::new();
    WASM.get_or_init(build_module)
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + WAIT;
    while !done() {
        assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

struct Fight {
    // (kept so that the database lives as long as the test)
    _server: Server,
    owner: MatchClient,
    players: Vec<PlayerClient>,
}

/// A match on a flat floor with two players, spawned and standing 10 units
/// apart, running: player 0 shoots, player 1 is shot at.
fn fight(name: &str) -> Option<Fight> {
    fight_on(name, |_| {})
}

/// ... on a map that `tweak` has changed.
fn fight_on(name: &str, tweak: impl FnOnce(&mut halo_sim::MapData)) -> Option<Fight> {
    let starts: Vec<_> = (0..4).map(|i| start_at(i as f32 * 10.0 - 20.0, 0.0, -1)).collect();
    fight_from(name, &starts, tweak)
}

/// ... where the two players spawn `gap` apart.
fn fight_apart(name: &str, gap: f32, tweak: impl FnOnce(&mut halo_sim::MapData)) -> Option<Fight> {
    fight_from(name, &[start_at(-gap / 2.0, 0.0, -1), start_at(gap / 2.0, 0.0, -1)], tweak)
}

fn fight_from(
    name: &str,
    starts: &[halo_sim::spawn::Start],
    tweak: impl FnOnce(&mut halo_sim::MapData),
) -> Option<Fight> {
    let Some(bin) = stdb_bin_dir() else {
        eprintln!("HALO_STDB_BIN is not set: skipping, this test needs a SpacetimeDB 2.10.x release");
        return None;
    };
    let server = Server::start(&bin);
    server.publish(wasm(), name);
    let owner = server.connect(name);
    let mut map = with_starts(flat_floor_map(), starts);
    tweak(&mut map);
    owner.load_map(map.to_bytes()).unwrap();
    owner.set_game(&Rules { suicide_penalty_ticks: 0, ..Rules::slayer() }).unwrap();
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
    // (a second of the server seeing them stand there)
    let now = owner.marker().unwrap().tick;
    owner.wait_for_tick(now + TICKS_PER_SECOND as u64, WAIT);
    Some(Fight { _server: server, owner, players })
}

impl Fight {
    fn tick(&self) -> u64 {
        self.owner.marker().unwrap().tick
    }

    fn position(&self, id: u16) -> [f32; 3] {
        let p = &self.owner.players()[&id];
        [p.x, p.y, p.z]
    }

    /// The hit a shooter's engine would report on the target as it stands, made now.
    fn hit_on_target(&self) -> HitReport {
        let at = self.position(TARGET);
        HitReport {
            target: TARGET,
            damage: PISTOL_DAMAGE,
            material: 1,
            scale: 1.0,
            host_tick: self.tick() as u32,
            origin: [at[0], at[1], at[2] + 0.3],
            target_position: at,
        }
    }

    fn fighter(&self, id: u16) -> FighterRow {
        self.owner.fighters().remove(&id).unwrap_or_else(|| panic!("player {id} has no fighter"))
    }

    fn rejected_hits(&self) -> u64 {
        self.owner.marker().unwrap().rejected_hits_total
    }

    fn vitals(&self, id: u16) -> Vitals {
        let f = self.fighter(id);
        Vitals { shield: f.shield, body: f.body, shield_stun_ticks: f.shield_stun_ticks, flags: f.flags }
    }

    /// Report hits as the shooter, and wait until the tick that judges them has run.
    fn report(&self, hits: &[HitReport]) {
        let before = self.tick();
        self.players[SHOOTER as usize].report_hits(hits).unwrap();
        self.owner.wait_for_tick(before + 3, WAIT);
    }
}

#[test]
fn a_player_who_spawns_has_full_health_and_shields_and_the_starting_weapon() {
    let Some(f) = fight("spawn-fighter") else { return };
    for id in [SHOOTER, TARGET] {
        let row = f.fighter(id);
        assert_eq!((row.shield, row.body, row.flags, row.shield_stun_ticks), (1.0, 1.0, 0, 0));
        assert_eq!((row.weapon_0, row.weapon_1), (PISTOL, u16::MAX), "the pistol the fixtures' map has");
        assert_eq!((row.hurt_count, row.hurt_by), (0, u16::MAX));
    }
}

#[test]
fn a_hit_that_passes_the_checks_takes_the_targets_shield_and_is_counted() {
    let Some(f) = fight("hit-passes") else { return };
    f.report(&[f.hit_on_target()]);
    wait_until("the damage", || f.fighter(TARGET).hurt_count == 1);
    let target = f.fighter(TARGET);
    assert!(
        (target.shield - (1.0 - 25.0 / 75.0)).abs() < 1e-5,
        "a pistol hit takes a third of the shield: {}",
        target.shield
    );
    assert_eq!((target.body, target.shield_stun_ticks, target.hurt_by), (1.0, 180, SHOOTER));
    assert!(target.hurt_tick > 0);
    assert_eq!(f.rejected_hits(), 0);
    assert_eq!(f.fighter(SHOOTER).hurt_count, 0, "the shooter is not hurt");
}

#[test]
fn a_shield_recharges_without_the_server_writing_it_and_a_client_counts_it() {
    let Some(f) = fight("hit-recharge") else { return };
    f.report(&[f.hit_on_target()]);
    wait_until("the damage", || f.fighter(TARGET).hurt_count == 1);
    let hurt = f.fighter(TARGET);
    // six seconds of stun and four of recharge: the row stays as the hit left it
    f.owner.wait_for_tick(hurt.tick + 3 * TICKS_PER_SECOND as u64, WAIT);
    assert_eq!(f.fighter(TARGET), hurt, "no write for a recharge");
    // ... and what a client counts from the row, as of a later tick, is the engine's recharge
    let map = {
        let mut m = flat_floor_map();
        m.combat = combat_fixture();
        m
    };
    let mut v = f.vitals(TARGET);
    v.advance(&map.combat.resistance, 180);
    assert_eq!(v.shield_stun_ticks, 0);
    let before = v.shield;
    v.advance(&map.combat.resistance, 30);
    assert!((v.shield - before - 30.0 * 0.008_333_334).abs() < 1e-5);
    v.advance(&map.combat.resistance, 400);
    assert_eq!(v.shield, 1.0);
}

#[test]
fn a_report_that_fails_each_check_is_rejected_and_counted_and_hurts_nobody() {
    let Some(f) = fight("hit-rejects") else { return };
    let good = f.hit_on_target();
    let mut expected = 0;
    let mut reject = |what: &str, hit: HitReport| {
        f.report(&[hit]);
        expected += 1;
        wait_until(what, || f.rejected_hits() == expected);
        assert_eq!(f.fighter(TARGET).hurt_count, 0, "{what}: the target was hurt");
        assert_eq!(f.vitals(TARGET).shield, 1.0, "{what}");
    };
    // the shooter does not own the weapon
    reject("a weapon the shooter does not own", HitReport { damage: 999, ..good });
    // the target was not within reach of where the server saw it
    let mut far = good;
    far.target_position[1] += 6.0;
    far.origin[1] += 6.0;
    reject("a target out of reach of where the server saw it", far);
    // the report is too old (more than three seconds)
    let now = f.tick();
    reject(
        "a report that is too old",
        HitReport { host_tick: (now - 4 * TICKS_PER_SECOND as u64) as u32, ..f.hit_on_target() },
    );
    // ... or from a tick the server has not reached
    reject("a report from the future", HitReport { host_tick: (f.tick() + 1000) as u32, ..f.hit_on_target() });
    // the impact is nowhere near the target
    let mut off = f.hit_on_target();
    off.origin[2] += 30.0;
    reject("an impact away from the target", off);
    // a hit on oneself
    reject("a hit on oneself", HitReport { target: SHOOTER, ..f.hit_on_target() });
    assert_eq!(f.fighter(SHOOTER).hurt_count, 0);
}

#[test]
fn more_hits_than_the_weapon_can_fire_are_rejected_and_counted() {
    // (a target that a thousand hits do not kill, so that every report is judged on its rate alone)
    let Some(f) = fight_on("hit-rate", |map| {
        map.combat.resistance.maximum_shield_vitality = 1.0e6;
        map.combat.resistance.maximum_body_vitality = 1.0e6;
    }) else {
        return;
    };
    // sixty at once: the bucket holds three seconds of fire at twice the pistol's rate, 21 hits, which a tick fills no more
    let hits = vec![f.hit_on_target(); 60];
    f.report(&hits);
    wait_until("the verdicts", || f.rejected_hits() >= 30);
    let rejected = f.rejected_hits();
    assert!((38..=40).contains(&rejected), "21 of 60 pass: {rejected} were rejected");
    assert_eq!(f.fighter(TARGET).hurt_count as u64, 60 - rejected, "what passed hurt the target, and nothing else did");
    // a second later the bucket has a second of fire in it again (7 hits)
    f.owner.wait_for_tick(f.tick() + TICKS_PER_SECOND as u64, WAIT);
    let before = f.rejected_hits();
    f.report(&[f.hit_on_target(); 20]);
    wait_until("the second verdicts", || f.rejected_hits() > before);
    let passed = 20 - (f.rejected_hits() - before);
    assert!((6..=9).contains(&passed), "a second of fire is about 7 hits: {passed}");
}

#[test]
fn a_hit_on_a_target_who_is_dead_is_rejected() {
    let Some(f) = fight("hit-dead") else { return };
    f.owner.report_death(TARGET, Some(SHOOTER)).unwrap();
    wait_until("the death", || f.owner.standings()[&TARGET].state == STATE_DEAD);
    let before = f.rejected_hits();
    f.report(&[f.hit_on_target()]);
    wait_until("the rejection", || f.rejected_hits() == before + 1);
    // ... and the shooter's own death keeps them from hitting anyone
    let now = f.tick();
    f.owner.wait_for_tick(now + 2, WAIT);
}

#[test]
fn five_hits_kill_the_target_and_the_shooter_scores_and_the_target_respawns_whole() {
    let Some(f) = fight("hit-kills") else { return };
    f.report(&[f.hit_on_target(); 5]);
    wait_until("the death", || f.owner.standings()[&TARGET].state == STATE_DEAD);
    assert_eq!(f.owner.standings()[&SHOOTER].score, 1, "the kill is the shooter's");
    assert_eq!(f.owner.standings()[&TARGET].deaths, 1);
    assert_eq!(f.rejected_hits(), 0, "all five were judged good");
    assert!(f.vitals(TARGET).flags & DEAD != 0);
    // the respawn gives them their health and shields back
    wait_until("the respawn", || f.owner.standings()[&TARGET].state == STATE_ALIVE);
    wait_until("the fresh fighter", || f.fighter(TARGET).flags & DEAD == 0);
    let row = f.fighter(TARGET);
    assert_eq!((row.shield, row.body, row.hurt_count), (1.0, 1.0, 0));
}

#[test]
fn a_hit_to_the_head_with_the_shield_down_kills_at_once() {
    let Some(f) = fight("hit-head") else { return };
    let head = HitReport { material: 0, ..f.hit_on_target() };
    // three hit the shield away (a head costs the shield what a body does), the fourth reaches the head
    f.report(&[head, head, head]);
    wait_until("the shield down", || f.fighter(TARGET).hurt_count == 3);
    assert!(f.vitals(TARGET).flags & DEAD != 0, "the third hit's remainder reached the head");
    wait_until("the death", || f.owner.standings()[&TARGET].state == STATE_DEAD);
}

#[test]
fn only_a_seated_player_can_report_hits_and_a_batch_must_be_whole() {
    let Some(f) = fight("hit-guards") else { return };
    let stranger = PlayerClient::connect_unsubscribed(&f._server.uri(), "hit-guards", &f._server.new_account().token);
    assert!(stranger.report_hits(&[f.hit_on_target()]).unwrap_err().contains("seat"));
    let batch = halo_sim::wire::encode_hits(&[f.hit_on_target()]);
    let ragged = batch[..batch.len() - 1].to_vec();
    let call =
        halo_match_driver::call_reducer("report_hits", |cb| f.players[0].conn.reducers.report_hits_then(ragged, cb));
    assert!(call.unwrap_err().contains("whole"));
    let call = halo_match_driver::call_reducer("report_hits", |cb| {
        f.players[0].conn.reducers.report_hits_then(halo_sim::wire::encode_hits(&[f.hit_on_target(); 65]), cb)
    });
    assert!(call.unwrap_err().contains("at most"));
    assert_eq!(f.rejected_hits(), 0);
}

#[test]
fn only_the_owner_may_set_a_loadout_and_a_weapon_taken_away_is_one_the_shooter_does_not_own() {
    let Some(f) = fight("hit-loadout") else { return };
    let call = halo_match_driver::call_reducer("set_loadout", |cb| {
        f.players[0].conn.reducers.set_loadout_then(SHOOTER, 1, 2, cb)
    });
    assert!(call.unwrap_err().contains("owner"));
    assert_eq!(f.fighter(SHOOTER).weapon_0, PISTOL, "not the owner's call");
    f.owner.set_loadout(SHOOTER, u16::MAX, u16::MAX).unwrap();
    wait_until("the empty hands", || f.fighter(SHOOTER).weapon_0 == u16::MAX);
    f.report(&[f.hit_on_target()]);
    wait_until("the rejection", || f.rejected_hits() == 1);
    assert_eq!(f.fighter(TARGET).hurt_count, 0);
}

/// A fight where the shooter carries the weapon, and the target's body is `vitality` times the usual (the
/// weapons of the maps beside the pistol are in the match's map).
fn armed_fight(name: &str, weapon: u16, vitality: f32) -> Option<Fight> {
    armed_fight_apart(name, 10.0, weapon, vitality)
}

/// ... with the players `gap` apart.
fn armed_fight_apart(name: &str, gap: f32, weapon: u16, vitality: f32) -> Option<Fight> {
    let f = fight_apart(name, gap, |map| {
        map.combat.weapons.extend([
            shotgun(),
            sniper_rifle(),
            plasma_rifle(),
            plasma_pistol(),
            rocket_launcher(),
            needler(),
        ]);
        map.combat.resistance.maximum_shield_vitality *= vitality;
        map.combat.resistance.maximum_body_vitality *= vitality;
    })?;
    f.owner.set_loadout(SHOOTER, weapon, u16::MAX).unwrap();
    wait_until("the weapon in hand", || f.fighter(SHOOTER).weapon_0 == weapon);
    Some(f)
}

#[test]
fn a_shotgun_blast_hurts_the_target_pellet_by_pellet_and_kills_it_for_its_shooter() {
    let Some(f) = armed_fight_apart("hit-shotgun", 5.0, SHOTGUN, 1.0) else { return };
    let pellet = HitReport { damage: SHOTGUN_DAMAGE, ..f.hit_on_target() };
    // fifteen pellets at once, as a shot makes them: 8 to 25 each, so the 150 of the target's shield and health is gone
    // in 6 to 19 of them, and the pellets that come after its death are refused
    f.report(&[pellet; 15]);
    wait_until("the death", || f.owner.standings()[&TARGET].state == STATE_DEAD);
    assert_eq!(f.owner.standings()[&SHOOTER].score, 1, "the kill is the shooter's");
    let hurt = f.fighter(TARGET).hurt_count as u64;
    assert!((6..=15).contains(&hurt), "{hurt} pellets hurt it");
    wait_until("the verdicts", || f.rejected_hits() + hurt == 15);
}

#[test]
fn a_pellet_that_has_flown_far_deals_only_the_minimum_whatever_the_client_says() {
    // thirty units apart: a pellet has slowed past all it does, and the server brings the report's scale down to 0
    let Some(f) = armed_fight_apart("hit-pellets-far", 30.0, SHOTGUN, 1.0) else { return };
    let pellet = HitReport { damage: SHOTGUN_DAMAGE, scale: 1.0, ..f.hit_on_target() };
    f.report(&[pellet; 15]);
    wait_until("the pellets", || f.fighter(TARGET).hurt_count == 15);
    // 15 of the minimum, 8: a shield of 75 and 45 of the 75 of health
    let target = f.fighter(TARGET);
    assert_eq!(target.shield, 0.0);
    assert!((target.body - (1.0 - 45.0 / 75.0)).abs() < 1.0e-4, "{}", target.body);
    assert_eq!(f.rejected_hits(), 0);
    assert_eq!(f.owner.standings()[&TARGET].state, STATE_ALIVE);
}

#[test]
fn a_sniper_rifles_hit_takes_the_shield_and_a_quarter_of_the_health_and_the_pistols_is_no_longer_the_shooters() {
    let Some(f) = armed_fight("hit-sniper", SNIPER_RIFLE, 1.0) else { return };
    let bullet = HitReport { damage: SNIPER_RIFLE_DAMAGE, ..f.hit_on_target() };
    f.report(&[bullet]);
    wait_until("the damage", || f.fighter(TARGET).hurt_count == 1);
    let target = f.fighter(TARGET);
    // 101 of damage: a shield of 75, and 26 of the body's 75
    assert_eq!(target.shield, 0.0);
    assert!((target.body - (1.0 - 26.0 / 75.0)).abs() < 1.0e-4, "{}", target.body);
    assert_eq!(f.rejected_hits(), 0);
    // the pistol it no longer carries
    f.report(&[f.hit_on_target()]);
    wait_until("the rejection", || f.rejected_hits() == 1);
    assert_eq!(f.fighter(TARGET).hurt_count, 1);
}

#[test]
fn a_plasma_rifle_is_held_to_what_its_heat_lets_it_fire() {
    // (a target that sixty hits do not kill, so that every report is judged on its rate alone)
    let Some(f) = armed_fight("hit-plasma", PLASMA_RIFLE, 1.0e4) else { return };
    let bolt = HitReport { damage: PLASMA_RIFLE_DAMAGE, ..f.hit_on_target() };
    // sixty-four at once: 10 a second at most, but a gauge of heat is 12 shots and what it loses in 3 seconds
    // is 11 more: 23.75 in 3 s, so at twice that the bucket holds 47 bolts
    f.report(&[bolt; 64]);
    wait_until("the verdicts", || f.rejected_hits() >= 15);
    let rejected = f.rejected_hits();
    assert!((16..=18).contains(&rejected), "47 of 64 pass: {rejected} were rejected");
    assert_eq!(f.fighter(TARGET).hurt_count as u64, 64 - rejected);
    // a bolt that has flown a few units has not slowed much: it deals most of its 12 to 14
    let target = f.fighter(TARGET);
    let dealt = (1.0 - target.shield) * 75.0 * 1.0e4 / f.fighter(TARGET).hurt_count as f32;
    assert!((9.0..=14.0).contains(&dealt), "each bolt dealt {dealt} on average");
}

#[test]
fn a_plasma_pistols_two_triggers_each_deal_their_own_damage_and_the_charged_one_is_rare() {
    // (a target that ten overcharged bolts do not kill, so that every report is judged on its rate alone)
    let Some(f) = armed_fight("hit-plasma-pistol", PLASMA_PISTOL, 1.0e4) else { return };
    // a bolt of the first trigger: 16 to 20 of the target's shield (at most the 10 minimum, if it has slowed to nothing)
    f.report(&[HitReport { damage: PLASMA_PISTOL_DAMAGE, ..f.hit_on_target() }]);
    wait_until("the bolt", || f.fighter(TARGET).hurt_count == 1);
    let bolt = (1.0 - f.fighter(TARGET).shield) * 75.0 * 1.0e4;
    assert!((10.0..=20.0).contains(&bolt), "the bolt dealt {bolt}");
    // the charged bolt of the second: 70, always
    let before = f.fighter(TARGET).shield;
    f.report(&[HitReport { damage: PLASMA_PISTOL_CHARGED_DAMAGE, ..f.hit_on_target() }]);
    wait_until("the charged bolt", || f.fighter(TARGET).hurt_count == 2);
    let charged = (before - f.fighter(TARGET).shield) * 75.0 * 1.0e4;
    assert!((charged - 70.0).abs() < 1.0e-2, "the charged bolt dealt {charged}");
    // a charge takes 18 ticks and a heat gauge only lets one go a second: ten at once are mostly too many
    let bolt = HitReport { damage: PLASMA_PISTOL_CHARGED_DAMAGE, ..f.hit_on_target() };
    f.report(&[bolt; 10]);
    // (two reports came before: all twelve are judged when the table says so)
    wait_until("the verdicts", || f.fighter(TARGET).hurt_count as u64 + f.rejected_hits() == 12);
    let rejected = f.rejected_hits();
    assert!((4..=6).contains(&rejected), "about 5 of 10 pass: {rejected} were rejected");
}

/// The report of the explosion `damage` on the target where it stands: at the target's feet, a little below the
/// middle of it, at the scale the engine would give it.
fn blast_on_target(f: &Fight, damage: u16, scale: f32) -> HitReport {
    let at = f.position(TARGET);
    HitReport { damage, scale, material: -1, origin: [at[0], at[1], at[2] + 0.2], ..f.hit_on_target() }
}

#[test]
fn a_rockets_blast_kills_the_target_it_reaches_and_one_it_does_not_reach_is_refused() {
    let Some(f) = armed_fight_apart("hit-rocket", 6.0, ROCKET_LAUNCHER, 1.0) else { return };
    // an explosion half a unit off the target's middle, the width of a body: all 300 of it (a player has 150)
    f.report(&[blast_on_target(&f, ROCKET_BLAST, 1.0)]);
    wait_until("the death", || f.owner.standings()[&TARGET].state == STATE_DEAD);
    assert_eq!(f.owner.standings()[&SHOOTER].score, 1, "the kill is the shooter's");
    assert_eq!(f.rejected_hits(), 0);
    // ... and one whose epicentre is nowhere near is no hit
    wait_until("the respawn", || f.owner.standings()[&TARGET].state == STATE_ALIVE);
    let mut far = blast_on_target(&f, ROCKET_BLAST, 1.0);
    far.origin[0] += 5.0;
    f.report(&[far]);
    wait_until("the refusal", || f.rejected_hits() == 1);
    assert_eq!(f.fighter(TARGET).hurt_count, 0);
}

#[test]
fn a_needler_needle_stuck_to_a_target_hurts_it_and_seven_make_a_blast_of_sixty() {
    let Some(f) = armed_fight_apart("hit-needler", 6.0, NEEDLER, 1.0) else { return };
    f.report(&[HitReport { damage: NEEDLER_ATTACHED_DAMAGE, material: -1, scale: 1.0, ..f.hit_on_target() }]);
    wait_until("the needle", || f.fighter(TARGET).hurt_count == 1);
    assert!((f.fighter(TARGET).shield - (1.0 - 10.0 / 75.0)).abs() < 1.0e-5);
    f.report(&[blast_on_target(&f, NEEDLER_BLAST, 1.0)]);
    wait_until("the blast", || f.fighter(TARGET).hurt_count == 2);
    // 60 more of the 65 that are left of the shield
    assert!((f.fighter(TARGET).shield - 5.0 / 75.0).abs() < 1.0e-4, "{}", f.fighter(TARGET).shield);
    assert_eq!(f.rejected_hits(), 0);
}
