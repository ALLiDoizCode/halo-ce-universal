//! Fighting, in the headless C client against a Slayer match that `halo-server`
//! runs on the real Blood Gulch: the game's own engine fires the pistol and
//! reports what it hits, the server checks the reports and deals the damage, and
//! what the server says of each player's health and shields is what the engine
//! shows. Runs of the game, each with simulated players beside it:
//!
//! - **the shooter**: the game aims at a simulated player and holds the trigger
//!   (`large.autofire`); the server kills the player with the game's reported hits,
//!   credits the game's player with the kill, and the player respawns; the same
//!   with the plasma rifle (bolts that take time to fly, and heat);
//! - **the victim**: a simulated player reports hits on the game's player, as a
//!   client would: the game's HUD shows the shield going down, staying down for
//!   the stun and recharging, and the game's player is killed by hits and spawns
//!   again;
//! - **the bystander**: two simulated players fight in front of the game, which
//!   sees the shield of the one that is hit and the unit of the one that is
//!   killed fall down.
//!
//! It needs the game's own data, so it skips itself (and says why) without all
//! of the variables `headless.rs` lists (`HALO_STDB_BIN`, `HALO_MAP_DIR`,
//! `HALO_GAME_BIN`, `HALO_DATA_ROOT`). With `HALO_SCREENSHOT_DIR` (and
//! `HALO_SCREENSHOT_EVERY`) the game saves frames, which show what each of the
//! players sees; `HALO_HEADLESS_LOG` keeps the game's log. Run it with
//! `--test-threads=1`: one game can run on a machine at a time.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use halo_match_driver::server::{stdb_bin_dir, Server as Stdb};
use halo_match_driver::{MatchClient, PlayerClient};
use halo_server::admin::Admin;
use halo_server::fixtures::scratch;
use halo_server::maps::MapFiles;
use halo_server::servers::Log;
use halo_sim::combat::HitReport;
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

/// What the test needs: the paths and the connections.
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
    let work = std::env::temp_dir().join(format!("halo-combat-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    Some(Arena { owner, url, database: row.database, gateway: row.gateway, stdb, running, data, game, work, dir })
}

impl Arena {
    /// A simulated player, seated, who stands where the server spawned them.
    fn seat(&self, key: u8) -> (PlayerClient, u16) {
        let admin = Admin::new(&self.url).unwrap();
        let account = admin.new_identity().unwrap();
        let client = PlayerClient::connect_unsubscribed(&self.url, &self.database, &account.token);
        client.join([key; 32]).unwrap();
        let identity = spacetimedb_sdk::Identity::from_hex(&account.identity).unwrap();
        let id =
            wait_for("the seat", 20, || self.owner.seats().values().find(|s| s.owner == identity).map(|s| s.player));
        wait_for("the player to stand in the world", 20, || {
            (self.owner.players().contains_key(&id) && self.owner.fighters().contains_key(&id)).then_some(())
        });
        (client, id)
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
                .envs(extra.iter().copied())
                .stdout(Stdio::from(log_file.try_clone().unwrap()))
                .stderr(Stdio::from(log_file))
                .spawn()
                .expect("start the game"),
            log_path.clone(),
        );
        (game, log_path)
    }

    fn rejected_hits(&self) -> u64 {
        self.owner.marker().unwrap().rejected_hits_total
    }

    fn tick(&self) -> u64 {
        self.owner.marker().unwrap().tick
    }

    /// The hit a simulated shooter reports on `target`, where the server has them now.
    fn hit_on(&self, target: u16, material: i16) -> HitReport {
        let p = &self.owner.players()[&target];
        HitReport {
            target,
            damage: self.pistol_damage(),
            material,
            scale: 1.0,
            host_tick: self.tick() as u32,
            origin: [p.x, p.y, p.z + 0.3],
            target_position: [p.x, p.y, p.z],
        }
    }

    /// The damage effect tag of the pistol's bullet in Blood Gulch, as the match's own map has it.
    fn pistol_damage(&self) -> u16 {
        let map = halo_map::HaloMap::from_path(self.data.join("maps/bloodgulch.map")).unwrap();
        let pistol = map.combat.weapons.iter().find(|w| w.name == "weapons\\pistol\\pistol.weap").unwrap();
        pistol.triggers[0].projectile.as_ref().unwrap().impact_damage.unwrap().tag_index
    }

    fn finish(self, game: Game) {
        drop(game);
        let _ = self.running.stop();
        drop(self.stdb);
        let _ = std::fs::remove_dir_all(&self.work);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn the_game_shoots_a_player_dead_with_the_pistol_and_the_server_counts_the_kill() {
    // the target stands where it spawned, four units from the game's player, who faces it
    let Some(a) = arena("combat-shooter", &[at(84.0, -166.2, std::f32::consts::PI), at(80.0, -166.2, 0.0)]) else {
        return;
    };
    let (_target, target_id) = a.seat(1);
    assert_eq!(target_id, 0);
    let (game, log) = a.start_game(110, &[("HALO_LARGE_AUTOFIRE", "1")]);
    let me = wait_for("the game's seat", 60, || a.owner.seats().values().map(|s| s.player).find(|p| *p != target_id));
    println!("the game is player {me}");
    wait_for("the game to hold the server's weapon", 60, || {
        read(&log).contains("the local unit holds the server's weapon").then_some(())
    });

    // the game's engine fires, reports the hit and the server hurts the target
    wait_for("the first hit to hurt the target", 60, || (a.owner.fighters()[&target_id].hurt_count > 0).then_some(()));
    let hurt = a.owner.fighters()[&target_id].clone();
    println!(
        "the first hit: the target's shield {:.3}, health {:.3}, hurt by player {}",
        hurt.shield, hurt.body, hurt.hurt_by
    );
    assert_eq!(hurt.hurt_by, me, "the hit is the game's player's");
    assert!(hurt.shield < 1.0 && hurt.shield > 0.0);

    // ... until the target is dead, and the kill is the game's
    wait_for("the target's death", 60, || (a.owner.standings()[&target_id].state == STATE_DEAD).then_some(()));
    let killed = a.owner.standings();
    println!(
        "the target died: the game's player has {} points, the target {} deaths",
        killed[&me].score, killed[&target_id].deaths
    );
    assert_eq!(killed[&me].score, 1, "the kill is the game's");
    assert_eq!(killed[&target_id].deaths, 1);
    let counted = |text: &str| text.matches("large mode: a hit on player").count();
    assert!(counted(&read(&log)) >= 1, "the engine's hooks reported hits:\n{}", read(&log));

    // the target respawns, somewhere the rules choose (not necessarily in the game's sight), with a
    // fresh body and shield
    wait_for("the target's respawn", 60, || (a.owner.standings()[&target_id].state == STATE_ALIVE).then_some(()));
    wait_for("the respawned target's living fighter", 20, || {
        let f = a.owner.fighters()[&target_id].clone();
        (f.flags & 2 == 0 && f.body > 0.0).then_some(())
    });
    let output = read(&log);
    let reported: Vec<&str> = output.lines().filter(|l| l.contains("hits reported in")).collect();
    println!("the game's last word on its reports: {:?}", reported.last());
    println!(
        "the server refused {} hit reports, and took {}",
        a.rejected_hits(),
        a.owner.fighters()[&target_id].hurt_count
    );
    assert!(!reported.is_empty(), "the game says how many hits it reported:\n{output}");
    assert!(
        a.rejected_hits() <= 3,
        "the engine's reports pass the server's checks: {} refused of the game's hits",
        a.rejected_hits()
    );
    a.finish(game);
}

/// A weapon's tag index in Blood Gulch, as the match's own map has it.
fn weapon_tag(data: &Path, name: &str) -> u16 {
    let map = halo_map::HaloMap::from_path(data.join("maps/bloodgulch.map")).unwrap();
    map.combat.weapons.iter().find(|w| w.name == name).unwrap_or_else(|| panic!("no {name} in the map")).tag_index
}

/// The game holds `weapon` (which the owner keeps putting in the player's hands from the moment they are seated: the
/// server gives a player who spawns the starting weapon, and the game takes the weapon the server says when its unit
/// first spawns, so the weapon has to be there when it does), shoots the player in front of it dead with it, and
/// the server counts the kill as the game's; `what` says what the weapon is for the log.
fn the_game_kills_with(name: &str, weapon: &str, what: &str) {
    let Some(a) = arena(name, &[at(84.0, -166.2, std::f32::consts::PI), at(80.0, -166.2, 0.0)]) else {
        return;
    };
    let (_target, target_id) = a.seat(1);
    assert_eq!(target_id, 0);
    let (game, log) = a.start_game(130, &[("HALO_LARGE_AUTOFIRE", "1")]);
    let me = wait_for("the game's seat", 60, || a.owner.seats().values().map(|s| s.player).find(|p| *p != target_id));
    let tag = weapon_tag(&a.data, weapon);
    wait_for("the game to take the weapon", 60, || {
        a.owner.set_loadout(me, tag, u16::MAX).ok()?;
        read(&log).contains("the local unit is given").then_some(())
    });
    wait_for("the game to hold the server's weapon", 60, || {
        read(&log).contains("the local unit holds the server's weapon").then_some(())
    });
    let output = read(&log);
    assert!(output.contains(&format!("the local unit is given {}", weapon.trim_end_matches(".weap"))), "{output}");
    println!("the game is player {me}, holding {what}");

    wait_for("the first hit to hurt the target", 60, || (a.owner.fighters()[&target_id].hurt_count > 0).then_some(()));
    assert_eq!(a.owner.fighters()[&target_id].hurt_by, me, "the hit is the game's player's");
    wait_for("the target's death", 60, || (a.owner.standings()[&target_id].state == STATE_DEAD).then_some(()));
    let standings = a.owner.standings();
    println!("the target died: the game's player has {} points", standings[&me].score);
    assert_eq!(standings[&me].score, 1, "the kill is the game's");
    assert_eq!(standings[&target_id].deaths, 1);
    let reports = read(&log).matches("large mode: a hit on player").count();
    assert!(reports >= 1, "the engine's hooks reported hits:\n{}", read(&log));
    println!("the server refused {} hit reports", a.rejected_hits());
    assert!(
        a.rejected_hits() <= 3,
        "the engine's reports of {what} pass the server's checks: {} refused",
        a.rejected_hits()
    );
    a.finish(game);
}

#[test]
fn the_game_shoots_a_player_dead_with_the_plasma_rifle_whose_bolts_take_time_to_fly() {
    the_game_kills_with("combat-plasma-rifle", "weapons\\plasma rifle\\plasma rifle.weap", "the plasma rifle");
}

#[test]
fn a_player_hit_by_reports_loses_shield_and_health_in_the_hud_recharges_dies_and_respawns() {
    // the game's player faces the simulated shooter, four units away
    let Some(a) = arena("combat-victim", &[at(80.0, -166.2, 0.0), at(84.0, -166.2, std::f32::consts::PI)]) else {
        return;
    };
    let (shooter, shooter_id) = a.seat(1);
    assert_eq!(shooter_id, 0);
    let (game, log) = a.start_game(150, &[]);
    let me = wait_for("the game's seat", 60, || a.owner.seats().values().map(|s| s.player).find(|p| *p != shooter_id));
    wait_for("the game's player in the world", 60, || {
        (a.owner.standings().get(&me)?.state == STATE_ALIVE && a.owner.fighters().contains_key(&me)).then_some(())
    });
    wait_for("the game to hold the server's weapon", 60, || {
        read(&log).contains("the local unit holds the server's weapon").then_some(())
    });
    std::thread::sleep(Duration::from_secs(3));
    let began = Instant::now();
    let stamp = |what: &str| println!("[{:6.1} s] {what}", began.elapsed().as_secs_f32());

    // two hits, a third of the shield each
    stamp("two hits");
    shooter.report_hits(&[a.hit_on(me, 1)]).unwrap();
    std::thread::sleep(Duration::from_millis(400));
    shooter.report_hits(&[a.hit_on(me, 1)]).unwrap();
    wait_for("the server to deal them", 20, || (a.owner.fighters()[&me].hurt_count == 2).then_some(()));
    let hurt = a.owner.fighters()[&me].clone();
    println!(
        "the game's player: shield {:.3}, health {:.3}, shield stun {} ticks",
        hurt.shield, hurt.body, hurt.shield_stun_ticks
    );
    assert!((hurt.shield - (1.0 - 2.0 * 25.0 / 75.0)).abs() < 1e-4);
    assert_eq!((hurt.body, hurt.hurt_by), (1.0, shooter_id));
    // the game's engine says its player's shield is down by a third and two
    wait_for("the game's log to say the shield is down", 15, || {
        read(&log).contains("the local player's shield is 0.333").then_some(())
    });
    stamp("the game shows the shield at a third");

    // the shield stays down for the stun (6 s), then comes back (4 s)
    wait_for("the shield to be whole again", 40, || {
        let text = read(&log);
        let at = text.rfind("the local player's shield is 0.333")?;
        text[at..].contains("the local player's shield is 1.000").then_some(())
    });
    stamp("the game shows the shield whole again");
    assert_eq!(a.owner.fighters()[&me].shield, hurt.shield, "the server wrote no recharge: the clients count it");

    // five hits: the game's player is dead, and spawns again
    stamp("five hits");
    shooter.report_hits(&[a.hit_on(me, 1); 5]).unwrap();
    wait_for("the death", 20, || (a.owner.standings()[&me].state == STATE_DEAD).then_some(()));
    stamp("dead");
    assert_eq!(a.owner.standings()[&shooter_id].score, 1, "the shooter has the kill");
    wait_for("the game to know it is dead", 20, || {
        read(&log).contains("the server says the local player is dead: the unit is killed").then_some(())
    });
    std::thread::sleep(Duration::from_secs(2));
    wait_for("the respawn", 30, || (a.owner.standings()[&me].state == STATE_ALIVE).then_some(()));
    stamp("alive again");
    wait_for("the fresh fighter", 20, || (a.owner.fighters()[&me].hurt_count == 0).then_some(()));
    let fresh = a.owner.fighters()[&me].clone();
    assert_eq!((fresh.shield, fresh.body), (1.0, 1.0));
    wait_for("the game to hold the weapon again", 30, || {
        (read(&log).matches("the local unit holds the server's weapon").count() >= 2).then_some(())
    });
    std::thread::sleep(Duration::from_secs(4));
    stamp("done");
    assert_eq!(a.rejected_hits(), 0, "every report was a good one");
    a.finish(game);
}

#[test]
fn a_bystander_sees_a_player_hit_and_killed() {
    // the game's player stands behind and to the side of two fighters, and faces them
    let Some(a) = arena(
        "combat-bystander",
        &[at(80.0, -166.2, 0.0), at(84.0, -166.2, std::f32::consts::PI), at(88.0, -169.2, 2.678)],
    ) else {
        return;
    };
    let (shooter, shooter_id) = a.seat(1);
    let (_victim, victim_id) = a.seat(2);
    assert_eq!((shooter_id, victim_id), (0, 1));
    let (game, log) = a.start_game(120, &[]);
    let me = wait_for("the game's seat", 60, || {
        a.owner.seats().values().map(|s| s.player).find(|p| *p != shooter_id && *p != victim_id)
    });
    wait_for("the game to see both", 60, || {
        let text = read(&log);
        (text.contains("large mode: 2 remote units")).then_some(())
    });
    std::thread::sleep(Duration::from_secs(4));
    let began = Instant::now();
    let stamp = |what: &str| println!("[{:6.1} s] {what}", began.elapsed().as_secs_f32());

    stamp("two hits on the victim");
    shooter.report_hits(&[a.hit_on(victim_id, 1)]).unwrap();
    std::thread::sleep(Duration::from_millis(400));
    shooter.report_hits(&[a.hit_on(victim_id, 1)]).unwrap();
    wait_for("the hits", 20, || (a.owner.fighters()[&victim_id].hurt_count == 2).then_some(()));
    std::thread::sleep(Duration::from_secs(4));

    stamp("three more: five in all, which is what the pistol needs");
    shooter.report_hits(&[a.hit_on(victim_id, 1); 3]).unwrap();
    wait_for("the death", 20, || (a.owner.standings()[&victim_id].state == STATE_DEAD).then_some(()));
    stamp("the victim is dead");
    wait_for("the game to bury it", 20, || read(&log).contains("was killed: its unit dies").then_some(()));
    std::thread::sleep(Duration::from_secs(6));
    stamp("the body lies");
    wait_for("the respawn", 30, || (a.owner.standings()[&victim_id].state == STATE_ALIVE).then_some(()));
    std::thread::sleep(Duration::from_secs(4));
    stamp("alive again");
    let _ = me;
    assert_eq!(a.rejected_hits(), 0);
    a.finish(game);
}
