//! The headless C client in the large-scale mode, against a real local
//! SpacetimeDB and gateway on Blood Gulch, with simulated players walking
//! beside it. The game starts a session from its settings, joins, and once a
//! second logs every player the gateway sends it and where the engine has
//! drawn each remote player's unit; this test compares both logs with what the
//! server held, and has one player leave the match and join it again to see
//! the unit go and come back. It covers the adapter: the C game, the library
//! linked into it, and the boundary between them.
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

use halo_gateway::harness::sim_public_key;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use halo_gateway::harness::{Crowd, Impairment, Rig, RigSetup, Truth};
use halo_match_driver::server::{build_module, stdb_bin_dir};
use halo_sim::MapData;

/// Players seated and walking beside the game's, which takes the next seat
/// (`HALO_HEADLESS_PLAYERS` says another number, such as 500 for the adapter's
/// cost with a full match in view).
const DEFAULT_OTHERS: u16 = 120;
/// How many of them the engine can give a player of its own (its player
/// records hold 128, the game's own included).
const ENGINE_PLAYERS: u16 = 127;
/// How long the game runs, from the moment its window opens, seconds. Booting
/// and loading Blood Gulch take a few seconds of it.
const GAME_SECONDS: u32 = 50;
/// The player who leaves the match and joins it again, to go out of range and
/// come back: it leaves a few seconds after the game has drawn them, and
/// joins again a few seconds after the game has deleted their unit.
const LEAVER: usize = 7;
const LEAVER_WAIT: Duration = Duration::from_secs(4);

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

/// How far from the position of a player's state the game may draw them (the log has the drawn
/// position, which is where the state would have taken the player by now): the state's age is at most
/// the extrapolation's limit of 15 ticks, at the fastest legal speed of 4 units a second, and a little over.
const EXTRAPOLATED: f32 = 2.2;

/// What one log line of the game says about one player.
#[derive(Debug, Clone, PartialEq)]
struct Logged {
    player: u16,
    tick: u32,
    position: [f32; 3],
}

/// What one `drawn` line says about one remote player: the tick of the state
/// it was driven from, and where the engine had the unit.
#[derive(Debug, Clone, PartialEq)]
struct Drawn {
    player: u16,
    tick: u32,
    position: [f32; 3],
    team: u8,
    /// The engine's player of the unit, if it has one.
    engine_player: i32,
}

/// `large mode: drawn 7 tick 812 (1.2345 -2.5000 3.0000) team 1 player 3`
fn parse_drawn_line(line: &str) -> Option<Drawn> {
    let rest = line.split("large mode: drawn ").nth(1)?;
    let mut words = rest.split_whitespace();
    let player = words.next()?.parse().ok()?;
    words.next().filter(|w| *w == "tick")?;
    let tick = words.next()?.parse().ok()?;
    let open = rest.find('(')?;
    let close = rest.find(')')?;
    let mut numbers = rest[open + 1..close].split_whitespace().map(|n| n.parse::<f32>());
    let position = [numbers.next()?.ok()?, numbers.next()?.ok()?, numbers.next()?.ok()?];
    let mut after = rest[close + 1..].split_whitespace();
    after.next().filter(|w| *w == "team")?;
    let team = after.next()?.parse().ok()?;
    after.next().filter(|w| *w == "player")?;
    let engine_player = after.next()?.parse().ok()?;
    Some(Drawn { player, tick, position, team, engine_player })
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

    let others: u16 =
        std::env::var("HALO_HEADLESS_PLAYERS").ok().and_then(|n| n.parse().ok()).unwrap_or(DEFAULT_OTHERS);
    let me = others;
    let halo_map = halo_map::HaloMap::from_path(maps.join("bloodgulch.map")).expect("Blood Gulch");
    let anchors: Vec<[f32; 3]> = halo_map.player_starts.iter().map(|s| s.position).collect();
    let map = MapData::from(halo_map);
    let wasm = build_module();
    let mut rig = Rig::start(
        &stdb,
        &wasm,
        RigSetup { name: "headless", map, anchors: &anchors, players: others, budget: 90_000 },
        |_| {},
    );
    // room for the game's own player too
    rig.client.set_capacity(others + 1).unwrap();
    let crowd = Crowd::connect(rig.gateway.local_addr(), 0..others, Impairment::none(), others as usize + 1, false);
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
            .env("HALO_LARGE_GATEWAY", rig.gateway.local_addr().to_string())
            .env("HALO_LARGE_SPACETIMEDB", rig.server.uri())
            .env("HALO_LARGE_DATABASE", "headless")
            .env("HALO_LARGE_LOG", "1")
            .env("HALO_NET_ONLINE", "0")
            .env("HALO_FULLSCREEN", "0")
            .env("HALO_NO_VSYNC", "1")
            .env("HALO_NO_AUDIO", "1")
            .env("HALO_HIDDEN_WINDOW", "1")
            .env("HALO_UPDATE_ANSWER", "no")
            .env("HALO_EXIT_AFTER", GAME_SECONDS.to_string())
            // (to see what is drawn: HALO_SCREENSHOT_DIR and _EVERY, passed on if set)
            .envs(
                ["HALO_SCREENSHOT_DIR", "HALO_SCREENSHOT_EVERY"]
                    .iter()
                    .filter_map(|n| Some((*n, std::env::var(n).ok()?))),
            )
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .expect("start the game"),
    );

    // walk the others for as long as the game runs, recording what the server held
    let mut truth = Truth::new(others as usize + 1);
    let until = Instant::now() + Duration::from_secs(GAME_SECONDS as u64 + 40);
    let mut exit = None;
    // (what the leaver has done: when the game was seen to draw them, when they left, and when
    // the game was seen to delete their unit)
    let (mut seen_drawn, mut left, mut gone, mut returned) = (None::<Instant>, false, None::<Instant>, false);
    let mut polled = Instant::now();
    while exit.is_none() && Instant::now() < until {
        let Some(seen) = rig.client.next_tick(Duration::from_secs(10)) else { panic!("no tick for 10 s") };
        // one player goes out of the match for a while: out of range (the game's
        // log is read once a second, to see how far it has got)
        if !returned && polled.elapsed() > Duration::from_secs(1) {
            polled = Instant::now();
            let log = std::fs::read_to_string(&log_path).unwrap_or_default();
            if seen_drawn.is_none() && log.contains(&format!("large mode: player {LEAVER} appears")) {
                seen_drawn = Some(Instant::now());
            }
            if !left && seen_drawn.is_some_and(|at| at.elapsed() > LEAVER_WAIT) {
                left = true;
                rig.seats.clients[LEAVER].leave().unwrap();
            }
            if left && gone.is_none() && log.contains(&format!("large mode: player {LEAVER} is out of range")) {
                gone = Some(Instant::now());
            }
            if gone.is_some_and(|at| at.elapsed() > LEAVER_WAIT) {
                returned = true;
                rig.seats.clients[LEAVER].join(sim_public_key(LEAVER as u16)).unwrap();
            }
        }
        truth.record(&seen);
        rig.walkers.sync_with_server(seen.players.values());
        crowd.send_inputs(&rig.walkers.next_inputs());
        exit = game_process.0.try_wait().unwrap();
    }
    let output = std::fs::read_to_string(&log_path).unwrap_or_default();
    // (to read the whole of it afterwards)
    if let Some(keep) = env_path("HALO_HEADLESS_LOG") {
        let _ = std::fs::write(keep, &output);
    }
    let tail = || output.lines().rev().take(15).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
    assert!(exit.is_some_and(|s| s.success()), "the game did not exit by itself; the end of its log:\n{}", tail());

    // the session: started, joined, and the local unit placed where the server has the player
    assert!(output.contains("large mode: bloodgulch, database headless"), "the settings were not read:\n{}", tail());
    assert!(output.contains("the local unit is where the server has the player"), "never placed:\n{}", tail());
    let statuses: Vec<&str> = output.lines().filter(|l| l.contains("large mode: tick ")).collect();
    assert!(statuses.len() >= 5, "only {} status lines:\n{}", statuses.len(), tail());
    let last = statuses.last().unwrap();
    assert!(last.contains(&format!("joined 1 slow 1 map 1 player {me} ")), "the last status line was {last:?}");
    assert!(!last.contains("inputs 0 "), "the game never sent an input: {last:?}");

    // the players it logged, against the truth
    let logged: Vec<Logged> = output.lines().filter_map(parse_player_line).collect();
    let mut compared = 0;
    let mut heard_of = std::collections::BTreeSet::new();
    for l in &logged {
        assert_ne!(l.player, me, "the game was sent its own player");
        let Some(at) = truth.ticks.get(&l.tick) else { continue };
        let want = at.positions[l.player as usize].expect("a state of a player the server has");
        for (axis, held) in want.iter().enumerate() {
            // the packing's resolution, and the log's four decimals
            let step = EXTRAPOLATED;
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

    // the remote players the game drew: a unit for each of the others, where the
    // server has the player (the engine's own position of the unit, which the
    // adapter set from the library's state of that tick), in the colour of the
    // team the roster gives
    let roster = rig.client.roster();
    let drawn: Vec<Drawn> = output.lines().filter_map(parse_drawn_line).collect();
    let mut drawn_compared = 0;
    let mut drawn_players = std::collections::BTreeSet::new();
    for d in &drawn {
        assert_ne!(d.player, me, "the game drew its own player as a remote one");
        assert_eq!(d.team, roster[&d.player].team, "player {} was drawn in a team the roster does not give", d.player);
        let Some(at) = truth.ticks.get(&d.tick) else { continue };
        let want = at.positions[d.player as usize].expect("a state of a player the server has");
        for (axis, held) in want.iter().enumerate() {
            let step = EXTRAPOLATED;
            assert!(
                (d.position[axis] - held).abs() <= step,
                "tick {}, player {}, axis {axis}: drawn at {} but the server held {}",
                d.tick,
                d.player,
                d.position[axis],
                held
            );
        }
        drawn_players.insert(d.player);
        drawn_compared += 1;
    }
    // and exactly where the library held the player, for the tick the unit was driven from (both
    // are logged to four decimals)
    let held: std::collections::HashMap<(u16, u32), [f32; 3]> =
        logged.iter().map(|l| ((l.player, l.tick), l.position)).collect();
    let mut matched = 0;
    for d in &drawn {
        if let Some(state) = held.get(&(d.player, d.tick)) {
            for (axis, held) in state.iter().enumerate() {
                assert!(
                    (d.position[axis] - held).abs() <= EXTRAPOLATED,
                    "tick {}, player {}, axis {axis}: drawn at {} but the library held {}",
                    d.tick,
                    d.player,
                    d.position[axis],
                    held
                );
            }
            matched += 1;
        }
    }
    assert!(matched > 300, "only {matched} drawn positions had a logged state of the library to match");
    println!(
        "{} drawn lines, {drawn_compared} compared with the server, {} distinct players",
        drawn.len(),
        drawn_players.len()
    );
    assert!(drawn_compared > 300, "only {drawn_compared} drawn positions could be compared:\n{}", tail());
    assert!(drawn_players.len() >= others as usize - 2, "only {} players drawn", drawn_players.len());
    assert!(drawn.iter().any(|d| d.team == 0) && drawn.iter().any(|d| d.team == 1), "both teams are drawn");
    // the engine has players of its own for 127 of them, and the rest are units alone
    let with_players = others.min(ENGINE_PLAYERS);
    if others <= ENGINE_PLAYERS {
        assert!(drawn.iter().all(|d| d.engine_player >= 0), "every one of the {others} has an engine player");
    }

    // the summary line says how many units, and how many have engine players
    let summaries: Vec<&str> = output.lines().filter(|l| l.contains(" remote units, ")).collect();
    let last = summaries.last().expect("a summary of the remote units");
    assert!(
        last.contains(&format!("{others} remote units, {with_players} with players")),
        "the last summary was {last:?}"
    );
    // what the adapter cost a tick, for the record
    // ("the adapter cost 1.873 ms a tick over 30 ticks, 2.430 ms at worst", every second)
    let costs: Vec<(f32, f32)> = output
        .lines()
        .filter_map(|l| {
            let rest = l.split("the adapter cost ").nth(1)?;
            let mut words = rest.split_whitespace();
            let mean = words.next()?.parse().ok()?;
            let worst = rest.split(" ticks, ").nth(1)?.split_whitespace().next()?.parse().ok()?;
            Some((mean, worst))
        })
        .collect();
    let seconds = costs.len().max(1) as f32;
    let mean = costs.iter().map(|c| c.0).sum::<f32>() / seconds;
    // the ticket's budget, with a full match in view
    if others >= 500 {
        assert!(mean < 3.0, "the adapter cost {mean} ms a tick with {others} players");
    }
    println!(
        "the adapter cost, over {} seconds with {others} remote players: {:.3} ms a tick on average (the seconds' \
         means from {:.3} to {:.3}), {:.3} ms at worst",
        costs.len(),
        costs.iter().map(|c| c.0).sum::<f32>() / seconds,
        costs.iter().map(|c| c.0).fold(f32::MAX, f32::min),
        costs.iter().map(|c| c.0).fold(0.0, f32::max),
        costs.iter().map(|c| c.1).fold(0.0, f32::max)
    );

    // the one who left was out of range, then came back
    let id = LEAVER;
    let appeared = output.matches(&format!("large mode: player {id} appears: ")).count();
    let gone = output.matches(&format!("large mode: player {id} is out of range: unit deleted")).count();
    assert_eq!((appeared, gone), (2, 1), "player {id} should appear, go out of range and appear again");
    let first_appears = output.find(&format!("large mode: player {id} appears")).unwrap();
    let goes = output.find(&format!("large mode: player {id} is out of range")).unwrap();
    let returns = output.rfind(&format!("large mode: player {id} appears")).unwrap();
    assert!(first_appears < goes && goes < returns);
    let after_return = output[returns..].lines().filter_map(parse_drawn_line).any(|d| d.player as usize == id);
    assert!(after_return, "player {id} was not drawn after coming back");

    let rejected = rig.client.players()[&me].rejected_moves;
    println!("the game's player had {rejected} moves rejected");
    let _ = std::fs::remove_dir_all(&work);
}

/// What one `local unit` line says: where the engine has the player's own
/// unit, and where the library has the player.
#[derive(Debug, Clone, Copy, PartialEq)]
struct LocalLine {
    engine: [f32; 3],
    library: [f32; 3],
    airborne: bool,
}

/// `large mode: local unit (1.0 2.0 3.0) library (1.0 2.0 3.0) v (0.1 0.2 0.3) airborne 0`
fn parse_local_line(line: &str) -> Option<LocalLine> {
    let rest = line.split("large mode: local unit ").nth(1)?;
    let triple = |s: &str| -> Option<[f32; 3]> {
        let open = s.find('(')?;
        let close = s.find(')')?;
        let mut n = s[open + 1..close].split_whitespace().map(|n| n.parse::<f32>());
        Some([n.next()?.ok()?, n.next()?.ok()?, n.next()?.ok()?])
    };
    let engine = triple(rest)?;
    let library = triple(rest.split("library ").nth(1)?)?;
    let airborne = rest.rsplit("airborne ").next()?.trim() == "1";
    Some(LocalLine { engine, library, airborne })
}

/// The game's own player walking on a scripted pattern (`HALO_TEST_INPUT=hop:`:
/// walking, strafing and turning, jumping every few seconds and crouching for
/// stretches, on the red base's platform, whose edge it walks off and falls
/// from now and then), moved by the library from its controls, for
/// `HALO_HEADLESS_WALK_SECONDS` (a minute by default; 600 is the ten-minute
/// run) against a real server. The unit is where the library says (the log
/// lines of both agree), it has gone somewhere, it has been in the air, and
/// the server rejected none of its moves, from the first second to the last.
#[test]
fn the_local_player_is_moved_by_the_library_and_the_server_accepts_every_move() {
    let (Some(stdb), Some(maps), Some(game), Some(data)) =
        (stdb_bin_dir(), env_path("HALO_MAP_DIR"), env_path("HALO_GAME_BIN"), env_path("HALO_DATA_ROOT"))
    else {
        eprintln!(
            "HALO_STDB_BIN, HALO_MAP_DIR, HALO_GAME_BIN and HALO_DATA_ROOT are not all set: skipping, \
             this test needs the game's own data and a game built with the library"
        );
        return;
    };
    let seconds: u32 = std::env::var("HALO_HEADLESS_WALK_SECONDS").ok().and_then(|n| n.parse().ok()).unwrap_or(60);
    let others: u16 = 20;
    let me = others;
    let halo_map = halo_map::HaloMap::from_path(maps.join("bloodgulch.map")).expect("Blood Gulch");
    let map = MapData::from(halo_map);
    // everyone starts on the red base's platform, whose west edge is a few
    // world units away: the game's player jumps and, walking about, falls off it
    let anchors = [[96.5, -157.9, 1.7]];
    let wasm = build_module();
    let mut rig = Rig::start(
        &stdb,
        &wasm,
        RigSetup { name: "headless-walk", map, anchors: &anchors, players: others, budget: 90_000 },
        |_| {},
    );
    rig.client.set_capacity(others + 1).unwrap();
    let crowd = Crowd::connect(rig.gateway.local_addr(), 0..others, Impairment::none(), others as usize + 1, false);
    crowd.join_all(Duration::from_secs(20)).unwrap();

    let work = std::env::temp_dir().join(format!("halo-headless-walk-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    let log_path = work.join("game.log");
    let log = std::fs::File::create(&log_path).unwrap();
    let mut game_process = Game(
        Command::new(&game)
            .current_dir(game.parent().unwrap())
            .env("HALO_DATA_ROOT", &data)
            .env("HALO_SAVE_ROOT", work.join("saves"))
            .env("HALO_LARGE_MAP", "bloodgulch")
            .env("HALO_LARGE_GATEWAY", rig.gateway.local_addr().to_string())
            .env("HALO_LARGE_SPACETIMEDB", rig.server.uri())
            .env("HALO_LARGE_DATABASE", "headless-walk")
            .env("HALO_TEST_INPUT", "hop:3")
            .env("HALO_NET_ONLINE", "0")
            .env("HALO_FULLSCREEN", "0")
            .env("HALO_NO_VSYNC", "1")
            .env("HALO_NO_AUDIO", "1")
            .env("HALO_HIDDEN_WINDOW", "1")
            .env("HALO_UPDATE_ANSWER", "no")
            .env("HALO_EXIT_AFTER", seconds.to_string())
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .expect("start the game"),
    );

    let until = Instant::now() + Duration::from_secs(seconds as u64 + 40);
    let mut exit = None;
    let mut rejected_seen = 0;
    let mut before = [0.0f32; 3];
    // (the reason and server tick of each rejection, and the first tick seen)
    let mut rejections: Vec<(u8, u64)> = Vec::new();
    while exit.is_none() && Instant::now() < until {
        let Some(seen) = rig.client.next_tick(Duration::from_secs(10)) else { panic!("no tick for 10 s") };
        rig.walkers.sync_with_server(seen.players.values());
        crowd.send_inputs(&rig.walkers.next_inputs());
        if let Some(row) = seen.players.get(&me) {
            if row.rejected_moves > rejected_seen {
                rejected_seen = row.rejected_moves;
                rejections.push((row.last_reject, row.last_reject_tick));
                println!(
                    "the game's player was rejected ({rejected_seen}): reason {} at tick {}, held at ({:.3} {:.3} {:.3}) \
                     flags {} air {} updated at tick {}, the move before it from ({:.3} {:.3} {:.3})",
                    row.last_reject, row.last_reject_tick, row.x, row.y, row.z, row.flags, row.air_ticks,
                    row.updated_tick, before[0], before[1], before[2]
                );
            }
            before = [row.x, row.y, row.z];
        }
        exit = game_process.0.try_wait().unwrap();
    }
    let output = std::fs::read_to_string(&log_path).unwrap_or_default();
    if let Some(keep) = env_path("HALO_HEADLESS_LOG") {
        let _ = std::fs::write(keep, &output);
    }
    let tail = || output.lines().rev().take(15).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
    assert!(exit.is_some_and(|s| s.success()), "the game did not exit by itself; the end of its log:\n{}", tail());
    assert!(output.contains("the local unit is where the server has the player"), "never placed:\n{}", tail());
    assert!(
        output.contains("the library moves the local unit from now on"),
        "the library never moved the unit:\n{}",
        tail()
    );

    // the unit is where the library says, every second of the run
    let lines: Vec<LocalLine> = output.lines().filter_map(parse_local_line).collect();
    assert!(lines.len() as u32 >= seconds / 2, "only {} local unit lines in {seconds} s:\n{}", lines.len(), tail());
    for (i, l) in lines.iter().enumerate() {
        for axis in 0..3 {
            assert!(
                (l.engine[axis] - l.library[axis]).abs() <= 0.00011,
                "second {i}, axis {axis}: the engine has the unit at {} but the library has the player at {}",
                l.engine[axis],
                l.library[axis]
            );
        }
    }
    // and it went somewhere
    let travelled: f32 =
        lines.windows(2).map(|w| (0..3).map(|a| (w[1].library[a] - w[0].library[a]).powi(2)).sum::<f32>().sqrt()).sum();
    let spread = (0..3)
        .map(|a| {
            let (lo, hi) =
                lines.iter().fold((f32::MAX, f32::MIN), |(lo, hi), l| (lo.min(l.library[a]), hi.max(l.library[a])));
            hi - lo
        })
        .fold(0.0, f32::max);
    let airborne = lines.iter().filter(|l| l.airborne).count();
    println!(
        "{} local unit lines over {seconds} s: {travelled:.1} world units between them, {spread:.1} across, \
         {airborne} in the air",
        lines.len()
    );
    assert!(spread > 1.0, "the player stayed within {spread} world units");

    let row = rig.client.players()[&me].clone();
    println!(
        "the game's player had {} moves rejected over {seconds} s (last reason {})",
        row.rejected_moves, row.last_reject
    );
    // Every move counts, from the first second: the game paces its movement by the
    // clock (it runs the ticks of its loading in a rush, and after a hitch), the
    // server accepts the moves of jumps and falls, and takes the newest of the
    // inputs the gateway has in a tick.
    println!("{} rejections in all: {rejections:?}", rejections.len());
    assert!(airborne > 0, "the player never left the ground");
    assert!(rejections.is_empty(), "the server rejected the game's player's moves (reason, tick): {rejections:?}");
    let _ = std::fs::remove_dir_all(&work);
}

/// What one `drawn` line says of the engine's unit for a remote player: the
/// flags the library holds of the player (1 in the air, 2 crouched), and what
/// the engine's animation made of them.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Animated {
    flags: i32,
    /// `unit.animation.state`: 20 airborne, 21 and 22 the soft and the hard landing
    state: i32,
    /// `unit.animation.base_seat_index`: 2 standing, 3 crouching
    seat: i32,
    /// `biped.landing`: -1 none, 0 soft, 1 hard
    landing: i32,
}

const STATE_AIRBORNE: i32 = 20;
const STATE_LAND_SOFT: i32 = 21;
const STATE_LAND_HARD: i32 = 22;
const SEAT_CROUCH: i32 = 3;

/// `... team 1 player 3 flags 2 state 4 seat 3 landing -1`
fn parse_animated_line(line: &str) -> Option<Animated> {
    let rest = line.split("large mode: drawn ").nth(1)?;
    let number =
        |key: &str| -> Option<i32> { rest.split(&format!(" {key} ")).nth(1)?.split_whitespace().next()?.parse().ok() };
    Some(Animated {
        flags: number("flags")?,
        state: number("state")?,
        seat: number("seat")?,
        landing: number("landing")?,
    })
}

/// Remote players who jump, crouch and walk off ledges, in the game's view
/// (`HALO_SCREENSHOT_DIR` and `HALO_SCREENSHOT_EVERY` take pictures of it): the
/// server accepts every one of their moves; the library gives the game their
/// flags; and the engine's animation plays what the flags say, with its
/// physics suspended for them (the airborne animation in the air, the
/// crouched seat while crouched, and the landing's after a fall).
#[test]
fn remote_players_that_jump_fall_and_crouch_are_animated_by_the_engine() {
    let (Some(stdb), Some(maps), Some(game), Some(data)) =
        (stdb_bin_dir(), env_path("HALO_MAP_DIR"), env_path("HALO_GAME_BIN"), env_path("HALO_DATA_ROOT"))
    else {
        eprintln!(
            "HALO_STDB_BIN, HALO_MAP_DIR, HALO_GAME_BIN and HALO_DATA_ROOT are not all set: skipping, \
             this test needs the game's own data and a game built with the library"
        );
        return;
    };
    let seconds: u32 = std::env::var("HALO_HEADLESS_ACROBAT_SECONDS").ok().and_then(|n| n.parse().ok()).unwrap_or(40);
    // (HALO_HEADLESS_ACROBAT_SHOW: a few acrobats in front of the game's player on the red base's
    // platform, for the screenshots: four jump and crouch where they stand, two walk to its edge)
    let show = std::env::var_os("HALO_HEADLESS_ACROBAT_SHOW").is_some();
    let halo_map = halo_map::HaloMap::from_path(maps.join("bloodgulch.map")).expect("Blood Gulch");
    let map = MapData::from(halo_map);
    // the red base's platform, which has an edge about two world units above the field
    let (others, anchors): (u16, Vec<[f32; 3]>) = if show {
        (
            6,
            // (the game's own player first, who looks about, then the acrobats ahead of them)
            vec![
                [93.5, -157.8, 1.7],
                [96.0, -156.6, 1.7],
                [96.5, -158.6, 1.7],
                [98.0, -157.2, 1.7],
                [97.0, -155.8, 1.7],
                [98.2, -159.0, 1.7],
            ],
        )
    } else {
        (16, vec![[98.4934, -157.639, 1.7]])
    };
    let wasm = build_module();
    let mut rig = Rig::start(
        &stdb,
        &wasm,
        RigSetup { name: "headless-acrobats", map, anchors: &anchors, players: others, budget: 90_000 },
        |_| {},
    );
    rig.walkers.set_acrobatics(true);
    if show {
        // (the first stands where the game's player does: it walks off behind them; three stay
        // where they are; the edge is to the +y side of the platform, which two walk to)
        rig.walkers.set_course(0, 1.0, core::f32::consts::PI);
        for id in 1..4 {
            rig.walkers.set_course(id, 0.0, 0.0);
        }
        rig.walkers.set_course(4, 0.5, core::f32::consts::FRAC_PI_2);
        rig.walkers.set_course(5, 0.5, core::f32::consts::FRAC_PI_2);
    }
    rig.client.set_capacity(others + 1).unwrap();
    let crowd = Crowd::connect(rig.gateway.local_addr(), 0..others, Impairment::none(), others as usize + 1, false);
    crowd.join_all(Duration::from_secs(20)).unwrap();

    let work = std::env::temp_dir().join(format!("halo-headless-acrobats-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    let log_path = work.join("game.log");
    let log = std::fs::File::create(&log_path).unwrap();
    let mut game_process = Game(
        Command::new(&game)
            .current_dir(game.parent().unwrap())
            .env("HALO_DATA_ROOT", &data)
            .env("HALO_SAVE_ROOT", work.join("saves"))
            .env("HALO_LARGE_MAP", "bloodgulch")
            .env("HALO_LARGE_GATEWAY", rig.gateway.local_addr().to_string())
            .env("HALO_LARGE_SPACETIMEDB", rig.server.uri())
            .env("HALO_LARGE_DATABASE", "headless-acrobats")
            .env("HALO_TEST_INPUT", if show { "scan:3" } else { "" })
            .env("HALO_LARGE_LOG", "1")
            .env("HALO_NET_ONLINE", "0")
            .env("HALO_FULLSCREEN", "0")
            .env("HALO_NO_VSYNC", "1")
            .env("HALO_NO_AUDIO", "1")
            .env("HALO_HIDDEN_WINDOW", "1")
            .env("HALO_UPDATE_ANSWER", "no")
            .env("HALO_EXIT_AFTER", seconds.to_string())
            .envs(
                ["HALO_SCREENSHOT_DIR", "HALO_SCREENSHOT_EVERY"]
                    .iter()
                    .filter_map(|n| Some((*n, std::env::var(n).ok()?))),
            )
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .expect("start the game"),
    );

    let until = Instant::now() + Duration::from_secs(seconds as u64 + 40);
    let mut exit = None;
    let mut airborne_seen = 0u32;
    let mut last_tick = 0u64;
    while exit.is_none() && Instant::now() < until {
        let Some(mut seen) = rig.client.next_tick(Duration::from_secs(10)) else { panic!("no tick for 10 s") };
        // (a machine busy with the game queues ticks up: the walkers move for the newest, once)
        while let Some(newer) = rig.client.next_tick(Duration::ZERO) {
            seen = newer;
        }
        // (and for each tick it was busy, the walkers have walked it, as a game does after a hitch: the
        // server judges the move against the time since the last)
        let elapsed = if last_tick == 0 { 1 } else { seen.marker.tick.saturating_sub(last_tick).clamp(1, 20) as u32 };
        last_tick = seen.marker.tick;
        rig.walkers.sync_with_server(seen.players.values());
        crowd.send_inputs(&rig.walkers.next_inputs_after(elapsed));
        airborne_seen += seen.players.values().filter(|p| p.flags & halo_sim::FLAG_AIRBORNE != 0).count() as u32;
        exit = game_process.0.try_wait().unwrap();
    }
    let output = std::fs::read_to_string(&log_path).unwrap_or_default();
    if let Some(keep) = env_path("HALO_HEADLESS_LOG") {
        let _ = std::fs::write(keep, &output);
    }
    let tail = || output.lines().rev().take(15).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
    assert!(exit.is_some_and(|s| s.success()), "the game did not exit by itself; the end of its log:\n{}", tail());

    // the server accepted all of what the acrobats did, jumps and falls included
    let rows = rig.client.players();
    let rejected: u64 = (0..others).map(|id| rows[&id].rejected_moves).sum();
    println!("{airborne_seen} player-ticks in the air on the server, {rejected} moves rejected");
    assert!(show || airborne_seen > 200, "the acrobats hardly left the ground ({airborne_seen} player-ticks)");
    let reasons: Vec<(u16, u8, u64)> = (0..others)
        .map(|id| &rows[&id])
        .filter(|r| r.rejected_moves > 0)
        .map(|r| (r.id, r.last_reject, r.last_reject_tick))
        .collect();
    assert_eq!(rejected, 0, "the server rejected the acrobats' moves (player, reason, tick): {reasons:?}");

    if show {
        let _ = std::fs::remove_dir_all(&work);
        return;
    }
    // the engine's units, as the animation made them
    let animated: Vec<Animated> = output.lines().filter_map(parse_animated_line).collect();
    let count = |f: &dyn Fn(&Animated) -> bool| animated.iter().filter(|a| f(a)).count();
    let in_the_air = count(&|a| a.flags & 1 != 0);
    let airborne_animation = count(&|a| a.flags & 1 != 0 && a.state == STATE_AIRBORNE);
    let crouched = count(&|a| a.flags & 2 != 0);
    let crouch_seat = count(&|a| a.flags & 2 != 0 && a.seat == SEAT_CROUCH);
    let standing_seat = count(&|a| a.flags & 2 == 0 && a.seat != SEAT_CROUCH);
    let landing = count(&|a| a.state == STATE_LAND_SOFT || a.state == STATE_LAND_HARD || a.landing >= 0);
    println!(
        "{} unit readings: {in_the_air} in the air ({airborne_animation} with the airborne animation), {crouched} \
         crouched ({crouch_seat} in the crouch seat), {standing_seat} standing in the standing seat, {landing} landing",
        animated.len()
    );
    assert!(in_the_air > 5, "no remote player was seen in the air");
    assert!(
        airborne_animation * 4 > in_the_air,
        "the engine played the airborne animation for {airborne_animation} of {in_the_air}"
    );
    assert!(crouched > 5, "no remote player was seen crouched");
    assert!(crouch_seat * 4 > crouched * 3, "the crouch seat for {crouch_seat} of {crouched}");
    assert!(standing_seat > 5, "no remote player was seen standing");
    assert!(landing > 0, "no remote player was seen landing");
    let _ = std::fs::remove_dir_all(&work);
}
