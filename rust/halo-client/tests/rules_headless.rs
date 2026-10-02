//! The headless C client in a Slayer match run by `halo-server` on the real
//! Blood Gulch, with simulated players seated beside it: the server spawns the
//! game's player (told that no starting location is free, it waits for a
//! wave and then spawns in it), a death the server decides on is the game's
//! player's death and respawn, the scores are the server's, the match ends at
//! its score limit with a final scoreboard that lists every player (the ones
//! out of range too), and the server moves on to its next match. It covers
//! the C adapter and the engine's hooks for all of it.
//!
//! It needs the game's own data, so it skips itself (and says why) without all
//! of the variables `headless.rs` lists (`HALO_STDB_BIN`, `HALO_MAP_DIR`,
//! `HALO_GAME_BIN`, `HALO_DATA_ROOT`). With `HALO_SCREENSHOT_DIR` (and
//! `HALO_SCREENSHOT_EVERY`) the game saves frames, which show the scoreboard
//! and the wave message; `HALO_HEADLESS_LOG` keeps the game's log.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use halo_match_driver::server::{stdb_bin_dir, Server as Stdb};
use halo_match_driver::{MatchClient, PlayerClient};
use halo_server::admin::Admin;
use halo_server::fixtures::scratch;
use halo_server::maps::MapFiles;
use halo_server::servers::Log;

/// Players seated beside the game's (the map has 16 starting locations for Slayer).
/// (`HALO_HEADLESS_PLAYERS` says another number, such as 200: more than the engine's 127 remote players)
fn simulated_count() -> usize {
    std::env::var("HALO_HEADLESS_PLAYERS").ok().and_then(|n| n.parse().ok()).unwrap_or(40)
}
/// Seconds between the match being announced and the first wave of players
/// told that no starting location is free: the game has to start, load Blood
/// Gulch and join before it, to be told to wait for it.
const WAVE_SECONDS: u32 = 30;
const END_SECONDS: u32 = 14;
const GAME_SECONDS: u32 = 100;

/// A simulated player's SpacetimeDB identity.
fn ident(account: &halo_server::admin::Account) -> spacetimedb_sdk::Identity {
    spacetimedb_sdk::Identity::from_hex(&account.identity).expect("a hex identity")
}

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

/// The game's log, as it is so far.
fn read(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// The numbers in the first `(x y z)` after `marker` in `text`.
fn triple_after(text: &str, marker: &str) -> Option<[f32; 3]> {
    let rest = text.split(marker).nth(1)?;
    let (open, close) = (rest.find('(')?, rest.find(')')?);
    let mut n = rest[open + 1..close].split_whitespace().map(|n| n.parse::<f32>());
    Some([n.next()?.ok()?, n.next()?.ok()?, n.next()?.ok()?])
}

#[test]
fn the_game_is_spawned_by_the_server_told_to_wait_for_a_wave_killed_and_respawned_and_sees_the_match_end() {
    let (Some(stdb_dir), Some(maps), Some(game), Some(data)) =
        (stdb_bin_dir(), env_path("HALO_MAP_DIR"), env_path("HALO_GAME_BIN"), env_path("HALO_DATA_ROOT"))
    else {
        eprintln!(
            "HALO_STDB_BIN, HALO_MAP_DIR, HALO_GAME_BIN and HALO_DATA_ROOT are not all set: skipping, \
             this test needs the game's own data and a game built with the library"
        );
        return;
    };

    // the server: Blood Gulch, Slayer to 3 kills, waves, and a long final scoreboard
    let dir = scratch("rules-headless");
    let stdb = Stdb::start(&stdb_dir);
    let url = stdb.uri();
    std::fs::write(dir.join("owner.token"), &stdb.owner().token).unwrap();
    let (root_wasm, match_wasm) = halo_server::fixtures::modules();
    let udp = halo_server::fixtures::free_udp_pair();
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
log_secs = 5
handover_secs = 1
end_secs = {END_SECONDS}
[[server.rotation]]
map = "bloodgulch"
seconds = 0
score_limit = 3
respawn_seconds = 3
suicide_penalty_seconds = 0
wave_seconds = {WAVE_SECONDS}
[[server.rotation]]
map = "sidewinder"
seconds = 0
"#,
            token = dir.join("owner.token"),
        ),
        &dir,
    )
    .expect("a valid configuration");
    let (log, lines) = Log::keeping();
    let running = halo_server::start(config, Arc::new(MapFiles { dir: maps.clone() }), log).expect("the server starts");
    let row = wait_for("the match", 60, || {
        running.root().servers().into_iter().find(|s| s.id == "lounge" && !s.database.is_empty())
    });
    println!("the match: {} on {}, up to {} players", row.database, row.map, row.capacity);
    let began = Instant::now();
    assert_eq!(row.capacity, 500, "Blood Gulch's own cap, as none was configured");

    // simulated players take seats: the first 16 spawn at the starting locations, the rest are
    // told to wait for the wave
    let owner = MatchClient::connect_as(&url, &row.database, Some(&stdb.owner().token));
    let admin = Admin::new(&url).unwrap();
    let simulated: Vec<(PlayerClient, halo_server::admin::Account)> = (0..simulated_count())
        .map(|i| {
            let account = admin.new_identity().unwrap();
            let client = PlayerClient::connect_unsubscribed(&url, &row.database, &account.token);
            client.join([i as u8 + 1; 32]).unwrap();
            (client, account)
        })
        .collect();
    wait_for("the simulated players' standings", 20, || (owner.standings().len() == simulated_count()).then_some(()));
    let at_a_start = owner.standings().values().filter(|s| s.state == 0).count();
    let waiting = owner.standings().values().filter(|s| s.state == 2).count();
    println!(
        "{at_a_start} of the {} simulated players spawned at a starting location, {waiting} wait for the wave",
        simulated_count()
    );
    assert_eq!(at_a_start + waiting, simulated_count());
    assert!(waiting >= simulated_count() - 16 && at_a_start <= 16, "Blood Gulch has 16 starting locations for Slayer");

    // the game
    let work = std::env::temp_dir().join(format!("halo-rules-headless-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    let log_path = work.join("game.log");
    let log_file = std::fs::File::create(&log_path).unwrap();
    let mut game_process = Game(
        Command::new(&game)
            .current_dir(game.parent().unwrap())
            .env("HALO_DATA_ROOT", &data)
            .env("HALO_SAVE_ROOT", work.join("saves"))
            .env("HALO_LARGE_MAP", "bloodgulch")
            .env("HALO_LARGE_GATEWAY", &row.gateway)
            .env("HALO_LARGE_SPACETIMEDB", &url)
            .env("HALO_LARGE_DATABASE", &row.database)
            // (the scoreboard up all the time, for the pictures; the test's own HALO_LARGE_SCOREBOARD=0
            // leaves it to the game's end)
            .env_remove("HALO_LARGE_SCOREBOARD")
            .envs(
                (std::env::var("HALO_LARGE_SCOREBOARD").as_deref() != Ok("0"))
                    .then_some(("HALO_LARGE_SCOREBOARD", "1")),
            )
            .env("HALO_NET_ONLINE", "0")
            .env("HALO_FULLSCREEN", "0")
            .env("HALO_NO_VSYNC", "1")
            .env("HALO_NO_AUDIO", "1")
            .env("HALO_HIDDEN_WINDOW", "1")
            .env("HALO_UPDATE_ANSWER", "no")
            .env("HALO_EXIT_AFTER", GAME_SECONDS.to_string())
            .envs(
                ["HALO_SCREENSHOT_DIR", "HALO_SCREENSHOT_EVERY"]
                    .iter()
                    .filter_map(|n| Some((*n, std::env::var(n).ok()?))),
            )
            .stdout(Stdio::from(log_file.try_clone().unwrap()))
            .stderr(Stdio::from(log_file))
            .spawn()
            .expect("start the game"),
    );

    // the game takes a seat; no starting location is free, so the server tells it to wait for the wave
    let me = wait_for("the game's seat", 60, || {
        let mine: Vec<u16> = owner
            .seats()
            .values()
            .filter(|s| simulated.iter().all(|(_, a)| ident(a) != s.owner))
            .map(|s| s.player)
            .collect();
        mine.first().copied()
    });
    println!("the game is player {me}, {:.1} s after the match began", began.elapsed().as_secs_f32());
    wait_for("the game to be told to wait", 30, || (owner.standings().get(&me)?.state == 2).then_some(()));
    let wave_at = owner.standings()[&me].due_tick;
    println!("the server tells it to wait for the wave at tick {wave_at}");
    wait_for("the log to say it waits", 30, || {
        read(&log_path).contains("the server says the local player is waiting").then_some(())
    });
    assert!(
        !read(&log_path).contains("the local unit is where the server has the player"),
        "the game placed a unit before the server had spawned the player"
    );

    // the wave spawns it (beside a starting location, at least a pill's width from everyone)
    wait_for("the wave", 60, || (owner.standings().get(&me)?.state == 0).then_some(()));
    let spawn = owner.standings()[&me].clone();
    println!("spawned in the wave at ({:.2} {:.2} {:.2}), spawn {}", spawn.x, spawn.y, spawn.z, spawn.spawns);
    assert_eq!(spawn.spawns, 1);
    let first_unit = wait_for("the unit to be put where the server spawned the player", 20, || {
        triple_after(&read(&log_path), "the local unit is where the server has the player")
    });
    for (axis, v) in [spawn.x, spawn.y, spawn.z].iter().enumerate() {
        assert!(
            (first_unit[axis] - v).abs() < 0.001,
            "the unit is at {first_unit:?}, the server spawned the player at {spawn:?}"
        );
    }
    // everyone placed so far is clear of everyone else
    wait_for("every simulated player in the world", 60, || {
        owner.standings().values().all(|s| s.state == 0).then_some(())
    });
    let players = owner.players();
    assert_eq!(players.len(), simulated_count() + 1);
    let points: Vec<[f32; 3]> = players.values().map(|p| [p.x, p.y, p.z]).collect();
    for (i, a) in points.iter().enumerate() {
        for b in &points[i + 1..] {
            let d = ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
            assert!(d >= 0.4, "two players {d} apart");
        }
    }

    // the server kills the game's player (credited to simulated player 0): the unit is killed,
    // the HUD counts down, and the server respawns the player when the timer is out
    std::thread::sleep(Duration::from_secs(2));
    let killer = owner.seats().values().find(|s| s.owner == ident(&simulated[0].1)).unwrap().player;
    owner.report_death(me, Some(killer)).unwrap();
    wait_for("the death", 10, || (owner.standings().get(&me)?.state == 1).then_some(()));
    assert_eq!(owner.standings()[&killer].score, 1, "the kill is credited to the killer");
    wait_for("the log to say it is dead", 10, || {
        read(&log_path).contains("the server says the local player is dead: the unit is killed").then_some(())
    });
    std::thread::sleep(Duration::from_secs(1));
    wait_for("the respawn", 60, || (owner.standings().get(&me)?.state == 0).then_some(()));
    let respawn = owner.standings()[&me].clone();
    assert_eq!(respawn.spawns, 2);
    wait_for("the unit to be put where the server respawned the player", 20, || {
        let log = read(&log_path);
        let at = log.rfind("the local unit is where the server has the player")?;
        let unit = triple_after(&log[at..], "the local unit is where the server has the player")?;
        log[at..].contains("spawn 2").then_some(unit)
    })
    .iter()
    .zip([respawn.x, respawn.y, respawn.z])
    .for_each(|(got, want)| assert!((got - want).abs() < 0.001, "respawned at {want}, the unit is at {got}"));

    // three kills by the game's player end the match at the score limit
    std::thread::sleep(Duration::from_secs(2));
    for victim in 1..=3u16 {
        let victim_id =
            owner.seats().values().find(|s| s.owner == ident(&simulated[victim as usize].1)).unwrap().player;
        owner.report_death(victim_id, Some(me)).unwrap();
        std::thread::sleep(Duration::from_millis(300));
    }
    wait_for("the end of the match", 20, || (owner.game()?.ending == 1).then_some(()));
    let game_state = owner.game().unwrap();
    println!("the match ended: winner kind {} player {}", game_state.winner_kind, game_state.winner);
    assert_eq!((game_state.winner_kind, game_state.winner), (1, me));
    assert_eq!(owner.standings()[&me].score, 3);

    // the final scoreboard is up for end_secs, and then the server's next match is announced
    wait_for("the log to say the game has ended", 10, || {
        let wanted = format!("has ended: its score limit was reached, won by Player {me}");
        lines.lock().unwrap().iter().any(|l| l.contains(&wanted)).then_some(())
    });
    let ended_at = Instant::now();
    let next = wait_for("the next match", 60, || {
        running.root().servers().into_iter().find(|s| s.id == "lounge" && s.match_number == 2 && !s.database.is_empty())
    });
    let shown_for = ended_at.elapsed();
    println!("the next match, on {}, was announced {:.1} s after the end", next.map, shown_for.as_secs_f32());
    assert!(
        shown_for >= Duration::from_secs(END_SECONDS as u64 - 3),
        "the final scoreboard was up for only {shown_for:?}"
    );
    assert_eq!(next.map, "sidewinder", "the rotation moves on");

    // the game ran out its time
    let status = {
        let until = Instant::now() + Duration::from_secs(GAME_SECONDS as u64 + 30);
        loop {
            if let Some(status) = game_process.0.try_wait().unwrap() {
                break Some(status);
            }
            if Instant::now() > until {
                break None;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    };
    let output = read(&log_path);
    if let Some(keep) = env_path("HALO_HEADLESS_LOG") {
        let _ = std::fs::write(keep, &output);
    }
    assert!(
        status.is_some_and(|s| s.success()),
        "the game did not exit by itself:\n{}",
        output.lines().rev().take(15).collect::<Vec<_>>().join("\n")
    );
    assert!(
        output.contains("the server says the local player is alive (spawns 1"),
        "the game never heard it was alive"
    );
    assert!(
        output.contains("the server says the local player is alive (spawns 2"),
        "the game never heard it respawned"
    );

    let _ = running.stop();
    drop(simulated);
    let _ = std::fs::remove_dir_all(&work);
    let _ = std::fs::remove_dir_all(&dir);
}
