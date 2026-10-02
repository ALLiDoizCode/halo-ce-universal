//! The real game picking a server from the list, against a whole server on
//! the real maps: the player opens the console (here its telnet twin, which
//! runs the same commands), reads `servers`, `join`s one, plays its map, is
//! carried to the next match when the rotation moves on, and is banned.
//!
//! It needs the game's own data, so it skips itself (and says why) without
//! all of:
//!
//! - `HALO_STDB_BIN`: a SpacetimeDB 2.10.x release directory;
//! - `HALO_MAP_DIR`: the Xbox `.map` files (the server loads them);
//! - `HALO_GAME_BIN`: the game built with the library (`python configure.py
//!   --large-mode=on && ninja linux`, then `build/linux/halo`);
//! - `HALO_DATA_ROOT`: the folder that holds the game's `maps/`.
//!
//! ```text
//! HALO_STDB_BIN=~/.local/share/spacetimedb-2.10.2 HALO_MAP_DIR=<data root>/maps \
//!   HALO_GAME_BIN=<repository>/build/linux/halo HALO_DATA_ROOT=<data root> \
//!   cargo test --release --test server_list -- --nocapture
//! ```
//!
//! The game runs in a hidden window, as in `headless.rs`, which needs a display.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use halo_match_driver::server::{stdb_bin_dir, Server as Stdb};
use halo_match_driver::MatchClient;
use halo_server::fixtures::{free_tcp_port, scratch};
use halo_server::maps::MapFiles;
use halo_server::servers::Log;
use spacetimedb_sdk::Identity;

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}

/// The game's process, which never outlives the test.
struct Game(std::process::Child);

impl Drop for Game {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// What the game's log holds now.
fn log_of(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

fn wait_for_log(path: &std::path::Path, what: &str, seconds: u64, mut found: impl FnMut(&str) -> bool) -> String {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    loop {
        let log = log_of(path);
        if found(&log) {
            return log;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}; the end of the game's log:\n{}",
            log.lines().rev().take(25).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// The game's console, through its telnet twin (which takes one connection at a time).
struct Console(TcpStream);

impl Console {
    fn connect(port: u16) -> Console {
        let stream = TcpStream::connect(("127.0.0.1", port)).expect("the game's console");
        stream.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
        let mut console = Console(stream);
        console.read_until_quiet(Duration::from_secs(1));
        console
    }

    /// What the game prints until it has been quiet for `quiet`.
    fn read_until_quiet(&mut self, quiet: Duration) -> String {
        let mut out = String::new();
        let mut last = Instant::now();
        let mut buf = [0u8; 4096];
        while last.elapsed() < quiet {
            if let Ok(n) = self.0.read(&mut buf) {
                if n > 0 {
                    out.push_str(&String::from_utf8_lossy(&buf[..n]));
                    last = Instant::now();
                }
            }
        }
        out
    }

    /// Type a command at the console; what the game printed in answer.
    fn say(&mut self, command: &str) -> String {
        self.0.write_all(format!("{command}\r\n").as_bytes()).unwrap();
        self.read_until_quiet(Duration::from_millis(1500))
    }
}

#[test]
fn a_player_lists_servers_in_the_game_joins_one_follows_its_rotation_and_is_banned() {
    let (Some(stdb_bin), Some(maps), Some(game), Some(data)) =
        (stdb_bin_dir(), env_path("HALO_MAP_DIR"), env_path("HALO_GAME_BIN"), env_path("HALO_DATA_ROOT"))
    else {
        eprintln!(
            "HALO_STDB_BIN, HALO_MAP_DIR, HALO_GAME_BIN and HALO_DATA_ROOT are not all set: skipping, \
             this test needs the game's own data and a game built with the library"
        );
        return;
    };

    // a whole server: two servers in the list, the first on a rotation of two maps
    let dir = scratch("server-list");
    let stdb = Stdb::start(&stdb_bin);
    std::fs::write(dir.join("owner.token"), &stdb.owner().token).unwrap();
    let rotation_lounge = "[[server.rotation]]\nmap = \"bloodgulch\"\ncapacity = 100\nseconds = 70\n\
                           [[server.rotation]]\nmap = \"sidewinder\"\ngame_type = \"slayer\"\ncapacity = 100\nseconds = 70\n";
    let rotation_arena = "[[server.rotation]]\nmap = \"ratrace\"\ncapacity = 20\nseconds = 0\n";
    let mut config = halo_server::fixtures::test_config(
        &dir,
        &stdb.uri(),
        None,
        &[("lounge", rotation_lounge), ("arena", rotation_arena)],
    );
    config.servers[0].title = Some("Blood Gulch Lounge".into());
    config.servers[1].title = Some("Ratrace Arena".into());
    let running = halo_server::start(config, Arc::new(MapFiles { dir: maps }), Log::new()).expect("the server starts");

    // the game, with a console and no map of its own: the player picks the server
    let work = dir.join("game");
    std::fs::create_dir_all(&work).unwrap();
    let log_path = work.join("game.log");
    let log = std::fs::File::create(&log_path).unwrap();
    let telnet = free_tcp_port();
    let game_process = Game(
        Command::new(&game)
            .current_dir(game.parent().unwrap())
            .env("HALO_DATA_ROOT", &data)
            .env("HALO_SAVE_ROOT", work.join("saves"))
            .env("HALO_LARGE_ROOT", "halo-root")
            .env("HALO_LARGE_SPACETIMEDB", stdb.uri())
            .env("HALO_LARGE_LOG", "1")
            .env("HALO_LARGE_NAME", "Tester")
            .env("HALO_TELNET_CONSOLE", "1")
            .env("HALO_TELNET_CONSOLE_PORT", telnet.to_string())
            .env("HALO_NET_ONLINE", "0")
            .env("HALO_FULLSCREEN", "0")
            .env("HALO_NO_VSYNC", "1")
            .env("HALO_NO_AUDIO", "1")
            .env("HALO_HIDDEN_WINDOW", "1")
            .env("HALO_UPDATE_ANSWER", "no")
            .env("HALO_EXIT_AFTER", "260")
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .expect("start the game"),
    );
    wait_for_log(&log_path, "the game's main menu", 60, |l| l.contains("the server list of halo-root"));
    std::thread::sleep(Duration::from_secs(3));

    // the list, as the player reads it on the console
    let mut console = Console::connect(telnet);
    let listed = console.say("servers");
    println!("{listed}");
    let identity_line = listed.lines().find(|l| l.starts_with("you are ")).expect("the player's identity is shown");
    let me = identity_line.trim_start_matches("you are ").trim().to_string();
    assert_eq!(me.len(), 64, "a hex identity: {me}");
    let lounge = listed.lines().find(|l| l.contains("Blood Gulch Lounge")).expect("the server in the list");
    for wanted in ["bloodgulch", "slayer", "0/100"] {
        assert!(lounge.contains(wanted), "the list's line for the lounge lacks {wanted:?}: {lounge}");
    }
    let arena = listed.lines().find(|l| l.contains("Ratrace Arena")).expect("the other server in the list");
    assert!(arena.contains("ratrace") && arena.contains("0/20"), "{arena}");
    let number = lounge.trim_start().trim_start_matches('*').split_whitespace().next().unwrap().to_string();

    // joining one from the list: a seat, then the game on the server's map
    let told = console.say(&format!("join {number}"));
    assert!(told.contains("joined Blood Gulch Lounge as player"), "told: {told}");
    let log = wait_for_log(&log_path, "the game on bloodgulch", 90, |l| {
        l.contains("the local unit is where the server has the player")
    });
    assert!(log.contains("large mode: joining lounge (bloodgulch)"), "the game joined the lounge's first match");
    let watch = |database: &str| MatchClient::connect_as(&stdb.uri(), database, Some(&stdb.owner().token));
    let first = running.root().servers().into_iter().find(|s| s.id == "lounge").unwrap();
    let first_match = watch(&first.database);
    let seats = first_match.seats();
    assert_eq!(seats.len(), 1);
    let names: Vec<String> = first_match.roster().values().map(|r| r.name.clone()).collect();
    assert_eq!(names, ["Tester"], "the name the player chose is on the roster");
    assert_eq!(seats.values().next().unwrap().owner.to_hex().to_string(), me, "the seat is the player's identity");
    let listed = console.say("servers");
    assert!(
        listed.lines().any(|l| l.starts_with('*') && l.contains("Blood Gulch Lounge") && l.contains("1/100")),
        "{listed}"
    );

    // the rotation moves on: the game leaves its match and joins the next, on the next map
    wait_for_log(&log_path, "the lounge's second match", 150, |l| {
        l.contains("large mode: joining lounge (sidewinder)")
    });
    let log = wait_for_log(&log_path, "the game on sidewinder", 90, |l| {
        l.matches("the local unit is where the server has the player").count() >= 2
    });
    assert!(log.contains("moved from"), "the game noticed the match moved on");
    let second = running.root().servers().into_iter().find(|s| s.id == "lounge").unwrap();
    assert_ne!(first.database, second.database, "a fresh database");
    assert_eq!(second.map, "sidewinder");
    let seats = watch(&second.database).seats();
    assert_eq!(seats.values().next().unwrap().owner.to_hex().to_string(), me, "the same identity in the next match");

    // another server: the same identity
    let told = console.say("servers");
    let arena_number = told
        .lines()
        .find(|l| l.contains("Ratrace Arena"))
        .and_then(|l| l.trim_start().trim_start_matches('*').split_whitespace().next())
        .unwrap()
        .to_string();
    let told = console.say(&format!("join {arena_number}"));
    assert!(told.contains("leaving the game to join Ratrace Arena"), "told: {told}");
    // (the lounge's game ends, its scores are shown, and the lobby joins the arena)
    wait_for_log(&log_path, "the arena", 90, |l| l.contains("large mode: joining arena (ratrace)"));
    let arena_now = running.root().servers().into_iter().find(|s| s.id == "arena").unwrap();
    let arena_match = watch(&arena_now.database);
    let deadline = Instant::now() + Duration::from_secs(30);
    while arena_match.seats().is_empty() {
        assert!(Instant::now() < deadline, "no seat on the arena");
        std::thread::sleep(Duration::from_millis(100));
    }
    let seats = arena_match.seats();
    assert_eq!(seats.values().next().unwrap().owner.to_hex().to_string(), me, "the same identity on the other server");
    wait_for_log(&log_path, "the game on ratrace", 90, |l| {
        l.matches("the local unit is where the server has the player").count() >= 3
    });

    // the operator bans the identity: the game says why, and so does every server
    running.root().ban(Identity::from_hex(&me).unwrap(), "griefing").unwrap();
    wait_for_log(&log_path, "the ban", 30, |l| l.contains("you are banned from this server: griefing"));
    let told = console.say(&format!("join {number}"));
    assert!(told.contains("you are banned from this server: griefing"), "told: {told}");

    drop(game_process);
    running.stop().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
