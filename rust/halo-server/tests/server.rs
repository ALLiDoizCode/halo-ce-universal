//! The orchestration against a real local SpacetimeDB: what an operator and a
//! player observe. A server is run from a configuration file; its rotation,
//! the list, the bans and the databases are read the way a player's client and
//! an operator's tools read them. They need `HALO_STDB_BIN` (a SpacetimeDB
//! 2.10.x release directory) and skip themselves without it; no game data: the
//! maps are a flat floor.

use std::net::UdpSocket;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use halo_match_driver::root_bindings::Server as ServerRow;
use halo_match_driver::server::{stdb_bin_dir, Server as Stdb};
use halo_match_driver::{MatchClient, PlayerClient};
use halo_server::admin::Admin;
use halo_server::fixtures::{modules, register_at_root, scratch, test_config, FlatFloors};
use halo_server::root::Root;
use halo_server::servers::Log;
use halo_wire::datagram::{ClientMessage, ServerMessage};
use spacetimedb_sdk::Identity;

const KEY: [u8; 32] = [7; 32];

/// One test at a time: each starts a SpacetimeDB and a few gateways.
fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

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

fn the_row(root: &Root) -> Option<ServerRow> {
    root.servers().into_iter().find(|s| s.id == "lounge" && !s.database.is_empty())
}

/// A player's SpacetimeDB identity on the server, with the token that proves it.
fn new_player(uri: &str) -> halo_server::admin::Account {
    Admin::new(uri).unwrap().new_identity().unwrap()
}

fn identity(account: &halo_server::admin::Account) -> Identity {
    Identity::from_hex(&account.identity).unwrap()
}

/// The first datagram a gateway at `gateway` answers to a Hello for `player`.
fn say_hello(gateway: &str, player: u16) -> Option<ServerMessage> {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
    for _ in 0..10 {
        socket.send_to(&ClientMessage::Hello { player }.encode(), gateway).unwrap();
        let mut buf = [0u8; 1500];
        if let Ok((len, _)) = socket.recv_from(&mut buf) {
            return ServerMessage::decode(&buf[..len]);
        }
    }
    None
}

fn is_gone(uri: &str, database: &str) -> bool {
    MatchClient::try_connect_as(uri, database, None).is_err()
}

const TWO_MAPS: &str = r#"
[[server.rotation]]
map = "alpha"
game_type = "slayer"
variant = "fast"
capacity = 3
seconds = 5
[[server.rotation]]
map = "beta"
capacity = 2
seconds = 5
budget = 30000
"#;

#[test]
fn a_server_cycles_through_its_rotation_unattended_each_match_in_a_fresh_database() {
    let _serial = serial();
    let Some(bin) = stdb_bin_dir() else {
        eprintln!("HALO_STDB_BIN is not set: skipping, this test needs a SpacetimeDB 2.10.x release");
        return;
    };
    let dir = scratch("rotation");
    let url = format!("http://127.0.0.1:{}", halo_server::fixtures::free_tcp_port());
    let config = test_config(&dir, &url, Some(&bin), &[("lounge", TWO_MAPS)]);
    let (log, lines) = Log::keeping();
    let running = halo_server::start(config, Arc::new(FlatFloors), log).expect("the server starts");

    // watch the list as a player's client does, through three matches
    let mut seen: Vec<ServerRow> = Vec::new();
    let mut joined = false;
    wait_for("three matches", 60, || {
        let row = the_row(running.root())?;
        if seen.last().is_none_or(|last| last.match_number != row.match_number) {
            seen.push(row.clone());
        }
        // the first match: a player takes a seat, and the list counts them
        if !joined && row.match_number == 1 {
            let player = new_player(&url);
            let client = PlayerClient::connect(&url, &row.database, &player.token);
            client.join(KEY).unwrap();
            let mut counted = false;
            wait_for("the player count", 10, || {
                counted = the_row(running.root()).is_some_and(|r| r.players == 1);
                counted.then_some(())
            });
            // the gateway of this match answers a Hello for the seat with a challenge
            assert!(
                matches!(say_hello(&row.gateway, 0), Some(ServerMessage::Challenge(_))),
                "the gateway at {} did not answer for the seat",
                row.gateway
            );
            joined = true;
        }
        (row.match_number >= 3).then_some(())
    });

    let maps: Vec<&str> = seen.iter().map(|r| r.map.as_str()).collect();
    assert_eq!(maps, ["alpha", "beta", "alpha"], "the rotation, in order and from the top again");
    let databases: Vec<&String> = seen.iter().map(|r| &r.database).collect();
    assert!(databases[0] != databases[1] && databases[1] != databases[2] && databases[0] != databases[2]);
    assert_eq!((seen[0].capacity, seen[1].capacity), (3, 2), "the cap of each map");
    assert_eq!((seen[0].game_type.as_str(), seen[0].variant.as_str()), ("slayer", "fast"));
    assert_eq!(seen[0].title, "Server lounge");
    let ports: Vec<&str> = seen.iter().map(|r| r.gateway.rsplit(':').next().unwrap()).collect();
    assert!(ports[0] != ports[1] && ports[0] == ports[2], "the gateway's port alternates: {ports:?}");

    // the old matches' databases are gone, the new one is fresh
    wait_for("the first match's database to be deleted", 20, || is_gone(&url, databases[0]).then_some(()));
    let latest = the_row(running.root()).unwrap();
    let fresh = MatchClient::connect(&url, &latest.database);
    assert_eq!(fresh.marker().unwrap().players, 0, "no one is carried over into the next match");
    assert!(fresh.seats().is_empty());

    // what the log tells the operator
    let log = lines.lock().unwrap().join("\n");
    for wanted in ["tick ", "players ", "out ", "rejected moves ", "is over: its time is up", "deleted"] {
        assert!(log.contains(wanted), "the log has no {wanted:?}:\n{log}");
    }
    assert!(log.contains("players 1/3"), "the player count in the log:\n{log}");

    running.stop().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_ban_reaches_the_running_match_and_every_match_after_it() {
    let _serial = serial();
    let Some(bin) = stdb_bin_dir() else {
        eprintln!("HALO_STDB_BIN is not set: skipping, this test needs a SpacetimeDB 2.10.x release");
        return;
    };
    let dir = scratch("bans");
    let stdb = Stdb::start(&bin);
    let url = stdb.uri();
    std::fs::write(dir.join("owner.token"), &stdb.owner().token).unwrap();
    let config = test_config(&dir, &url, None, &[("lounge", TWO_MAPS)]);
    let running = halo_server::start(config, Arc::new(FlatFloors), Log::new()).expect("the server starts");
    let first = wait_for("the first match", 30, || the_row(running.root()));

    let (cheat, honest) = (new_player(&url), new_player(&url));
    let cheat_client = PlayerClient::connect(&url, &first.database, &cheat.token);
    let honest_client = PlayerClient::connect(&url, &first.database, &honest.token);
    cheat_client.join(KEY).unwrap();
    honest_client.join(KEY).unwrap();
    let watcher = MatchClient::connect_as(&url, &first.database, Some(&stdb.owner().token));
    assert_eq!(watcher.seats().len(), 2);

    // the operator bans an identity in the root database
    running.root().ban(identity(&cheat), "cheating, twice").unwrap();

    // ... which takes its player out of the running match, and refuses its next join, with the reason
    wait_for("the banned player to be taken out", 10, || (watcher.seats().len() == 1).then_some(()));
    let refusal = cheat_client.join(KEY).expect_err("a banned identity cannot join");
    assert!(refusal.contains("banned: cheating, twice"), "told: {refusal}");
    honest_client.join(KEY).unwrap();
    assert_eq!(watcher.seats().len(), 1, "the other player was not touched");

    // the next match starts with the ban in force
    let second = wait_for("the second match", 30, || the_row(running.root()).filter(|r| r.match_number == 2));
    let next_cheat = PlayerClient::connect(&url, &second.database, &cheat.token);
    let refusal = next_cheat.join(KEY).expect_err("still banned in the next match");
    assert!(refusal.contains("banned: cheating, twice"), "told: {refusal}");
    let next_honest = PlayerClient::connect(&url, &second.database, &honest.token);
    next_honest.join(KEY).unwrap();

    // lifting it lets the identity in, in the running match
    running.root().unban(identity(&cheat)).unwrap();
    wait_for("the ban to be lifted", 10, || next_cheat.join(KEY).ok());
    running.stop().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_name_a_player_chose_at_the_root_is_on_the_roster_of_the_running_match_and_the_next() {
    let _serial = serial();
    let Some(bin) = stdb_bin_dir() else {
        eprintln!("HALO_STDB_BIN is not set: skipping, this test needs a SpacetimeDB 2.10.x release");
        return;
    };
    let dir = scratch("names");
    let stdb = Stdb::start(&bin);
    let url = stdb.uri();
    std::fs::write(dir.join("owner.token"), &stdb.owner().token).unwrap();
    let running =
        halo_server::start(test_config(&dir, &url, None, &[("lounge", TWO_MAPS)]), Arc::new(FlatFloors), Log::new())
            .expect("the server starts");
    let first = wait_for("the first match", 30, || the_row(running.root()));
    let (alice, bob) = (new_player(&url), new_player(&url));

    // a player says who they are at the root, then takes a seat in the match: the roster shows the name
    register_at_root(&url, &alice.token, "Alice").unwrap();
    let client = PlayerClient::connect(&url, &first.database, &alice.token);
    let watcher = MatchClient::connect_as(&url, &first.database, Some(&stdb.owner().token));
    client.join(KEY).unwrap();
    wait_for("the name on the roster", 10, || {
        (watcher.roster().values().map(|r| r.name.clone()).collect::<Vec<_>>() == ["Alice"]).then_some(())
    });
    // a name chosen after the seat is taken replaces the one on the roster
    register_at_root(&url, &alice.token, "Alice B").unwrap();
    wait_for("the new name", 10, || {
        (watcher.roster().values().next().is_some_and(|r| r.name == "Alice B")).then_some(())
    });
    // a player who never chose one is "Player <number>"
    let other = PlayerClient::connect(&url, &first.database, &bob.token);
    other.join(KEY).unwrap();
    wait_for("the second player", 10, || (watcher.roster().len() == 2).then_some(()));
    assert_eq!(watcher.roster()[&1].name, "Player 1");

    // the next match starts knowing the names
    let second = wait_for("the second match", 30, || the_row(running.root()).filter(|r| r.match_number == 2));
    let client = PlayerClient::connect(&url, &second.database, &alice.token);
    client.join(KEY).unwrap();
    let watcher = MatchClient::connect_as(&url, &second.database, Some(&stdb.owner().token));
    wait_for("the name in the next match", 10, || {
        (watcher.roster().values().map(|r| r.name.clone()).collect::<Vec<_>>() == ["Alice B"]).then_some(())
    });
    running.stop().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_full_match_says_it_is_full_and_a_match_that_cannot_start_is_not_listed() {
    let _serial = serial();
    let Some(bin) = stdb_bin_dir() else {
        eprintln!("HALO_STDB_BIN is not set: skipping, this test needs a SpacetimeDB 2.10.x release");
        return;
    };
    let dir = scratch("full");
    let stdb = Stdb::start(&bin);
    let url = stdb.uri();
    std::fs::write(dir.join("owner.token"), &stdb.owner().token).unwrap();
    // the first map cannot be loaded: the server says so, and goes on to the next
    let rotation = r#"
[[server.rotation]]
map = "nomap"
seconds = 0
"#;
    let (log, lines) = Log::keeping();
    let config = test_config(&dir, &url, None, &[("lounge", rotation)]);
    let running = halo_server::start(config, Arc::new(FlatFloors), log).expect("the server starts");
    wait_for("the failure in the log", 20, || {
        lines.lock().unwrap().iter().any(|l| l.contains("could not start a match") && l.contains("nomap")).then_some(())
    });
    assert!(the_row(running.root()).is_none(), "nothing is listed while there is nothing to join");
    running.stop().unwrap();

    let config = test_config(
        &dir,
        &url,
        None,
        &[("lounge", "[[server.rotation]]\nmap = \"alpha\"\ncapacity = 2\nseconds = 0\n")],
    );
    let running = halo_server::start(config, Arc::new(FlatFloors), Log::new()).expect("the server starts");
    let row = wait_for("the match", 30, || the_row(running.root()));
    assert_eq!(row.capacity, 2);
    let players: Vec<_> = (0..3).map(|_| new_player(&url)).collect();
    let clients: Vec<_> = players.iter().map(|p| PlayerClient::connect(&url, &row.database, &p.token)).collect();
    clients[0].join(KEY).unwrap();
    clients[1].join(KEY).unwrap();
    let refusal = clients[2].join(KEY).expect_err("the match is full");
    assert!(refusal.contains("full: the match has its 2 players"), "told: {refusal}");
    running.stop().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stopping_takes_the_server_off_the_list_and_deletes_its_match_and_a_start_clears_what_a_crash_left() {
    let _serial = serial();
    let Some(bin) = stdb_bin_dir() else {
        eprintln!("HALO_STDB_BIN is not set: skipping, this test needs a SpacetimeDB 2.10.x release");
        return;
    };
    let dir = scratch("clean");
    let stdb = Stdb::start(&bin);
    let url = stdb.uri();
    let token = stdb.owner().token.clone();
    std::fs::write(dir.join("owner.token"), &token).unwrap();
    let rotation = "[[server.rotation]]\nmap = \"alpha\"\nseconds = 0\n";

    let running =
        halo_server::start(test_config(&dir, &url, None, &[("lounge", rotation)]), Arc::new(FlatFloors), Log::new())
            .unwrap();
    let row = wait_for("the match", 30, || the_row(running.root()));
    running.stop().unwrap();
    let root = Root::connect(&url, "halo-root", &token).unwrap();
    assert!(root.servers().is_empty(), "the server left the list");
    assert!(is_gone(&url, &row.database), "its match's database is deleted");

    // a crash leaves a match and its row behind
    let admin = Admin::new(&url).unwrap();
    admin.publish("hm-left-over-1", &std::fs::read(&modules().1).unwrap(), &token).unwrap();
    root.set_server(ServerRow { id: "left-over".into(), database: "hm-left-over-1".into(), ..empty_row() }).unwrap();
    root.disconnect();
    assert!(!is_gone(&url, "hm-left-over-1"));

    let running =
        halo_server::start(test_config(&dir, &url, None, &[("lounge", rotation)]), Arc::new(FlatFloors), Log::new())
            .unwrap();
    let row = wait_for("the match", 30, || the_row(running.root()));
    assert!(is_gone(&url, "hm-left-over-1"), "the left-over match's database is deleted by the next start");
    assert!(running.root().servers().iter().all(|s| s.id == "lounge"), "and its row is gone");
    assert!(!row.database.is_empty());
    running.stop().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

fn empty_row() -> ServerRow {
    ServerRow {
        id: String::new(),
        title: String::new(),
        map: String::new(),
        game_type: String::new(),
        variant: String::new(),
        database: String::new(),
        gateway: String::new(),
        players: 0,
        capacity: 0,
        match_number: 0,
        match_seconds: 0,
        match_started_us: 0,
        updated_us: 0,
    }
}
