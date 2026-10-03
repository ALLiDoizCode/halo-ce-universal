//! The local player's view of a fight, in the headless C client against a Slayer match that `halo-server`
//! runs on the real Blood Gulch: what the HUD shows of the weapon, and what the game plays and shows of
//! shots, hits and deaths, for the game's own player and for the players around it. Runs of the game,
//! with simulated players beside it:
//!
//! - **the shooter**: the game aims at a simulated player and holds the trigger (`large.autofire`) until
//!   the magazine is empty and has been reloaded. The ammunition on the HUD is the library's
//!   ([`halo_sim::weapon::Hands`], which the comparison harness holds to the engine's weapon): the log
//!   has what the HUD reads and what the library says, once a second, and they are the same, and the
//!   engine's own weapon agreed with the library's on every tick. The sounds the engine was asked to
//!   start are logged (`large.log_sounds`), and the weapon's own (firing, the shell, the reload) are
//!   among them;
//! - **the bystander and the victim**: a simulated player on the wire (a UDP player, as a client is) fires
//!   its pistol at the real rate and says so in the flags of its input, as the client does; the game
//!   draws it with the pistol in its hand, fires the weapon the shots the states tell of, and plays what the
//!   weapon's tags say (the sounds of the shots, the impacts on a player's shield, the pain and the death
//!   of one that is hit and killed), and for the game's own player, hit by the same shooter, the screen
//!   and the sounds of being hit.
//!
//! Sounds cannot be seen: the evidence is the game's log of the sounds it is asked to start. The
//! pictures are for a person to look at (`HALO_SCREENSHOT_DIR`, `HALO_SCREENSHOT_EVERY`).
//!
//! It needs the game's own data, so it skips itself (and says why) without all of the variables
//! `headless.rs` lists (`HALO_STDB_BIN`, `HALO_MAP_DIR`, `HALO_GAME_BIN`, `HALO_DATA_ROOT`).
//! `HALO_HEADLESS_LOG` keeps the game's log. Run it with `--test-threads=1`: one game can run on a
//! machine at a time.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use halo_gateway::harness::{sim_public_key, Crowd, Impairment};
use halo_match_driver::server::{stdb_bin_dir, Server as Stdb};
use halo_match_driver::{MatchClient, PlayerClient};
use halo_server::admin::Admin;
use halo_server::fixtures::scratch;
use halo_server::maps::MapFiles;
use halo_server::servers::Log;
use halo_sim::combat::HitReport;
use halo_sim::weapon::Hands;
use halo_sim::PlayerInput;

const STATE_ALIVE: u8 = 0;
const STATE_DEAD: u8 = 1;

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}

/// The game's process, which never outlives the test; its log is kept (`HALO_HEADLESS_LOG`) however the test ends.
struct Game(std::process::Child, PathBuf);

impl Drop for Game {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        if let Some(keep) = env_path("HALO_HEADLESS_LOG") {
            let _ = std::fs::write(keep, read(&self.1));
        }
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

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Where a player stands in a fight: on the flat field of Blood Gulch, facing along `yaw`.
fn at(x: f32, y: f32, yaw: f32) -> PlayerInput {
    PlayerInput { player: 0, position: [x, y, 0.45], yaw, pitch: 0.0, flags: 0 }
}

struct Arena {
    owner: MatchClient,
    url: String,
    database: String,
    gateway: String,
    stdb: Stdb,
    running: halo_server::Running,
    data: PathBuf,
    game: PathBuf,
    work: PathBuf,
    dir: PathBuf,
}

/// A match on Blood Gulch where the players that join appear at `places` (the n-th for player n), with
/// Slayer to no score and a short respawn; `None` without the game's data.
fn arena(name: &str, places: &[PlayerInput]) -> Option<Arena> {
    let (Some(stdb_dir), Some(maps), Some(game), Some(data)) =
        (stdb_bin_dir(), env_path("HALO_MAP_DIR"), env_path("HALO_GAME_BIN"), env_path("HALO_DATA_ROOT"))
    else {
        eprintln!(
            "HALO_STDB_BIN, HALO_MAP_DIR, HALO_GAME_BIN and HALO_DATA_ROOT are not all set: skipping, \
             this test needs the game's own data and a game built with the library"
        );
        return None;
    };
    let dir = scratch(name);
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
id = "arena"
bind = "127.0.0.1:{udp}"
budget = 90000
send_threads = 2
log_secs = 5
handover_secs = 1
end_secs = 10
[[server.rotation]]
map = "bloodgulch"
seconds = 0
score_limit = 50
respawn_seconds = 3
suicide_penalty_seconds = 0
wave_seconds = 5
"#,
            token = dir.join("owner.token"),
        ),
        &dir,
    )
    .expect("a valid configuration");
    let (log, _lines) = Log::keeping();
    let running = halo_server::start(config, Arc::new(MapFiles { dir: maps.clone() }), log).expect("the server starts");
    let row = wait_for("the match", 60, || {
        running.root().servers().into_iter().find(|s| s.id == "arena" && !s.database.is_empty())
    });
    let owner = MatchClient::connect_as(&url, &row.database, Some(&stdb.owner().token));
    owner.set_spawn_points(places).unwrap();
    let work = std::env::temp_dir().join(format!("halo-effects-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    Some(Arena { owner, url, database: row.database, gateway: row.gateway, stdb, running, data, game, work, dir })
}

impl Arena {
    /// A simulated player, seated, who stands where the server spawned them. With `udp` it is a
    /// player on the wire (the n-th seat's key is `sim_public_key(n)`), who can say where it is
    /// and what its weapon does.
    fn seat(&self, key: u8, udp: bool) -> (PlayerClient, u16) {
        let admin = Admin::new(&self.url).unwrap();
        let account = admin.new_identity().unwrap();
        let client = PlayerClient::connect_unsubscribed(&self.url, &self.database, &account.token);
        let next = self.owner.seats().len() as u16;
        client.join(if udp { sim_public_key(next) } else { [key; 32] }).unwrap();
        let identity = spacetimedb_sdk::Identity::from_hex(&account.identity).unwrap();
        let id =
            wait_for("the seat", 20, || self.owner.seats().values().find(|s| s.owner == identity).map(|s| s.player));
        wait_for("the player to stand in the world", 20, || {
            (self.owner.players().contains_key(&id) && self.owner.fighters().contains_key(&id)).then_some(())
        });
        (client, id)
    }

    /// The wire of a seated player: a UDP player, welcomed by the gateway.
    fn wire(&self, id: u16, capacity: usize) -> Crowd {
        let gateway: SocketAddr = self.gateway.parse().unwrap();
        let crowd = Crowd::connect(gateway, id..id + 1, Impairment::none(), capacity, false);
        crowd.join_all(Duration::from_secs(20)).unwrap();
        crowd
    }

    /// The game, started on the arena's match for `seconds`, with `extra` in its environment.
    fn start_game(&self, seconds: u32, extra: &[(&str, &str)]) -> (Game, PathBuf) {
        let log_path = self.work.join("game.log");
        let log_file = std::fs::File::create(&log_path).unwrap();
        let game = Game(
            Command::new(&self.game)
                .current_dir(self.game.parent().unwrap())
                .env("HALO_DATA_ROOT", &self.data)
                .env("HALO_SAVE_ROOT", self.work.join("saves"))
                .env("HALO_LARGE_MAP", "bloodgulch")
                .env("HALO_LARGE_GATEWAY", &self.gateway)
                .env("HALO_LARGE_SPACETIMEDB", &self.url)
                .env("HALO_LARGE_DATABASE", &self.database)
                .env("HALO_LARGE_LOG", "1")
                .env("HALO_LARGE_LOG_SOUNDS", "1")
                .env("HALO_NET_ONLINE", "0")
                .env("HALO_FULLSCREEN", "0")
                .env("HALO_NO_VSYNC", "1")
                .env("HALO_NO_AUDIO", "1")
                .env("HALO_HIDDEN_WINDOW", "1")
                .env("HALO_UPDATE_ANSWER", "no")
                .env("HALO_EXIT_AFTER", seconds.to_string())
                .envs(
                    ["HALO_SCREENSHOT_DIR", "HALO_SCREENSHOT_EVERY", "HALO_MAX_FPS"]
                        .iter()
                        .filter_map(|n| Some((*n, std::env::var(n).ok()?))),
                )
                .envs(extra.iter().copied())
                .stdout(Stdio::from(log_file.try_clone().unwrap()))
                .stderr(Stdio::from(log_file))
                .spawn()
                .expect("start the game"),
            log_path.clone(),
        );
        (game, log_path)
    }

    fn tick(&self) -> u64 {
        self.owner.marker().unwrap().tick
    }

    /// The hit a simulated shooter reports on `target`, where the server has them now.
    fn hit_on(&self, target: u16, material: i16) -> HitReport {
        let p = &self.owner.players()[&target];
        HitReport {
            target,
            weapon: self.pistol().tag_index,
            material,
            host_tick: self.tick() as u32,
            origin: [p.x, p.y, p.z + 0.3],
            target_position: [p.x, p.y, p.z],
        }
    }

    /// Where the server has a player now, as the input that says they stand there.
    fn standing(&self, id: u16) -> PlayerInput {
        let p = &self.owner.players()[&id];
        PlayerInput { player: id, position: [p.x, p.y, p.z], yaw: p.yaw, pitch: p.pitch, flags: 0 }
    }

    /// The pistol, as the match's own map has it.
    fn pistol(&self) -> halo_map::combat::Weapon {
        let map = halo_map::HaloMap::from_path(self.data.join("maps/bloodgulch.map")).unwrap();
        map.combat.weapons.iter().find(|w| w.name == "weapons\\pistol\\pistol.weap").unwrap().clone()
    }

    fn finish(self, game: Game) {
        drop(game);
        let _ = self.running.stop();
        drop(self.stdb);
        let _ = std::fs::remove_dir_all(&self.work);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The sound tags the game was asked to start, in order, as `(game tick, tag)`.
fn sounds(log: &str) -> Vec<(i64, String)> {
    log.lines()
        .filter_map(|l| {
            let rest = l.split("large mode: sound ").nth(1)?;
            let (tag, rest) = rest.split_once(" class ")?;
            let tick = rest.split(" tick ").nth(1)?.split_whitespace().next()?.parse().ok()?;
            Some((tick, tag.to_string()))
        })
        .collect()
}

fn count_sounds(sounds: &[(i64, String)], containing: &str) -> usize {
    sounds.iter().filter(|(_, t)| t.contains(containing)).count()
}

/// A simulated shooter on the wire: every tick it holds the trigger of the pistol for the ticks `held`
/// says, and tells of it in the flags of its input, as the game's client does (the shot counter and the
/// reload). Returns how many shots it fired.
fn fire_for(
    crowd: &Crowd,
    id: u16,
    place: PlayerInput,
    pistol: &halo_map::combat::Weapon,
    seconds: f32,
    mut held: impl FnMut(u32) -> bool,
    mut on_shot: impl FnMut(u32),
) -> u32 {
    let mut hands = Hands::new(pistol);
    let mut counter = 0u8;
    let mut shots = 0u32;
    let start = Instant::now();
    let mut tick = 0u32;
    while start.elapsed().as_secs_f32() < seconds {
        let shot = hands.update(pistol, held(tick));
        if shot.fired {
            shots += 1;
            counter = (counter + 1) & 7;
            on_shot(tick);
        }
        let flags = halo_sim::with_shot_counter(if hands.reloading() { halo_sim::FLAG_RELOADING } else { 0 }, counter);
        crowd.send_inputs(&[PlayerInput { player: id, flags, ..place }]);
        tick += 1;
        let due = start + Duration::from_micros(tick as u64 * 1_000_000 / 30);
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
    }
    shots
}

#[derive(Debug)]
struct WeaponLine {
    hud_loaded: i32,
    hud_total: i32,
    library_loaded: i32,
    library_total: i32,
    shots_library: u32,
    shots_engine: u32,
    differed: u32,
}

/// "the local weapon ...: the HUD has 9 of 48 rounds, heat ... | the library 9 of 48, heat ... | shots:
/// library 3, engine 3, rounds differed on 0 ticks"
fn parse_weapon_line(line: &str) -> Option<WeaponLine> {
    let rest = line.split("large mode: the local weapon ").nth(1)?;
    let hud = rest.split("the HUD has ").nth(1)?;
    let (hud_loaded, hud) = hud.split_once(" of ")?;
    let (hud_total, _) = hud.split_once(" rounds")?;
    let library = rest.split("| the library ").nth(1)?;
    let (library_loaded, library) = library.split_once(" of ")?;
    let (library_total, _) = library.split_once(',')?;
    let shots = rest.split("shots: library ").nth(1)?;
    let (shots_library, shots) = shots.split_once(", engine ")?;
    let (shots_engine, shots) = shots.split_once(", rounds differed on ")?;
    let (differed, _) = shots.split_once(" ticks")?;
    Some(WeaponLine {
        hud_loaded: hud_loaded.trim().parse().ok()?,
        hud_total: hud_total.trim().parse().ok()?,
        library_loaded: library_loaded.trim().parse().ok()?,
        library_total: library_total.trim().parse().ok()?,
        shots_library: shots_library.trim().parse().ok()?,
        shots_engine: shots_engine.trim().parse().ok()?,
        differed: differed.trim().parse().ok()?,
    })
}

#[test]
fn the_hud_shows_the_libraries_ammunition_and_the_weapon_plays_its_sounds_for_the_game_that_shoots() {
    // the target stands where it spawned, four units from the game's player, who faces it; it dies
    // after five pistol hits and spawns again, so that the game goes on firing until it has
    // emptied its magazine and reloaded
    let Some(a) = arena("effects-shooter", &[at(84.0, -166.2, std::f32::consts::PI), at(80.0, -166.2, 0.0)]) else {
        return;
    };
    let (_target, target_id) = a.seat(1, false);
    assert_eq!(target_id, 0);
    let (game, log) = a.start_game(60, &[("HALO_LARGE_AUTOFIRE", "1")]);
    let _me = wait_for("the game's seat", 60, || a.owner.seats().values().map(|s| s.player).find(|p| *p != target_id));
    wait_for("the game to hold the server's weapon", 60, || {
        read(&log).contains("the local unit holds the server's weapon").then_some(())
    });

    // until the magazine has been emptied and reloaded: the HUD's rounds went to 0 or so and came back to 12
    wait_for("the game to fire a magazine and reload it", 90, || {
        let lines: Vec<WeaponLine> = read(&log).lines().filter_map(parse_weapon_line).collect();
        let low = lines.iter().position(|l| l.hud_loaded <= 3)?;
        lines[low..].iter().any(|l| l.hud_loaded >= 9 && l.hud_total < 48).then_some(())
    });
    std::thread::sleep(Duration::from_secs(3));
    let output = read(&log);
    let lines: Vec<WeaponLine> = output.lines().filter_map(parse_weapon_line).collect();
    println!("the weapon lines: {}", lines.len());
    for l in lines.iter().step_by(4) {
        println!("  {l:?}");
    }

    // the HUD shows the library's rounds, whatever the engine's weapon had
    assert!(lines.len() > 10, "only {} weapon lines", lines.len());
    for l in &lines {
        assert_eq!(
            (l.hud_loaded, l.hud_total),
            (l.library_loaded, l.library_total),
            "the HUD is not the library's: {l:?}"
        );
    }
    // ... and the engine's own weapon agreed with the library's on every tick: the same shots, the same rounds
    let last = lines.last().unwrap();
    assert!(last.shots_library >= 12, "the game fired {} shots", last.shots_library);
    assert_eq!(last.shots_engine, last.shots_library, "the engine's weapon and the library's model fired alike");
    assert!(last.differed <= 2, "the engine's rounds were not the library's on {} ticks", last.differed);
    // ... and a magazine was spent and reloaded: 12 of the 48, then most of 12 loaded again, with fewer in reserve
    assert!(lines.iter().any(|l| l.hud_loaded <= 3));
    assert!(lines.iter().any(|l| l.hud_loaded >= 9 && l.hud_total < 48), "the reload put rounds in the magazine");

    // the sounds the engine was asked to start: the weapon's own, in the numbers of the shots
    let heard = sounds(&output);
    let fired = count_sounds(&heard, "weapons\\pistol\\fire");
    println!("sounds: {} in all, {fired} pistol shots, {} reloads", heard.len(), count_sounds(&heard, "pistol_reload"));
    assert!(fired as u32 >= last.shots_library, "{fired} shot sounds for {} shots", last.shots_library);
    assert!(count_sounds(&heard, "pistol\\eject") >= 12, "the shell's sound");
    assert!(count_sounds(&heard, "pistol_reload") >= 1, "the reload's sound; the sounds were {:?}", {
        let mut tags: Vec<&str> = heard.iter().map(|(_, t)| t.as_str()).collect();
        tags.sort();
        tags.dedup();
        tags
    });
    // ... a kill: the death of the target
    assert!(count_sounds(&heard, "dialog\\chief\\death") >= 1, "the target's death");
    a.finish(game);
}

/// The part of `log` after the first line that has `marker` (nothing if none has).
fn after<'a>(log: &'a str, marker: &str) -> &'a str {
    match log.find(marker) {
        Some(at) => &log[at..],
        None => "",
    }
}

/// The last "remote shots: N told by the states, M fired by the engine's weapons" of the log: (N, M).
fn remote_shots(log: &str) -> (u32, u32) {
    let Some(line) = log.lines().rfind(|l| l.contains("large mode: remote shots:")) else { return (0, 0) };
    let number = |after: &str, before: &str| -> u32 {
        line.split(after).nth(1).and_then(|r| r.split(before).next()).and_then(|n| n.trim().parse().ok()).unwrap_or(0)
    };
    (number("remote shots: ", " told"), number("states, ", " fired"))
}

#[test]
fn players_in_range_fire_the_weapons_they_are_said_to_and_the_game_plays_what_is_hit_and_killed() {
    // the game's player faces the victim, nine units ahead; the shooter, a player on the wire, stands
    // to the side of the victim and fires across at it; the flanker, another, stands on the game's right
    // and fires at the game's player. Each fires its pistol at the real rate and says so in the flags of
    // its input, as the game's own client does
    let (shooter_at, victim_at, flanker_at, game_at) = (
        at(85.0, -169.7, std::f32::consts::FRAC_PI_2),
        at(85.0, -166.2, std::f32::consts::PI),
        at(76.0, -171.5, std::f32::consts::FRAC_PI_2),
        at(76.0, -166.2, 0.0),
    );
    let Some(a) = arena("effects-bystander", &[shooter_at, victim_at, flanker_at, game_at]) else {
        return;
    };
    let (shooter, shooter_id) = a.seat(1, true);
    let (_victim, victim_id) = a.seat(2, false);
    let (flanker, flanker_id) = a.seat(3, true);
    assert_eq!((shooter_id, victim_id, flanker_id), (0, 1, 2));
    // (where the server put them: the ground under the spawn point, which the wire's inputs must be on)
    let (shooter_at, flanker_at) = (a.standing(shooter_id), a.standing(flanker_id));
    let pistol = a.pistol();
    let (game, log) = a.start_game(90, &[]);
    let me = wait_for("the game's seat", 60, || {
        a.owner.seats().values().map(|s| s.player).find(|p| ![shooter_id, victim_id, flanker_id].contains(p))
    });
    wait_for("the game to see all three", 60, || read(&log).contains("large mode: 3 remote units").then_some(()));
    // (it gives the players their weapons as it creates them, and a weapon is drawn for a second or so
    // after, in which it fires nothing)
    wait_for("the weapons", 20, || (read(&log).matches("holds weapons\\pistol\\pistol").count() >= 3).then_some(()));
    std::thread::sleep(Duration::from_millis(2500));
    // (the wires are opened now: the gateway lets a player's session go when it hears nothing for a while,
    // and the game took some seconds to start)
    let shooter_wire = a.wire(shooter_id, 8);
    let began = Instant::now();
    let stamp = |what: &str| println!("[{:6.1} s] {what}", began.elapsed().as_secs_f32());

    // the shooter fires for two seconds: the shots, and nothing else
    stamp("the shooter fires for two seconds");
    let fired = fire_for(&shooter_wire, shooter_id, shooter_at, &pistol, 2.0, |_| true, |_| {});
    std::thread::sleep(Duration::from_secs(1));
    let output = read(&log);
    let (told, engine) = remote_shots(&output);
    let shot_sounds = count_sounds(&sounds(&output), "weapons\\pistol\\fire") as u32;
    println!(
        "the shooter fired {fired} shots; the states told of {told}, the engine's weapons fired {engine}, \
         and {shot_sounds} shot sounds were asked for"
    );
    assert!(fired >= 6, "{fired} shots in two seconds");
    // (the count of shots is told at the next state, so the one that was fired last may not have been)
    assert!(told + 1 >= fired && told <= fired + 1, "the states told of {told} of {fired} shots");
    assert!(engine + 1 >= told && engine <= told, "the engine's weapons fired {engine} of the {told} told");
    assert!(shot_sounds + 1 >= engine && shot_sounds <= engine + 1, "{shot_sounds} shot sounds for {engine} shots");

    // five hits on the victim, which the shooter reports as it fires, kill it
    stamp("five hits on the victim");
    let mut reported = 0;
    fire_for(
        &shooter_wire,
        shooter_id,
        shooter_at,
        &pistol,
        4.0,
        |tick| tick < 40,
        |_| {
            if reported < 5 {
                reported += 1;
                shooter.report_hits(&[a.hit_on(victim_id, 1)]).unwrap();
            }
        },
    );
    wait_for("the victim's death", 20, || (a.owner.standings()[&victim_id].state == STATE_DEAD).then_some(()));
    stamp("the victim is dead");
    std::thread::sleep(Duration::from_secs(2));
    let output = read(&log);
    let hurt = output.matches("remote player 1 is hurt by player 0 (weapons\\pistol\\pistol)").count();
    println!("the victim was shown hurt {hurt} times");
    assert!(hurt >= 3, "the victim, hit five times, was shown hurt {hurt} times");
    assert!(output.contains("player 1 is killed by weapons\\pistol\\pistol"), "the kill is the pistol's");
    let after_the_kill = sounds(after(&output, "player 1 is killed by"));
    assert!(count_sounds(&after_the_kill, "dialog\\chief\\death") >= 1, "the death's sound: {after_the_kill:?}");

    // the flanker hits the game's own player five times: the screen, the sounds of being hit, and the death
    wait_for("the game's player alive", 30, || (a.owner.standings()[&me].state == STATE_ALIVE).then_some(()));
    let flanker_wire = a.wire(flanker_id, 8);
    stamp("two hits on the game's player");
    let mut reported = 0;
    fire_for(
        &flanker_wire,
        flanker_id,
        flanker_at,
        &pistol,
        2.0,
        |tick| tick < 12,
        |_| {
            if reported < 2 {
                reported += 1;
                flanker.report_hits(&[a.hit_on(me, 1)]).unwrap();
            }
        },
    );
    wait_for("the hits to hurt the game's player", 20, || (a.owner.fighters()[&me].hurt_count >= 2).then_some(()));
    std::thread::sleep(Duration::from_secs(3));
    let output = read(&log);
    let hurt_me = output.matches("the local player 3 is hurt by player 2 (weapons\\pistol\\pistol)").count();
    println!("the game's player was shown hurt {hurt_me} times");
    assert_eq!(hurt_me, 2, "two hits on the game's player");
    let after_the_hit = sounds(after(&output, "the local player 3 is hurt by"));
    assert!(count_sounds(&after_the_hit, "ui\\shield_hit") >= 1, "the sound of the player's own shield being hit");
    stamp("three more: dead");
    let mut reported = 0;
    fire_for(
        &flanker_wire,
        flanker_id,
        flanker_at,
        &pistol,
        3.0,
        |tick| tick < 30,
        |_| {
            if reported < 3 {
                reported += 1;
                flanker.report_hits(&[a.hit_on(me, 1)]).unwrap();
            }
        },
    );
    wait_for("the game's player's death", 20, || (a.owner.standings()[&me].state == STATE_DEAD).then_some(()));
    std::thread::sleep(Duration::from_secs(3));
    stamp("done");
    let output = read(&log);
    assert!(
        output.contains("player 3 is killed by weapons\\pistol\\pistol"),
        "the game's player's death is the pistol's"
    );
    let after_my_death = sounds(after(&output, "player 3 is killed by"));
    assert!(count_sounds(&after_my_death, "dialog\\chief\\death") >= 1, "the sound of the player's own death");
    let everything = sounds(&output);
    let mut tags: Vec<&str> = everything.iter().map(|(_, t)| t.as_str()).collect();
    tags.sort();
    tags.dedup();
    println!("every sound tag asked for: {tags:#?}");
    a.finish(game);
}

/// "large mode: 120 frames in 1.000 s: 120.0 a second, the slowest 8.4 ms, 100 remote units": (frames a
/// second, the slowest frame in ms, remote units)
fn parse_frames_line(line: &str) -> Option<(f64, f64, u32)> {
    let rest = line.split("large mode: ").nth(1)?;
    let rate = rest.split(" s: ").nth(1)?.split(" a second").next()?.trim().parse().ok()?;
    let slowest = rest.split("the slowest ").nth(1)?.split(" ms").next()?.trim().parse().ok()?;
    let units = rest.rsplit(", ").next()?.split(" remote units").next()?.trim().parse().ok()?;
    Some((rate, slowest, units))
}

#[test]
fn a_hundred_players_in_view_with_their_weapons_are_drawn_at_the_frame_rate_the_game_reports() {
    // a hundred players in a block of ten by ten, a few units apart, in front of the game's player, who
    // faces them: all in view, each with the pistol in its hand. The game draws as fast as it can (no
    // frame limit); what it reports is the frames a second over each second. The floor the test asks
    // for is HALO_FPS_FLOOR (the frame-rate figure is only meaningful for an optimised build, and a
    // machine with nothing else running).
    const PLAYERS: usize = 100;
    let mut places = Vec::new();
    for i in 0..PLAYERS {
        places.push(at(82.0 + (i % 10) as f32 * 1.3, -172.0 + (i / 10) as f32 * 1.3, std::f32::consts::PI));
    }
    places.push(at(74.0, -166.2, 0.0));
    let Some(a) = arena("effects-crowd", &places) else { return };
    let seats: Vec<(PlayerClient, u16)> = (0..PLAYERS).map(|i| a.seat(i as u8 + 1, false)).collect();
    let (game, log) = a.start_game(45, &[("HALO_MAX_FPS", "-1")]);
    wait_for("the game to see the crowd", 90, || {
        read(&log).contains(&format!("large mode: {PLAYERS} remote units")).then_some(())
    });
    std::thread::sleep(Duration::from_secs(25));
    let output = read(&log);
    let frames: Vec<(f64, f64, u32)> =
        output.lines().filter_map(parse_frames_line).filter(|(_, _, units)| *units as usize == PLAYERS).collect();
    let mut rates: Vec<f64> = frames.iter().skip(5).map(|f| f.0).collect();
    rates.sort_by(f64::total_cmp);
    assert!(rates.len() >= 8, "only {} seconds of the crowd were logged", rates.len());
    let (lowest, median) = (rates[0], rates[rates.len() / 2]);
    let slowest = frames.iter().skip(5).map(|f| f.1).fold(0.0, f64::max);
    println!(
        "{PLAYERS} players in view: {} seconds measured, frames a second: lowest {lowest:.1}, median {median:.1}; \
         the slowest single frame {slowest:.1} ms",
        rates.len()
    );
    for line in output.lines().filter(|l| l.contains("the adapter cost")).skip(5).take(3) {
        println!("{line}");
    }
    let floor: f64 = std::env::var("HALO_FPS_FLOOR").ok().and_then(|n| n.parse().ok()).unwrap_or(0.0);
    assert!(median >= floor, "{median:.1} frames a second with {PLAYERS} players in view; the floor is {floor}");
    drop(seats);
    a.finish(game);
}
