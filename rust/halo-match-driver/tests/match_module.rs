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
use halo_match_driver::{MatchClient, PlayerClient, SeenTick};
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
    let client = server.connect("flat");
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
    let client = server.connect("rejects");
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
    let client = server.connect("badmap");
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
        let client = server.connect("fresh");
        client.load_map(map.to_bytes()).unwrap();
        client.add_players(&[PlayerInput { player: 0, position: [0.0, 0.0, 0.01], yaw: 0.0, pitch: 0.0 }]).unwrap();
        client.start();
        client.next_tick(WAIT).unwrap();
        client.stop();
    }
    // let the commit log flush, then restart: the new process has no cached map
    std::thread::sleep(Duration::from_millis(500));
    server.restart();

    let client = server.connect("fresh");
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
    let client = server.connect("bloodgulch");
    client.load_map(map.to_bytes()).unwrap();
    let (mut walkers, spawn) = Walkers::new(map, &anchors, 500, 7);
    client.add_players(&spawn).unwrap();
    client.start();

    let seen = drive(&client, &mut walkers, 60);
    assert_eq!(seen.last().unwrap().marker.players, 500);
}

// ---- who may call what, and seats ----

const KEY_A: [u8; 32] = [0xa1; 32];
const KEY_B: [u8; 32] = [0xb2; 32];

fn at(id: u16, x: f32) -> PlayerInput {
    PlayerInput { player: id, position: [x, 0.0, 0.01], yaw: 0.0, pitch: 0.0 }
}

/// A match on the flat floor with two spawn points, ticking, and the owner's connection to it.
fn seated_match(name: &str) -> Option<(Server, MatchClient)> {
    let server = start_server(name)?;
    let owner = server.connect(name);
    owner.load_map(flat_floor_map().to_bytes()).unwrap();
    owner.set_spawn_points(&[at(0, 1.0), at(1, 2.0)]).unwrap();
    owner.start();
    Some((server, owner))
}

fn is_refused<T: std::fmt::Debug>(result: Result<T, String>, wanted: &str) {
    let e = result.expect_err("should have been refused");
    assert!(e.contains(wanted), "refused for another reason: {e}");
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + WAIT;
    while !done() {
        assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn only_the_owner_runs_the_match_and_only_the_gateway_submits_inputs() {
    let Some((server, owner)) = seated_match("guards") else { return };
    let stranger = MatchClient::connect(&server.uri(), "guards");
    let map = flat_floor_map().to_bytes();

    is_refused(stranger.load_map(map), "owner");
    is_refused(stranger.add_players(&[at(5, 0.0)]), "owner");
    is_refused(stranger.remove_players(vec![0]), "owner");
    is_refused(stranger.set_spawn_points(&[at(0, 9.0)]), "owner");
    is_refused(stranger.set_capacity(1), "owner");
    is_refused(stranger.set_away_grace(1), "owner");
    is_refused(stranger.set_gateway(server.new_account().identity()), "owner");
    is_refused(stranger.start_and_wait(), "owner");
    is_refused(stranger.stop_and_wait(), "owner");
    is_refused(stranger.reset_and_wait(), "owner");
    is_refused(stranger.submit_and_wait(&[at(0, 0.0)]), "gateway");
    assert_eq!(owner.players().len(), 0, "nothing the stranger tried did anything");
    owner.discard_ticks();
    owner.next_tick(WAIT).expect("still ticking: the stranger could not stop it");

    // until a gateway is named the owner is it
    owner.submit_and_wait(&[at(0, 0.0)]).unwrap();
    let gateway = server.new_account();
    owner.set_gateway(gateway.identity()).unwrap();
    is_refused(owner.submit_and_wait(&[at(0, 0.0)]), "gateway");
    let as_gateway = MatchClient::connect_as(&server.uri(), "guards", Some(&gateway.token));
    as_gateway.submit_and_wait(&[at(0, 0.0)]).unwrap();
    is_refused(as_gateway.start_and_wait(), "owner");
}

#[test]
fn a_player_who_joins_gets_a_seat_tied_to_their_identity() {
    let Some((server, owner)) = seated_match("seats") else { return };
    let (a, b) = (server.new_account(), server.new_account());
    let (pa, pb) = (
        PlayerClient::connect(&server.uri(), "seats", &a.token),
        PlayerClient::connect(&server.uri(), "seats", &b.token),
    );

    pa.join(KEY_A).unwrap();
    pb.join(KEY_B).unwrap();
    wait_until("both seats", || owner.seats().len() == 2 && owner.players().len() == 2);
    let (seat_a, seat_b) = (pa.seat().expect("a seat"), pb.seat().expect("a seat"));
    assert_eq!((seat_a.player, seat_b.player), (0, 1), "the lowest free ids");
    assert_eq!((seat_a.owner, seat_b.owner), (a.identity(), b.identity()));
    assert_eq!((seat_a.udp_key, seat_b.udp_key), (KEY_A.to_vec(), KEY_B.to_vec()));
    // they start at the spawn points
    let players = owner.players();
    assert_eq!((players[&0].x, players[&1].x), (1.0, 2.0));

    // joining again is the same seat, with the new key
    pa.join([0xcc; 32]).unwrap();
    wait_until("the new key", || pa.seat().is_some_and(|s| s.udp_key == [0xcc; 32]));
    assert_eq!(pa.seat().unwrap().player, 0);
    assert_eq!(owner.seats().len(), 2);

    // a key that is not 32 bytes is no key
    let c = PlayerClient::connect(&server.uri(), "seats", &server.new_account().token);
    is_refused(c.join_with(vec![1, 2, 3]), "32 bytes");
}

#[test]
fn a_full_match_and_a_match_without_spawn_points_turn_joiners_away() {
    let Some(server) = start_server("full") else { return };
    let owner = server.connect("full");
    owner.load_map(flat_floor_map().to_bytes()).unwrap();
    let joiner = |_: u8| PlayerClient::connect(&server.uri(), "full", &server.new_account().token);

    is_refused(joiner(0).join(KEY_A), "no spawn points");
    owner.set_spawn_points(&[at(0, 1.0)]).unwrap();
    owner.set_capacity(2).unwrap();
    let (x, y, z) = (joiner(1), joiner(2), joiner(3));
    x.join(KEY_A).unwrap();
    y.join(KEY_A).unwrap();
    is_refused(z.join(KEY_A), "full");
    // a seat freed is a seat to take
    x.leave().unwrap();
    z.join(KEY_B).unwrap();
    wait_until("the third seat", || z.seat().is_some());
    assert_eq!(z.seat().unwrap().player, 0, "the freed id");
}

#[test]
fn a_banned_identity_is_turned_away_with_the_reason_and_a_ban_takes_a_seated_player_out() {
    let Some((server, owner)) = seated_match("banned") else { return };
    let (bad_account, good_account) = (server.new_account(), server.new_account());
    let bad = PlayerClient::connect(&server.uri(), "banned", &bad_account.token);
    let good = PlayerClient::connect(&server.uri(), "banned", &good_account.token);
    bad.join(KEY_A).unwrap();
    good.join(KEY_B).unwrap();
    wait_until("both seats", || owner.seats().len() == 2);

    // only the owner bans
    let stranger = MatchClient::connect(&server.uri(), "banned");
    is_refused(stranger.set_ban(bad_account.identity(), "no right"), "owner");

    // a ban on a seated player takes them out of the match, and only them
    owner.set_ban(bad_account.identity(), "cheating").unwrap();
    wait_until("the banned player's seat to go", || owner.seats().len() == 1);
    assert!(owner.seats().values().all(|s| s.owner == good_account.identity()));
    assert!(!owner.players().contains_key(&0), "the player is gone with the seat");

    // joining again is refused, and says why, from any connection of the identity
    let message = bad.join(KEY_A).expect_err("a banned identity is refused");
    assert!(message.contains("banned: cheating"), "the message was {message}");
    let again = PlayerClient::connect_unsubscribed(&server.uri(), "banned", &bad_account.token);
    assert!(again.join(KEY_A).unwrap_err().contains("banned: cheating"));
    // the others are not affected
    good.join(KEY_B).unwrap();

    // lifting the ban lets them in, to the seat that is free
    owner.clear_ban(bad_account.identity()).unwrap();
    bad.join(KEY_A).unwrap();
    wait_until("the seat back", || owner.seats().len() == 2);
}

#[test]
fn a_full_match_says_it_is_full_and_how_many_it_holds() {
    let Some((server, owner)) = seated_match("fullmsg") else { return };
    owner.set_capacity(1).unwrap();
    let player = || PlayerClient::connect(&server.uri(), "fullmsg", &server.new_account().token);
    player().join(KEY_A).unwrap();
    let message = player().join(KEY_B).expect_err("no room");
    assert!(message.contains("full: the match has its 1 players"), "the message was {message}");
}

#[test]
fn a_player_who_leaves_is_removed_and_no_longer_listed() {
    let Some((server, owner)) = seated_match("leave") else { return };
    let (a, b) = (server.new_account(), server.new_account());
    let (pa, pb) = (
        PlayerClient::connect(&server.uri(), "leave", &a.token),
        PlayerClient::connect(&server.uri(), "leave", &b.token),
    );
    pa.join(KEY_A).unwrap();
    pb.join(KEY_B).unwrap();
    wait_until("both seated", || owner.players().len() == 2 && owner.seats().len() == 2);

    pa.leave().unwrap();
    wait_until("player 0 gone", || !owner.players().contains_key(&0) && !owner.seats().contains_key(&0));
    assert!(owner.players().contains_key(&1) && owner.seats().contains_key(&1), "the other player stays");
    // leaving without a seat is not an error and changes nothing
    pa.leave().unwrap();
    assert_eq!(owner.players().len(), 1);
}

#[test]
fn the_roster_names_each_player_and_shares_the_teams_out_and_forgets_who_leaves() {
    let Some((server, owner)) = seated_match("roster") else { return };
    let players: Vec<PlayerClient> = (0..5)
        .map(|_| PlayerClient::connect_unsubscribed(&server.uri(), "roster", &server.new_account().token))
        .collect();
    for (i, player) in players.iter().enumerate() {
        player.join([i as u8 + 1; 32]).unwrap();
    }
    wait_until("five on the roster", || owner.roster().len() == 5);
    let roster = owner.roster();
    // a name each, and the teams alternate: red (0) and blue (1) differ by at most one player
    assert_eq!(
        roster.values().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        ["Player 0", "Player 1", "Player 2", "Player 3", "Player 4"]
    );
    assert_eq!(roster.values().map(|r| r.team).collect::<Vec<_>>(), [0, 1, 0, 1, 0]);

    // someone on the smaller team leaves, and the next to join takes it again
    players[1].leave().unwrap();
    wait_until("player 1 gone from the roster", || !owner.roster().contains_key(&1));
    let sixth = PlayerClient::connect_unsubscribed(&server.uri(), "roster", &server.new_account().token);
    sixth.join([9; 32]).unwrap();
    wait_until("the sixth", || owner.roster().contains_key(&1));
    assert_eq!(owner.roster()[&1].team, 1, "blue had one and red three");

    // players added by the owner are on it too
    owner.add_players(&[at(7, 3.0)]).unwrap();
    wait_until("player 7 on the roster", || owner.roster().contains_key(&7));
    owner.remove_players(vec![7]).unwrap();
    wait_until("player 7 off the roster", || !owner.roster().contains_key(&7));
}

#[test]
fn a_dropped_connection_holds_the_seat_and_the_same_identity_resumes_it() {
    let Some((server, owner)) = seated_match("rejoin") else { return };
    let a = server.new_account();
    let first = PlayerClient::connect(&server.uri(), "rejoin", &a.token);
    first.join(KEY_A).unwrap();
    wait_until("the seat", || owner.seats().contains_key(&0));
    assert_eq!(owner.seats()[&0].away_since, 0);
    let before = owner.players()[&0].clone();

    first.disconnect();
    wait_until("the seat marked away", || owner.seats().get(&0).is_some_and(|s| s.away_since != 0));
    assert!(owner.players().contains_key(&0), "the player is held, not removed");

    // from a new connection, with a new key, the same identity is the same player
    let again = PlayerClient::connect(&server.uri(), "rejoin", &a.token);
    again.join(KEY_B).unwrap();
    wait_until("back", || owner.seats().get(&0).is_some_and(|s| s.away_since == 0 && s.udp_key == KEY_B.to_vec()));
    assert_eq!(owner.seats().len(), 1);
    let after = owner.players()[&0].clone();
    assert_eq!((after.x, after.y, after.z), (before.x, before.y, before.z), "they are where they were");

    // the old connection's disconnection (it may arrive late) does not undo the rejoin
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(owner.seats()[&0].away_since, 0);
}

#[test]
fn a_seat_not_come_back_to_is_freed_after_the_grace_period() {
    let Some((server, owner)) = seated_match("grace") else { return };
    owner.set_away_grace(45).unwrap(); // a second and a half
    let a = server.new_account();
    let p = PlayerClient::connect(&server.uri(), "grace", &a.token);
    p.join(KEY_A).unwrap();
    wait_until("the seat", || owner.seats().contains_key(&0));
    p.disconnect();
    wait_until("marked away", || owner.seats().get(&0).is_some_and(|s| s.away_since != 0));
    assert!(owner.players().contains_key(&0));
    wait_until("freed", || owner.seats().is_empty() && owner.players().is_empty());
}

#[test]
fn a_move_after_skipped_inputs_is_judged_by_the_time_since_the_last_one() {
    let Some((server, owner)) = seated_match("catchup") else { return };
    let p = PlayerClient::connect(&server.uri(), "catchup", &server.new_account().token);
    p.join(KEY_A).unwrap();
    wait_until("the player", || owner.players().contains_key(&0));
    let start = owner.players()[&0].updated_tick;
    // twelve ticks pass with no input at all (as if eleven were lost)
    let seen = owner.wait_for_tick(start + 12, WAIT);
    let x = seen.players[&0].x;

    // 1.0 unit: nine ticks' worth of the bound, more than one tick's (0.133)
    owner.submit_and_wait(&[at(0, x + 1.0)]).unwrap();
    let applied = loop {
        let seen = owner.next_tick(WAIT).unwrap();
        if seen.players[&0].x != x {
            break seen;
        }
    };
    assert_eq!(applied.players[&0].rejected_moves, 0);
    assert_eq!(applied.players[&0].x, x + 1.0);

    // and straight after, a move that size is too much again: one tick has passed
    owner.submit_and_wait(&[at(0, x + 2.0)]).unwrap();
    let refused = loop {
        let seen = owner.next_tick(WAIT).unwrap();
        if seen.players[&0].rejected_moves > 0 {
            break seen;
        }
    };
    assert_eq!(refused.players[&0].last_reject, REJECT_TOO_FAST);
    assert_eq!(refused.players[&0].x, x + 1.0);
}
