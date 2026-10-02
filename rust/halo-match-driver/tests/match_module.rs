//! The match module against a real local SpacetimeDB.
//!
//! These tests start their own Standalone (own port and data directory), so
//! they need a SpacetimeDB 2.10.x release unpacked somewhere, named by the
//! `HALO_STDB_BIN` environment variable (the directory holding
//! `spacetimedb-standalone` and `spacetimedb-cli`). Without it they print a
//! note and pass without testing. Tests on a real map also need
//! `HALO_MAP_DIR`; the others run on a flat floor and need no game data.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use halo_match_driver::server::{build_module, stdb_bin_dir, Server};
use halo_match_driver::walkers::Walkers;
use halo_match_driver::{MatchClient, SeenTick};
use halo_sim::fixtures::flat_floor_map;
use halo_sim::{Event, MapData, PlayerInput};

const WAIT: Duration = Duration::from_secs(20);

/// Codes of `last_reject`, as the module defines them.
const REJECT_TOO_FAST: u8 = 3;
const REJECT_THROUGH_SURFACE: u8 = 4;

fn wasm() -> &'static PathBuf {
    static WASM: OnceLock<PathBuf> = OnceLock::new();
    WASM.get_or_init(build_module)
}

/// A published, empty match on a fresh server; `None` (skip) without a server.
fn start_server(name: &str) -> Option<Server> {
    let Some(bin) = stdb_bin_dir() else {
        eprintln!("HALO_STDB_BIN is not set: skipping, this test needs a SpacetimeDB 2.10.x release");
        return None;
    };
    let server = Server::start(&bin);
    server.publish(wasm(), name);
    Some(server)
}

fn real_map(name: &str) -> Option<halo_map::HaloMap> {
    let Some(dir) = std::env::var_os("HALO_MAP_DIR") else {
        eprintln!("HALO_MAP_DIR is not set: skipping, this test needs the game's own map files");
        return None;
    };
    let path = PathBuf::from(dir).join(format!("{name}.map"));
    Some(halo_map::HaloMap::from_path(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
}

/// What a subscriber should see after a tick, given the local copy.
fn assert_matches_mirror(walkers: &Walkers, seen: &SeenTick, rejects: &BTreeMap<u16, u64>, accepted_last_tick: &[u16]) {
    use halo_sim::Store;
    let tick = seen.marker.tick;
    assert_eq!(seen.players.len(), walkers.mirror.player_ids().len(), "tick {tick}: player count");
    for id in walkers.mirror.player_ids() {
        let want = walkers.mirror.player(id).unwrap();
        let row = seen.players.get(&id).unwrap_or_else(|| panic!("tick {tick}: player {id} missing"));
        assert_eq!(
            [row.x, row.y, row.z, row.yaw, row.pitch],
            [want.position[0], want.position[1], want.position[2], want.yaw, want.pitch],
            "tick {tick}: player {id}"
        );
        assert_eq!(row.rejected_moves, rejects.get(&id).copied().unwrap_or(0), "tick {tick}: player {id} rejects");
        assert!(row.updated_tick <= tick, "tick {tick}: player {id} is from the future");
    }
    // completeness: the rows an accepted move wrote are in the same update as the marker
    for id in accepted_last_tick {
        assert_eq!(seen.players[id].updated_tick, tick, "tick {tick}: player {id}'s move is not in this tick's update");
    }
}

/// Run `ticks` ticks the way the gateway will: when a tick is complete,
/// submit the next one's inputs as one batch, and compare the tables with the
/// local copy every time. Returns the markers seen.
fn drive(client: &MatchClient, walkers: &mut Walkers, ticks: usize) -> Vec<SeenTick> {
    let mut rejects: BTreeMap<u16, u64> = BTreeMap::new();
    let mut accepted: Vec<u16> = Vec::new();
    let mut all = Vec::new();
    for _ in 0..ticks {
        let seen = client.next_tick(WAIT).expect("a tick");
        assert_matches_mirror(walkers, &seen, &rejects, &accepted);
        let inputs = walkers.next_inputs();
        let events = walkers.apply(&inputs, seen.marker.tick + 1);
        accepted.clear();
        for event in events {
            match event {
                Event::MoveAccepted { player } => accepted.push(player),
                Event::MoveRejected { player, .. } => *rejects.entry(player).or_default() += 1,
            }
        }
        client.submit(&inputs);
        all.push(seen);
    }
    all
}

#[test]
fn ticks_hold_30_hz_with_one_batch_per_tick_and_the_tables_match_a_local_copy() {
    let Some(server) = start_server("flat") else { return };
    let client = MatchClient::connect(&server.uri(), "flat");
    let map = flat_floor_map();
    client.load_map(map.to_bytes()).unwrap();
    let (mut walkers, spawn) = Walkers::new(map, &[[0.0, 0.0, 0.0]], 50, 1);
    client.add_players(&spawn).unwrap();
    let before = server.tick_metrics();
    client.start();

    let seen = drive(&client, &mut walkers, 90);

    // the tick numbers are consecutive and the server's own clock says 30 Hz
    for pair in seen.windows(2) {
        assert_eq!(pair[1].marker.tick, pair[0].marker.tick + 1);
    }
    let span_us = (seen.last().unwrap().marker.stamped_us - seen[0].marker.stamped_us) as f64;
    let hz = (seen.len() - 1) as f64 / (span_us / 1e6);
    assert!((29.0..=31.0).contains(&hz), "ticked at {hz:.2} Hz");
    assert_eq!(seen.last().unwrap().marker.players, 50);
    assert_eq!(seen.last().unwrap().marker.rejected_total, 0, "walkers on the ground are never rejected");

    // 50 players, and the module was called about once a tick, not 50 times
    let used = server.tick_metrics().since(&before);
    assert!(
        used.submits >= 88.0 && used.submits <= used.ticks + 3.0,
        "{} submits in {} ticks",
        used.submits,
        used.ticks
    );
}

#[test]
fn rejected_moves_are_counted_and_visible_per_player() {
    let Some(server) = start_server("rejects") else { return };
    let client = MatchClient::connect(&server.uri(), "rejects");
    let map = flat_floor_map();
    client.load_map(map.to_bytes()).unwrap();
    let spawn: Vec<PlayerInput> =
        (0..3).map(|id| PlayerInput { player: id, position: [id as f32, 0.0, 0.01], yaw: 0.0, pitch: 0.0 }).collect();
    client.add_players(&spawn).unwrap();
    client.start();

    const ROUNDS: u64 = 4;
    let mut last_submitted_at = 0;
    for _ in 0..ROUNDS {
        let seen = client.next_tick(WAIT).unwrap();
        last_submitted_at = seen.marker.tick;
        let p = |id: u16| seen.players[&id].clone();
        let inputs = [
            // 0 stays put, which is valid
            PlayerInput { player: 0, position: [0.0, 0.0, 0.01], yaw: 0.5, pitch: 0.0 },
            // 1 jumps 5 units in a tick
            PlayerInput { player: 1, position: [p(1).x + 5.0, 0.0, 0.01], yaw: 0.0, pitch: 0.0 },
            // 2 goes down through the floor
            PlayerInput { player: 2, position: [2.0, 0.0, -0.09], yaw: 0.0, pitch: 0.0 },
            // 7 is not in the match
            PlayerInput { player: 7, position: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0 },
        ];
        client.submit(&inputs);
    }
    // the last batch is applied by the next tick
    let seen = client.wait_for_tick(last_submitted_at + 1, WAIT);
    let rows = &seen.players;

    assert_eq!(rows[&0].rejected_moves, 0);
    assert_eq!(rows[&0].yaw, 0.5, "the accepted move was applied");
    assert_eq!((rows[&1].rejected_moves, rows[&1].last_reject), (ROUNDS, REJECT_TOO_FAST));
    assert_eq!((rows[&2].rejected_moves, rows[&2].last_reject), (ROUNDS, REJECT_THROUGH_SURFACE));
    assert_eq!(rows[&1].x, 1.0, "a rejected move leaves the player where they were");
    assert_eq!(rows[&2].z, 0.01);
    // the unknown player has no row to count on, but the tick's total does
    assert_eq!(seen.marker.rejected_total, 3 * ROUNDS);
}

#[test]
fn a_map_that_does_not_decode_is_refused_and_the_loaded_one_stays() {
    let Some(server) = start_server("badmap") else { return };
    let client = MatchClient::connect(&server.uri(), "badmap");
    let map = flat_floor_map();
    client.load_map(map.to_bytes()).unwrap();
    let mut damaged = map.to_bytes();
    damaged.truncate(damaged.len() - 3);
    assert!(client.load_map(damaged).is_err());
    assert!(client.load_map(vec![1, 2, 3]).is_err());

    // still the floor: a move down through it is rejected
    client.add_players(&[PlayerInput { player: 0, position: [0.0, 0.0, 0.01], yaw: 0.0, pitch: 0.0 }]).unwrap();
    client.start();
    client.discard_ticks();
    client.submit(&[PlayerInput { player: 0, position: [0.0, 0.0, -0.09], yaw: 0.0, pitch: 0.0 }]);
    let seen = loop {
        let seen = client.next_tick(WAIT).unwrap();
        if seen.marker.rejected_total > 0 {
            break seen;
        }
    };
    assert_eq!(seen.players[&0].last_reject, REJECT_THROUGH_SURFACE);
}

#[test]
fn a_server_restarted_with_fresh_module_memory_reloads_the_map_from_its_row() {
    let Some(mut server) = start_server("fresh") else { return };
    let map = flat_floor_map();
    {
        let client = MatchClient::connect(&server.uri(), "fresh");
        client.load_map(map.to_bytes()).unwrap();
        client.add_players(&[PlayerInput { player: 0, position: [0.0, 0.0, 0.01], yaw: 0.0, pitch: 0.0 }]).unwrap();
        client.start();
        client.next_tick(WAIT).unwrap();
        client.stop();
    }
    // let the commit log flush, then restart: the new process has no cached map
    std::thread::sleep(Duration::from_millis(500));
    server.restart();

    let client = MatchClient::connect(&server.uri(), "fresh");
    assert_eq!(client.players().len(), 1, "the match survived the restart");
    client.start();
    client.discard_ticks();
    let ok = PlayerInput { player: 0, position: [0.05, 0.0, 0.01], yaw: 1.0, pitch: 0.0 };
    client.submit(&[ok]);
    let seen = loop {
        let seen = client.next_tick(WAIT).unwrap();
        if seen.players[&0].yaw == 1.0 {
            break seen;
        }
    };
    assert_eq!(seen.players[&0].x, 0.05, "a valid move was accepted: the map was found");

    let down = PlayerInput { player: 0, position: [0.05, 0.0, -0.09], yaw: 1.0, pitch: 0.0 };
    client.submit(&[down]);
    let seen = loop {
        let seen = client.next_tick(WAIT).unwrap();
        if seen.players[&0].rejected_moves > 0 {
            break seen;
        }
    };
    assert_eq!(seen.players[&0].last_reject, REJECT_THROUGH_SURFACE, "and it still collides");
}

#[test]
fn five_hundred_players_on_blood_gulch_stay_in_step_with_a_local_copy() {
    let Some(halo_map) = real_map("bloodgulch") else { return };
    let Some(server) = start_server("bloodgulch") else { return };
    let anchors: Vec<[f32; 3]> = halo_map.player_starts.iter().map(|s| s.position).collect();
    let map = MapData::from(halo_map);
    let client = MatchClient::connect(&server.uri(), "bloodgulch");
    client.load_map(map.to_bytes()).unwrap();
    let (mut walkers, spawn) = Walkers::new(map, &anchors, 500, 7);
    client.add_players(&spawn).unwrap();
    client.start();

    let seen = drive(&client, &mut walkers, 60);
    assert_eq!(seen.last().unwrap().marker.players, 500);
}
