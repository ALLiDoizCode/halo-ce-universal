//! The headless C client in a Team Slayer match of more than 128 simulated
//! players, the nearest 127 of them all on one team: the engine's own game
//! rules read only the player table it has, which holds the nearest 127 players
//! of the match, so they must not end the game, score it or show a result from
//! it. Only the server does (see the ticket "Engine game rules read a player
//! table that holds only the nearest 127 players").
//!
//! Two matches of a few hundred players: in one the nearest players are all on
//! the game's own team (the engine's table then holds one team only, and
//! `game_engine_should_end_game` ended the game within seconds before it was
//! guarded), in the other all on the other team. In each the game must still be
//! in the match after 30 seconds (the engine says in the log when its game
//! ends), and its scoreboard must be the server's: it lists every player of the
//! match and shows the server's team scores, which the test has the server
//! change (the engine's own table of 127 never saw those kills).
//!
//! It needs the game's own data, so it skips itself (and says why) without all
//! of the variables `headless.rs` lists (`HALO_STDB_BIN`, `HALO_MAP_DIR`,
//! `HALO_GAME_BIN`, `HALO_DATA_ROOT`); `HALO_HEADLESS_LOG` keeps the game's log
//! of the last match.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use halo_gateway::harness::{Rig, RigSetup};
use halo_match_driver::server::{build_module, stdb_bin_dir};
use halo_sim::rules::Rules;
use halo_sim::MapData;

/// The engine's player table holds this many players besides the game's own.
const ENGINE_REMOTES: usize = 127;
/// How long the match goes on with the engine holding its 127, seconds.
const MATCH_SECONDS: u64 = 32;
/// The game stops itself after this long (booting, loading and joining take a part of it).
const GAME_SECONDS: u32 = 80;

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}

struct Game(std::process::Child);

impl Drop for Game {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn read(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
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

/// A match of `others` simulated players and the game's own (the next player id), in which the
/// nearest 127 players to the game's player are all of the game's team (`same_team`), or all of
/// the other.
fn run_match(others: u16, same_team: bool) {
    let (Some(stdb), Some(maps), Some(game), Some(data)) =
        (stdb_bin_dir(), env_path("HALO_MAP_DIR"), env_path("HALO_GAME_BIN"), env_path("HALO_DATA_ROOT"))
    else {
        eprintln!(
            "HALO_STDB_BIN, HALO_MAP_DIR, HALO_GAME_BIN and HALO_DATA_ROOT are not all set: skipping, \
             this test needs the game's own data and a game built with the library"
        );
        return;
    };
    let me = others;

    // two bases as far apart as the map's starting locations are: the simulated players of even
    // ids (the roster puts them on the red team, as the teams take turns) are placed around the
    // first, the odd ones around the second; the game's player, the next id, is placed with
    // player 0
    let halo_map = halo_map::HaloMap::from_path(maps.join("bloodgulch.map")).expect("Blood Gulch");
    let starts: Vec<[f32; 3]> = halo_map.player_starts.iter().map(|s| s.position).collect();
    let dist = |a: &[f32; 3], b: &[f32; 3]| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
    let (mut red_base, mut blue_base) = (starts[0], starts[0]);
    for a in &starts {
        for b in &starts {
            if dist(a, b) > dist(&red_base, &blue_base) {
                (red_base, blue_base) = (*a, *b);
            }
        }
    }
    assert!(dist(&red_base, &blue_base) > 60.0, "the bases are only {} apart", dist(&red_base, &blue_base));
    let map = MapData::from(halo_map);
    let wasm = build_module();
    let rig = Rig::start(
        &stdb,
        &wasm,
        RigSetup { name: "engine-rules", map, anchors: &[red_base, blue_base], players: others, budget: 90_000 },
        |_| {},
    );
    // Team Slayer to 50 kills, which the 3 kills below do not come near
    let rules = Rules { teams: true, score_limit: 50, respawn_ticks: 90, suicide_penalty_ticks: 0, ..Rules::slayer() };
    rig.client.set_game(&rules).unwrap();

    let work = std::env::temp_dir().join(format!("halo-engine-rules-{}-{others}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    let log_path = work.join("game.log");
    let log_file = std::fs::File::create(&log_path).unwrap();
    let game_process = Game(
        Command::new(&game)
            .current_dir(game.parent().unwrap())
            .env("HALO_DATA_ROOT", &data)
            .env("HALO_SAVE_ROOT", work.join("saves"))
            .env("HALO_LARGE_MAP", "bloodgulch")
            .env("HALO_LARGE_GATEWAY", rig.gateway.local_addr().to_string())
            .env("HALO_LARGE_SPACETIMEDB", rig.server.uri())
            .env("HALO_LARGE_DATABASE", "engine-rules")
            // (the scoreboard up all the time, so that it is drawn, and logged)
            .env("HALO_LARGE_SCOREBOARD", "1")
            .env("HALO_NET_ONLINE", "0")
            .env("HALO_FULLSCREEN", "0")
            .env("HALO_NO_VSYNC", "1")
            .env("HALO_NO_AUDIO", "1")
            .env("HALO_HIDDEN_WINDOW", "1")
            .env("HALO_UPDATE_ANSWER", "no")
            .env("HALO_EXIT_AFTER", GAME_SECONDS.to_string())
            .stdout(Stdio::from(log_file.try_clone().unwrap()))
            .stderr(Stdio::from(log_file))
            .spawn()
            .expect("start the game"),
    );
    let mut game_process = game_process;

    // the game takes the next seat; the nearest 127 players to it are all on one team, as the test
    // is about
    wait_for("the game's seat", 90, || rig.client.roster().contains_key(&me).then_some(()));
    let my_team = rig.client.roster()[&me].team;
    let players = rig.client.players();
    let roster = rig.client.roster();
    let here = &players[&me];
    let mut by_distance: Vec<(f32, u16)> = players
        .values()
        .filter(|p| p.id != me)
        .map(|p| (((p.x - here.x).powi(2) + (p.y - here.y).powi(2) + (p.z - here.z).powi(2)).sqrt(), p.id))
        .collect();
    by_distance.sort_by(|a, b| a.0.total_cmp(&b.0));
    let nearest_teams: Vec<u8> = by_distance.iter().take(ENGINE_REMOTES).map(|(_, id)| roster[id].team).collect();
    println!(
        "the game is player {me} of {}, on team {my_team}; its {ENGINE_REMOTES} nearest players span {:.1} units, all of team {}",
        others + 1,
        by_distance[ENGINE_REMOTES - 1].0,
        nearest_teams[0]
    );
    assert!(
        nearest_teams.iter().all(|t| *t == nearest_teams[0]),
        "the {ENGINE_REMOTES} nearest players are of both teams"
    );
    assert_eq!(nearest_teams[0] == my_team, same_team, "the nearest players are of the wrong team for this test");

    // the game has the engine's 127 players, the nearest, and units alone for the rest
    let with_players = format!("{others} remote units, {ENGINE_REMOTES} with players");
    wait_for("the game to hold the match's players", 90, || read(&log_path).contains(&with_players).then_some(()));
    let began = Instant::now();
    println!("the game holds {with_players}");

    // the match goes on: the server has some kills of the red team's (the engine's table of the
    // nearest players cannot show these), and the game must neither end its game nor stop drawing
    // the server's scoreboard
    let red_killers = (0..others).filter(|id| rig.client.roster()[id].team == 0).collect::<Vec<_>>();
    let blue_victims = (0..others).filter(|id| rig.client.roster()[id].team == 1).collect::<Vec<_>>();
    std::thread::sleep(Duration::from_secs(5));
    for i in 0..3 {
        rig.client.report_death(blue_victims[i], Some(red_killers[i])).unwrap();
        std::thread::sleep(Duration::from_millis(300));
    }
    let want_scores = "Red 3   Blue 0";
    wait_for("the server's scores", 20, || {
        rig.client.game().filter(|g| (g.red_score, g.blue_score) == (3, 0)).map(|_| ())
    });
    let mut ended_early = None;
    while began.elapsed() < Duration::from_secs(MATCH_SECONDS) {
        let log = read(&log_path);
        if log.contains("game engine: the game ends") {
            ended_early = Some(began.elapsed());
            break;
        }
        assert!(game_process.0.try_wait().unwrap().is_none(), "the game stopped:\n{}", tail(&log));
        std::thread::sleep(Duration::from_millis(250));
    }
    let output = read(&log_path);
    if let Some(keep) = env_path("HALO_HEADLESS_LOG") {
        let _ = std::fs::write(keep, &output);
    }
    assert!(ended_early.is_none(), "the engine ended the game {:?} into the match:\n{}", ended_early, tail(&output));
    assert!(game_process.0.try_wait().unwrap().is_none(), "the game stopped:\n{}", tail(&output));
    assert!(!output.contains("Game over"), "the scoreboard says the game is over:\n{}", tail(&output));
    let game_state = rig.client.game().unwrap();
    assert_eq!(game_state.ending, 0, "the server has not ended the match");

    // the scoreboard is the server's: all of the match's players, and the server's team scores
    let scoreboards: Vec<&str> = output.lines().filter(|l| l.contains("game engine: the scoreboard lists ")).collect();
    assert!(!scoreboards.is_empty(), "the game never drew its scoreboard:\n{}", tail(&output));
    let listed = format!("the scoreboard lists {} players", others + 1);
    assert!(
        scoreboards.iter().any(|l| l.contains(&listed)),
        "no scoreboard listed all {} players: {scoreboards:?}",
        others + 1
    );
    assert!(
        scoreboards.last().unwrap().contains(want_scores) && scoreboards.last().unwrap().contains(&listed),
        "the last scoreboard was not the server's ({want_scores}): {:?}",
        scoreboards.last()
    );
    println!(
        "the engine did not end the game in {MATCH_SECONDS} s; the last scoreboard: {}",
        scoreboards.last().unwrap()
    );
    let _ = std::fs::remove_dir_all(&work);
}

fn tail(log: &str) -> String {
    log.lines().rev().take(15).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")
}

/// 300 players and the game's own: an even number, so the game's player is on the red team, with
/// the red players (150 of them) around it.
#[test]
fn the_engine_does_not_end_the_match_when_the_nearest_players_are_all_of_the_local_players_team() {
    run_match(300, true);
}

/// 301 players and the game's own: the game's player is on the blue team, with the red players
/// around it.
#[test]
fn the_engine_does_not_end_the_match_when_the_nearest_players_are_all_of_the_other_team() {
    run_match(301, false);
}
