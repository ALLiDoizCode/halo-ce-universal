//! What one tick of the simulation costs at 500 players, by stage, in CPU time that does not depend on
//! SpacetimeDB, a socket or the machine's load much (issue #48): the module's tick (`halo-match-module`'s
//! `tick` reducer) run natively over in-memory stores, in the same order, on a crowd of 500 walking
//! players on Blood Gulch who shoot at enemies within 25 wu (about 0.5 shots a second each), die, and
//! respawn.
//!
//! It leaves out what the module pays for the tables (the host calls that read and write rows, the commit
//! and the subscriptions): those are measured inside SpacetimeDB by `check/run.sh` with
//! `MODULE_FEATURES=stage-timing` (see `check/stages.py`). What it shows is the simulation's own share.
//!
//!     HALO_MAP_DIR=<the folder with bloodgulch.map> \
//!     cargo test --release --test tick_cost -- --ignored --nocapture
//!
//! The figures are the mean over the ticks after the first 100 (the crowd has spread out by then), of
//! the thread's CPU time (`/proc/thread-self/schedstat`), best of three runs. `TICKS` (default 600) and
//! `PLAYERS` (500) set the run.

use std::path::{Path, PathBuf};
use std::time::Instant;

use halo_match_driver::walkers::Walkers;
use halo_sim::combat::{self, CombatStore, HitEvent, HitReport, MemoryCombat, Trails};
use halo_sim::items::{self, MemoryItems};
use halo_sim::pickups::{self, ItemEvent};
use halo_sim::rules::{self, enter_placed, GameEvent, GameStore, MemoryGame, Rules};
use halo_sim::{MapData, MemoryStore, Rng, Store};

/// This thread's CPU time so far, in nanoseconds.
fn cpu_ns() -> u64 {
    std::fs::read_to_string("/proc/thread-self/schedstat")
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse().ok())
        .unwrap_or(0)
}

const STAGES: [&str; 6] = [
    "combat::resolve (hits)",
    "rules::play (deaths, moves, spawns)",
    "deaths' weapons, spawns' kits",
    "items::tick (items, pickups)",
    "trails",
    "whole tick",
];

/// A tick's thread CPU time by stage, in ns.
fn run(map_dir: &Path, players: u16, ticks: u64, skip: u64) -> ([f64; 6], u64) {
    let halo_map = halo_map::HaloMap::from_path(map_dir.join("bloodgulch.map")).expect("load the map");
    let anchors: Vec<[f32; 3]> = halo_map.player_starts.iter().map(|s| s.position).collect();
    let map = MapData::from(halo_map);
    let (mut walkers, _) = Walkers::new(map.clone(), &anchors, players, 1);
    let mut store: MemoryStore = walkers.mirror.clone();
    let mut game = MemoryGame::new(Rules { score_limit: 1_000_000, ..Rules::team_slayer() });
    let mut combat = MemoryCombat::new();
    let mut items = MemoryItems::new();
    let mut trails = Trails::new();
    for id in 0..players {
        let p = store.player(id).unwrap();
        assert!(enter_placed(&mut game, id, (id % 2) as u8, p.position, p.yaw));
        combat::spawn(&mut combat, &mut trails, &map, id, 0);
        let loadout = combat.fighter(id).unwrap().loadout;
        pickups::on_spawn(&mut items, &map, id, &loadout);
    }
    rules::begin(&mut game, 0);

    let mut shot_at = vec![0u64; players as usize];
    let mut totals = [0u64; 6];
    let mut counted = 0u64;
    let (mut hits, mut deaths_total) = (0u64, 0u64);
    for tick in 1..=ticks {
        let inputs = walkers.next_inputs();
        walkers.apply(&inputs, tick);
        // the crowd's shots (not timed): each player with an enemy within 25 wu, every 2 s or so
        let mut reports: Vec<(u16, HitReport)> = Vec::new();
        let positions: Vec<(u16, [f32; 3], u8)> = store
            .players()
            .iter()
            .filter(|p| game.contestant(p.id).is_some_and(|c| c.is_alive()))
            .map(|p| (p.id, p.position, (p.id % 2) as u8))
            .collect();
        for &(id, at, team) in &positions {
            if tick < shot_at[id as usize] {
                continue;
            }
            let near = positions.iter().find(|&&(o, p, t)| {
                t != team && o != id && (0..3).map(|i| (p[i] - at[i]).powi(2)).sum::<f32>() <= 25.0 * 25.0
            });
            let Some(&(target, target_at, _)) = near else { continue };
            shot_at[id as usize] = tick + 30 + (id as u64 * 7 + tick) % 20;
            let weapon = combat.fighter(id).map(|f| f.loadout.weapons[0]);
            let damage = weapon.and_then(|w| {
                map.combat
                    .weapon(w)?
                    .triggers
                    .iter()
                    .find_map(|t| t.projectile.as_ref()?.impact_damage.map(|d| d.tag_index))
            });
            if let Some(damage) = damage {
                reports.push((
                    id,
                    HitReport {
                        target,
                        damage,
                        material: 1,
                        scale: 1.0,
                        host_tick: (tick - 1) as u32,
                        origin: [target_at[0], target_at[1], target_at[2] + 0.3],
                        target_position: target_at,
                    },
                ));
            }
        }

        let mut rng = Rng::seeded(tick);
        let started = cpu_ns();
        let mut mark = started;
        let mut stage = [0u64; 6];
        let mut lap = |i: usize, mark: &mut u64| {
            let now = cpu_ns();
            stage[i] = now - *mark;
            *mark = now;
        };

        let dealt = combat::resolve(&store, &game, &mut combat, &trails, &map, &mut rng, tick, &reports);
        lap(0, &mut mark);
        let mut deaths: Vec<rules::Death> = Vec::new();
        for event in &dealt.events {
            if let HitEvent::Hit { .. } = event {
                hits += 1;
            }
        }
        deaths.extend(dealt.deaths);
        deaths_total += deaths.len() as u64;
        let outcome = rules::play(&mut store, &mut game, &map, &mut rng, tick, &deaths, &inputs);
        lap(1, &mut mark);
        let mut item_events: Vec<ItemEvent> = Vec::new();
        for event in &outcome.events {
            if let GameEvent::Died { victim, .. } = event {
                pickups::on_death(&mut items, &mut combat, &store, &map, &mut rng, tick, *victim, &mut item_events);
            }
        }
        for event in &outcome.events {
            if let GameEvent::Spawned { player, .. } = event {
                combat::spawn(&mut combat, &mut trails, &map, *player, tick);
                if let Some(f) = combat.fighter(*player) {
                    pickups::on_spawn(&mut items, &map, *player, &f.loadout);
                }
            }
        }
        lap(2, &mut mark);
        item_events.extend(items::tick(&mut items, &mut combat, &store, &game, &map, &mut rng, tick, &[]));
        lap(3, &mut mark);
        trails.record(tick, store.players().iter().map(|p| (p.id, p.position)));
        lap(4, &mut mark);
        stage[5] = cpu_ns() - started;
        if tick > skip {
            counted += 1;
            for (t, s) in totals.iter_mut().zip(stage) {
                *t += s;
            }
        }
    }
    let mut mean = [0.0; 6];
    for (m, t) in mean.iter_mut().zip(totals) {
        *m = t as f64 / counted.max(1) as f64;
    }
    eprintln!("  {hits} hits, {deaths_total} deaths over {ticks} ticks");
    (mean, counted)
}

#[test]
#[ignore = "a timed run: cargo test --release --test tick_cost -- --ignored --nocapture"]
fn one_tick_at_500_players_by_stage_in_cpu_time() {
    let Some(dir) = std::env::var_os("HALO_MAP_DIR").map(PathBuf::from) else {
        eprintln!("HALO_MAP_DIR is not set: skipping, this test needs the game's own maps");
        return;
    };
    let players: u16 = std::env::var("PLAYERS").ok().and_then(|v| v.parse().ok()).unwrap_or(500);
    let ticks: u64 = std::env::var("TICKS").ok().and_then(|v| v.parse().ok()).unwrap_or(600);
    let mut best: Option<[f64; 6]> = None;
    for attempt in 0..3 {
        let wall = Instant::now();
        let (mean, counted) = run(&dir, players, ticks, 100);
        eprintln!(
            "run {attempt}: {players} players, {counted} ticks counted, {:.1} s wall, whole tick {:.3} ms CPU",
            wall.elapsed().as_secs_f64(),
            mean[5] / 1e6
        );
        if best.is_none_or(|b| mean[5] < b[5]) {
            best = Some(mean);
        }
    }
    let best = best.unwrap();
    eprintln!("one tick at {players} players, native, in-memory stores, mean thread CPU time of the best run:");
    for (name, ns) in STAGES.iter().zip(best) {
        eprintln!("  {name:<40} {:>7.3} ms", ns / 1e6);
    }
}
