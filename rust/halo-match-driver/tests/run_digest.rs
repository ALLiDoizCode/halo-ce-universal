//! A scripted run of the match module on the real Blood Gulch, reduced to a digest of every table the
//! simulation writes, to show that a change to how the module or the simulation does its work did not
//! change what it computes (issue #48): run it on the commit before and the commit after and compare
//! the digests (and the files `DIGEST_OUT` names, which hold the tables as text).
//!
//!     HALO_STDB_BIN=<SpacetimeDB 2.10.x folder> HALO_MAP_DIR=<folder with bloodgulch.map> \
//!     DIGEST_OUT=/some/where/after.txt \
//!     cargo test --release --test run_digest -- --ignored --nocapture
//!
//! 300 walkers (`halo_match_driver::walkers`, seeded) walk Blood Gulch for 450 ticks, one batch of inputs
//! for each tick, sent as soon as the previous tick is seen; every fourth tick the owner kills a player
//! (so that deaths, the weapons they drop, respawns beside starts and the items on the ground are all in
//! it). Which tick a batch lands in depends on the machine keeping up, so a run on a busy machine can
//! differ from another: compare runs that agree with themselves (run each side twice).

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use halo_match_driver::server::{build_module, stdb_bin_dir, Server};
use halo_match_driver::walkers::Walkers;
use halo_sim::rules::Rules;
use halo_sim::MapData;

const PLAYERS: u16 = 300;
const TICKS: u64 = 450;

#[test]
#[ignore = "a scripted run for comparing two builds: see the module's documentation"]
fn the_tables_after_a_scripted_run_of_the_module_on_blood_gulch() {
    let (Some(bin), Some(maps)) = (stdb_bin_dir(), std::env::var_os("HALO_MAP_DIR").map(PathBuf::from)) else {
        eprintln!("HALO_STDB_BIN and HALO_MAP_DIR are not both set: skipping, this test needs the game's own maps");
        return;
    };
    let halo_map = halo_map::HaloMap::from_path(maps.join("bloodgulch.map")).expect("load the map");
    let anchors: Vec<[f32; 3]> = halo_map.player_starts.iter().map(|s| s.position).collect();
    let map = MapData::from(halo_map);
    let blob = map.to_bytes();

    let server = Server::start(&bin);
    server.publish(&build_module(), "digest");
    let owner = server.connect("digest");
    owner.load_map(blob).unwrap();
    owner.set_capacity(PLAYERS + 10).unwrap();
    owner.set_game(&Rules { score_limit: 100_000, respawn_ticks: 60, ..Rules::team_slayer() }).unwrap();
    let (mut walkers, spawn) = Walkers::new(map, &anchors, PLAYERS, 1);
    owner.add_players(&spawn).unwrap();
    owner.start();

    let mut seen = owner.wait_for_tick(1, Duration::from_secs(10));
    let first = seen.marker.tick;
    for step in 0..TICKS {
        walkers.sync_with_server(seen.players.values());
        let inputs = walkers.next_inputs();
        walkers.apply(&inputs, seen.marker.tick + 1);
        if step % 4 == 0 {
            let victim = ((step * 7) % PLAYERS as u64) as u16;
            let killer = ((step * 13 + 1) % PLAYERS as u64) as u16;
            owner.report_death(victim, (killer != victim).then_some(killer)).unwrap();
        }
        owner.submit(&inputs);
        seen = owner.wait_for_tick(seen.marker.tick + 1, Duration::from_secs(10));
    }
    owner.stop();
    // (what the last tick wrote has reached the owner's copy of the tables by now)
    std::thread::sleep(Duration::from_millis(500));

    let mut text = String::new();
    writeln!(text, "first tick seen {first}").unwrap();
    for row in owner.players().values() {
        writeln!(text, "{row:?}").unwrap();
    }
    for row in owner.standings().values() {
        writeln!(text, "{row:?}").unwrap();
    }
    writeln!(text, "{:?}", owner.game()).unwrap();
    for row in owner.fighters().values() {
        writeln!(text, "{row:?}").unwrap();
    }
    for row in owner.items().values() {
        writeln!(text, "{row:?}").unwrap();
    }
    for row in owner.kits().values() {
        writeln!(text, "{row:?}").unwrap();
    }
    for row in owner.powerups().values() {
        writeln!(text, "{row:?}").unwrap();
    }
    let marker = owner.marker().unwrap();
    writeln!(text, "marker: rejected_total {} hits_total {}", marker.rejected_total, marker.hits_total).unwrap();
    let digest = text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01B3));
    eprintln!("digest {digest:016x} of {} bytes, {} players", text.len(), owner.players().len());
    if let Some(path) = std::env::var_os("DIGEST_OUT") {
        std::fs::write(path, &text).unwrap();
    }
}
