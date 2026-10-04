//! Whole Slayer matches on the real Blood Gulch and Sidewinder, run by
//! `halo-server` against a real local SpacetimeDB, with simulated players: more
//! players than starting locations (so that some spawn beside a start and
//! everyone ends up in the world at once, somewhere they can stand and clear of everyone else), kills
//! credited through the server's death path, the match ending at the score
//! limit and at the time limit with a final scoreboard, the rotation moving
//! on, and the cap refusing the player beyond it.
//!
//! They need the game's own maps (`HALO_MAP_DIR`, a folder with
//! `bloodgulch.map` and the rest) and `HALO_STDB_BIN`, and skip themselves
//! without them.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use halo_match_driver::server::{stdb_bin_dir, Server as Stdb};
use halo_match_driver::{MatchClient, PlayerClient};
use halo_server::admin::Admin;
use halo_server::fixtures::{free_udp_pair, modules, scratch};
use halo_server::maps::MapFiles;
use halo_server::servers::Log;

fn wait_for<T>(what: &str, seconds: u64, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    loop {
        if let Some(found) = check() {
            return found;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_blood_gulch_match_spawns_everyone_ends_at_its_score_and_time_limits_and_the_rotation_moves_on() {
    let (Some(bin), Some(maps)) = (stdb_bin_dir(), std::env::var_os("HALO_MAP_DIR").map(PathBuf::from)) else {
        eprintln!("HALO_STDB_BIN and HALO_MAP_DIR are not both set: skipping, this test needs the game's own maps");
        return;
    };
    let dir = scratch("real-maps");
    let stdb = Stdb::start(&bin);
    let url = stdb.uri();
    std::fs::write(dir.join("owner.token"), &stdb.owner().token).unwrap();
    let (root_wasm, match_wasm) = modules();
    let udp = free_udp_pair();
    // Blood Gulch, capped at 24 (it has 16 starting locations for Slayer), to 2 kills or 14 s; then
    // Sidewinder with the map's own cap; then Blood Gulch again
    let config = halo_server::config::Config::parse(
        &format!(
            r#"
[spacetimedb]
url = "{url}"
owner_token_file = {token:?}
[root]
module = {root_wasm:?}
[match]
module = {match_wasm:?}
maps_dir = {maps:?}
[[server]]
id = "lounge"
bind = "127.0.0.1:{udp}"
budget = 90000
send_threads = 2
log_secs = 0
handover_secs = 1
end_secs = 3
[[server.rotation]]
map = "bloodgulch"
capacity = 24
seconds = 14
score_limit = 2
respawn_seconds = 3
suicide_penalty_seconds = 0
wave_seconds = 3
[[server.rotation]]
map = "sidewinder"
game_type = "team_slayer"
seconds = 20
[[server.rotation]]
map = "bloodgulch"
capacity = 24
seconds = 10
score_limit = 0
wave_seconds = 3
"#,
            token = dir.join("owner.token"),
        ),
        &dir,
    )
    .expect("a valid configuration");
    let (log, lines) = Log::keeping();
    let running = halo_server::start(config, Arc::new(MapFiles { dir: maps }), log).expect("the server starts");

    // ---- match 1: Blood Gulch, Slayer
    let row = wait_for("the first match", 60, || running.root().servers().into_iter().find(|s| !s.database.is_empty()));
    assert_eq!((row.map.as_str(), row.capacity, row.match_number), ("bloodgulch", 24, 1));
    let owner = MatchClient::connect_as(&url, &row.database, Some(&stdb.owner().token));
    let admin = Admin::new(&url).unwrap();
    let players: Vec<PlayerClient> = (0..24)
        .map(|i| {
            let account = admin.new_identity().unwrap();
            let client = PlayerClient::connect_unsubscribed(&url, &row.database, &account.token);
            client.join([i as u8 + 1; 32]).unwrap();
            client
        })
        .collect();
    // the cap: the 25th is refused, and told why
    let extra = PlayerClient::connect_unsubscribed(&url, &row.database, &admin.new_identity().unwrap().token);
    let refusal = extra.join([99; 32]).expect_err("the match is full");
    assert!(refusal.contains("full: the match has its 24 players"), "told: {refusal}");

    // more players than the map's 16 starting locations for Slayer: the ones who find no free start
    // spawn beside one at once, and nobody is told to wait for a wave
    wait_for("the standings", 20, || (owner.standings().len() == 24).then_some(()));
    assert!(owner.standings().values().all(|s| s.state != 2), "somebody is told to wait for a wave");
    wait_for("everyone to spawn", 10, || owner.standings().values().all(|s| s.state == 0).then_some(()));
    let rows = owner.players();
    assert_eq!(rows.len(), 24);
    let map = halo_sim::MapData::from(halo_map::HaloMap::from_path(row_map_path(&row.map)).unwrap());
    let points: Vec<[f32; 3]> = rows.values().map(|p| [p.x, p.y, p.z]).collect();
    for (i, a) in points.iter().enumerate() {
        let footing = halo_sim::walk::footing(&map, *a);
        assert!(footing.penetration <= 0.02 && footing.supported, "a player was spawned at {a:?}: {footing:?}");
        for b in &points[i + 1..] {
            let d = ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
            assert!(d >= 0.4, "two players {d} apart");
        }
    }

    // kills, credited through the server's death path, to the score limit of 2
    owner.report_death(1, Some(0)).unwrap();
    wait_for("the first kill", 10, || (owner.standings()[&0].score == 1).then_some(()));
    assert_eq!(owner.standings()[&1].deaths, 1);
    assert_eq!(owner.game().unwrap().ending, 0);
    owner.report_death(2, Some(0)).unwrap();
    wait_for("the end", 10, || (owner.game().unwrap().ending == 1).then_some(()));
    assert_eq!((owner.game().unwrap().winner_kind, owner.game().unwrap().winner), (1, 0));

    // ---- the rotation moves on to Sidewinder (Team Slayer, the map's own cap) after the final scoreboard
    let ended = Instant::now();
    let second = wait_for("the second match", 60, || {
        running.root().servers().into_iter().find(|s| s.match_number == 2 && !s.database.is_empty())
    });
    assert!(ended.elapsed() >= Duration::from_secs(2), "the final scoreboard was up for {:?}", ended.elapsed());
    assert_eq!((second.map.as_str(), second.game_type.as_str(), second.capacity), ("sidewinder", "team_slayer", 300));
    let owner2 = MatchClient::connect_as(&url, &second.database, Some(&stdb.owner().token));
    let teams: Vec<PlayerClient> = (0..4)
        .map(|i| {
            let client =
                PlayerClient::connect_unsubscribed(&url, &second.database, &admin.new_identity().unwrap().token);
            client.join([i as u8 + 1; 32]).unwrap();
            client
        })
        .collect();
    wait_for("the team game's players", 20, || (owner2.standings().len() == 4).then_some(()));
    let game = owner2.game().unwrap();
    assert!(game.teams && game.score_limit == 50, "Team Slayer as the engine has it: {game:?}");
    // a kill of a teammate costs, of an enemy scores: the roster alternates teams, so 0 and 2 are red
    wait_for("spawned", 20, || owner2.standings().values().all(|s| s.state == 0).then_some(()));
    owner2.report_death(2, Some(0)).unwrap();
    wait_for("the betrayal", 10, || (owner2.standings()[&0].score == -1).then_some(()));
    assert_eq!((owner2.game().unwrap().red_score, owner2.game().unwrap().blue_score), (-1, 0));
    owner2.report_death(1, Some(0)).unwrap();
    wait_for("the kill", 10, || (owner2.standings()[&0].score == 0).then_some(()));
    assert_eq!(owner2.game().unwrap().red_score, 0);
    drop(teams);

    // ---- match 3: Blood Gulch again, no score limit: it ends at its time limit
    let third = wait_for("the third match", 120, || {
        running.root().servers().into_iter().find(|s| s.match_number == 3 && !s.database.is_empty())
    });
    assert_eq!((third.map.as_str(), third.capacity), ("bloodgulch", 24));
    let owner3 = MatchClient::connect_as(&url, &third.database, Some(&stdb.owner().token));
    let _third_players: Vec<PlayerClient> = (0..3)
        .map(|i| {
            let client =
                PlayerClient::connect_unsubscribed(&url, &third.database, &admin.new_identity().unwrap().token);
            client.join([i as u8 + 1; 32]).unwrap();
            client
        })
        .collect();
    wait_for("the standings", 20, || (owner3.standings().len() == 3).then_some(()));
    owner3.report_death(1, Some(0)).unwrap();
    wait_for("the kill", 10, || (owner3.standings()[&0].score == 1).then_some(()));
    // (its 10 s are counted from when the list named it)
    wait_for("the time limit", 30, || (owner3.game().unwrap().ending == 2).then_some(()));
    let game = owner3.game().unwrap();
    assert_eq!((game.ending, game.winner_kind, game.winner), (2, 1, 0), "the time limit, won by the top score");
    wait_for("the fourth match", 60, || {
        running.root().servers().into_iter().find(|s| s.match_number == 4 && !s.database.is_empty())
    });

    let log = lines.lock().unwrap().join("\n");
    for wanted in [
        "match 1 (bloodgulch) has ended: its score limit was reached, won by Player 0",
        "match 3 (bloodgulch) has ended: its time is up, won by Player 0",
        "is over: its score limit was reached",
        "is over: its time is up",
        "(the map's own cap)",
    ] {
        assert!(log.contains(wanted), "the log has no {wanted:?}:\n{log}");
    }
    running.stop().unwrap();
    drop(players);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Where the map of that name is, as `HALO_MAP_DIR` has it.
fn row_map_path(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("HALO_MAP_DIR").expect("HALO_MAP_DIR")).join(format!("{name}.map"))
}
