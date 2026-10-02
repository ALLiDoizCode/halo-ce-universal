//! The headless C client in the large-scale mode, against a real local
//! SpacetimeDB and gateway on Blood Gulch, with simulated players walking
//! beside it. The game starts a session from its settings, joins, and logs
//! every player the gateway sends it once a second; this test compares that
//! log with what the server held. It covers the adapter: the C game, the
//! library linked into it, and the boundary between them.
//!
//! It needs the game's own data, so it skips itself (and says why) without
//! all of:
//!
//! - `HALO_STDB_BIN`: a SpacetimeDB 2.10.x release directory;
//! - `HALO_MAP_DIR`: the Xbox `.map` files (the module needs Blood Gulch's
//!   collision data);
//! - `HALO_GAME_BIN`: the game built with the library (`python configure.py
//!   --large-mode=on && ninja linux`, then `build/linux/halo`);
//! - `HALO_DATA_ROOT`: the folder that holds the game's `maps/`.
//!
//! ```text
//! HALO_STDB_BIN=~/.local/share/spacetimedb-2.10.2 HALO_MAP_DIR=<data root>/maps \
//!   HALO_GAME_BIN=<repository>/build/linux/halo HALO_DATA_ROOT=<data root> \
//!   cargo test --release --test headless -- --nocapture
//! ```
//!
//! The game runs in a hidden window (`HALO_HIDDEN_WINDOW`, which needs a
//! display to make an OpenGL context on), and stops itself after
//! `HALO_EXIT_AFTER` seconds, as the engine's other scripted runs do.
//! (`HALO_NULL_RENDERER` would need no display, but then there is no window
//! and `HALO_EXIT_AFTER` never fires.)

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use halo_gateway::harness::{Crowd, Impairment, Truth};
use halo_gateway::{Gateway, GatewayConfig, UdpTransport};
use halo_match_driver::server::{build_module, stdb_bin_dir, Server};
use halo_match_driver::walkers::Walkers;
use halo_match_driver::MatchClient;
use halo_sim::MapData;
use halo_wire::unit::Bounds;

/// Players walking beside the game's, and the game's.
const OTHERS: u16 = 120;
const ME: u16 = OTHERS;
/// How long the game runs, from the moment its window opens, seconds. Booting
/// and loading Blood Gulch take a few seconds of it.
const GAME_SECONDS: u32 = 40;

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

/// What one log line of the game says about one player.
#[derive(Debug, Clone, PartialEq)]
struct Logged {
    player: u16,
    tick: u32,
    position: [f32; 3],
}

/// `large mode: player 7 tick 812 (1.2345 -2.5000 3.0000) v (...) yaw .. pitch ..`
fn parse_player_line(line: &str) -> Option<Logged> {
    let rest = line.split("large mode: player ").nth(1)?;
    let mut words = rest.split_whitespace();
    let player = words.next()?.parse().ok()?;
    words.next().filter(|w| *w == "tick")?;
    let tick = words.next()?.parse().ok()?;
    let open = rest.find('(')?;
    let close = rest.find(')')?;
    let mut numbers = rest[open + 1..close].split_whitespace().map(|n| n.parse::<f32>());
    let position = [numbers.next()?.ok()?, numbers.next()?.ok()?, numbers.next()?.ok()?];
    Some(Logged { player, tick, position })
}

#[test]
fn the_logged_players_are_the_ones_the_server_sent() {
    let (Some(stdb), Some(maps), Some(game), Some(data)) =
        (stdb_bin_dir(), env_path("HALO_MAP_DIR"), env_path("HALO_GAME_BIN"), env_path("HALO_DATA_ROOT"))
    else {
        eprintln!(
            "HALO_STDB_BIN, HALO_MAP_DIR, HALO_GAME_BIN and HALO_DATA_ROOT are not all set: skipping, \
             this test needs the game's own data and a game built with the library"
        );
        return;
    };

    let server = Server::start(&stdb);
    server.publish(&build_module(), "headless");
    let client = MatchClient::connect(&server.uri(), "headless");
    let halo_map = halo_map::HaloMap::from_path(maps.join("bloodgulch.map")).expect("Blood Gulch");
    let anchors: Vec<[f32; 3]> = halo_map.player_starts.iter().map(|s| s.position).collect();
    let map = MapData::from(halo_map);
    let bounds = Bounds::from_world(map.world_bounds);
    client.load_map(map.to_bytes()).unwrap();
    // the game's player is the last: it stands where it spawns, nobody sends for it
    let (mut walkers, spawn) = Walkers::new(map, &anchors, OTHERS + 1, 7);
    client.add_players(&spawn).unwrap();
    client.start();
    let transport = Arc::new(UdpTransport::bind("127.0.0.1:0".parse().unwrap()).unwrap());
    let gateway = Gateway::start(GatewayConfig::new(server.uri(), "headless"), transport).expect("the gateway");
    let crowd = Crowd::connect(gateway.local_addr(), 0..OTHERS, Impairment::none(), OTHERS as usize + 1, false);
    crowd.join_all(Duration::from_secs(20)).unwrap();

    // the game, started from its settings alone
    let work = std::env::temp_dir().join(format!("halo-headless-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    let log_path = work.join("game.log");
    let log = std::fs::File::create(&log_path).unwrap();
    let mut game_process = Game(
        Command::new(&game)
            .current_dir(game.parent().unwrap())
            .env("HALO_DATA_ROOT", &data)
            .env("HALO_SAVE_ROOT", work.join("saves"))
            .env("HALO_LARGE_MAP", "bloodgulch")
            .env("HALO_LARGE_GATEWAY", gateway.local_addr().to_string())
            .env("HALO_LARGE_SPACETIMEDB", server.uri())
            .env("HALO_LARGE_DATABASE", "headless")
            .env("HALO_LARGE_PLAYER", ME.to_string())
            .env("HALO_LARGE_LOG", "1")
            .env("HALO_NET_ONLINE", "0")
            .env("HALO_FULLSCREEN", "0")
            .env("HALO_NO_VSYNC", "1")
            .env("HALO_NO_AUDIO", "1")
            .env("HALO_HIDDEN_WINDOW", "1")
            .env("HALO_UPDATE_ANSWER", "no")
            .env("HALO_EXIT_AFTER", GAME_SECONDS.to_string())
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .expect("start the game"),
    );

    // walk the others for as long as the game runs, recording what the server held
    let mut truth = Truth::new(OTHERS as usize + 1);
    let until = Instant::now() + Duration::from_secs(GAME_SECONDS as u64 + 40);
    let mut exit = None;
    while exit.is_none() && Instant::now() < until {
        let Some(seen) = client.next_tick(Duration::from_secs(10)) else { panic!("no tick for 10 s") };
        truth.record(&seen);
        walkers.sync_with_server(seen.players.values());
        crowd.send_inputs(&walkers.next_inputs());
        exit = game_process.0.try_wait().unwrap();
    }
    let output = std::fs::read_to_string(&log_path).unwrap_or_default();
    let tail = || output.lines().rev().take(15).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
    assert!(exit.is_some_and(|s| s.success()), "the game did not exit by itself; the end of its log:\n{}", tail());

    // the session: started, joined, and the local unit placed where the server has the player
    assert!(output.contains("large mode: bloodgulch as player 120"), "the settings were not read:\n{}", tail());
    assert!(output.contains("the local unit is where the server has the player"), "never placed:\n{}", tail());
    let statuses: Vec<&str> = output.lines().filter(|l| l.contains("large mode: tick ")).collect();
    assert!(statuses.len() >= 5, "only {} status lines:\n{}", statuses.len(), tail());
    let last = statuses.last().unwrap();
    assert!(last.contains("joined 1 slow 1 map 1"), "the last status line was {last:?}");
    assert!(!last.contains("inputs 0 "), "the game never sent an input: {last:?}");

    // the players it logged, against the truth
    let logged: Vec<Logged> = output.lines().filter_map(parse_player_line).collect();
    let mut compared = 0;
    let mut heard_of = std::collections::BTreeSet::new();
    for l in &logged {
        assert_ne!(l.player, ME, "the game was sent its own player");
        let Some(at) = truth.ticks.get(&l.tick) else { continue };
        let want = at.positions[l.player as usize].expect("a state of a player the server has");
        for (axis, held) in want.iter().enumerate() {
            // the packing's resolution, and the log's four decimals
            let step = (bounds.max[axis] - bounds.min[axis]) / 65535.0 + 0.0001;
            assert!(
                (l.position[axis] - held).abs() <= step,
                "tick {}, player {}, axis {axis}: logged {} but the server held {}",
                l.tick,
                l.player,
                l.position[axis],
                held
            );
        }
        heard_of.insert(l.player);
        compared += 1;
    }
    println!(
        "{} players logged over {} status lines, {compared} compared with the server, {} distinct players",
        logged.len(),
        statuses.len(),
        heard_of.len()
    );
    assert!(compared > 300, "only {compared} logged players could be compared; the end of the log:\n{}", tail());
    assert!(heard_of.len() > 20, "only {} different players were logged", heard_of.len());

    let rejected = client.players()[&ME].rejected_moves;
    println!("the game's player had {rejected} moves rejected");
    let _ = std::fs::remove_dir_all(&work);
}
