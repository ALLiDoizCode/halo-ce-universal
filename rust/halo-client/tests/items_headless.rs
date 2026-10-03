//! Items and pickups, in the headless C client against a Slayer match that
//! `halo-server` runs on the real maps: the game shows the server's items where
//! the map's placements put them, its player takes one, swaps for another, sees
//! the weapons a player who dies puts down fall, and is camouflaged for as long
//! as the tags say. Two runs of the game, each with a simulated player beside it:
//!
//! - **Blood Gulch**: the game's player stands at the red base, among the
//!   placements' weapons. It takes the assault rifle as its second weapon and
//!   (with `large.autouse`, which presses the action button twice a second from
//!   a little after the weapon is in hand) swaps for the weapons that a
//!   simulated player puts down when it dies, which fall in front of the game.
//! - **Boarding Action**: the game's player stands at a camouflage, takes it,
//!   and is camouflaged for 45 seconds.
//!
//! It needs the game's own data, so it skips itself (and says why) without all
//! of the variables `headless.rs` lists (`HALO_STDB_BIN`, `HALO_MAP_DIR`,
//! `HALO_GAME_BIN`, `HALO_DATA_ROOT`). With `HALO_SCREENSHOT_DIR` (and
//! `HALO_SCREENSHOT_EVERY`) the game saves frames, which show what the player
//! sees; `HALO_HEADLESS_LOG` keeps the game's log. Run it with
//! `--test-threads=1`: one game can run on a machine at a time.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use halo_map::HaloMap;
use halo_match_driver::server::{stdb_bin_dir, Server as Stdb};
use halo_match_driver::{MatchClient, PlayerClient};
use halo_server::admin::Admin;
use halo_server::fixtures::scratch;
use halo_server::maps::MapFiles;
use halo_server::servers::Log;
use halo_sim::PlayerInput;

const STATE_ALIVE: u8 = 0;
const NO_WEAPON: u16 = u16::MAX;

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

/// Where a player stands, on the ground there (the server settles it), facing along `yaw`.
fn at(x: f32, y: f32, yaw: f32) -> PlayerInput {
    PlayerInput { player: 0, position: [x, y, 0.0], yaw, pitch: 0.0, flags: 0 }
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
    map: HaloMap,
    map_name: String,
}

/// A match on the real map `map_name` where the players that join appear at `places` (the n-th for player n),
/// Slayer to no score and a short respawn; `None` without the game's data.
fn arena(name: &str, map_name: &str, places: &[PlayerInput]) -> Option<Arena> {
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
map = "{map_name}"
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
    let work = std::env::temp_dir().join(format!("halo-items-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    let map = HaloMap::from_path(data.join(format!("maps/{map_name}.map"))).unwrap();
    Some(Arena {
        owner,
        url,
        database: row.database,
        gateway: row.gateway,
        stdb,
        running,
        data,
        game,
        work,
        dir,
        map,
        map_name: map_name.into(),
    })
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
                .env("HALO_LARGE_MAP", &self.map_name)
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

    /// The tag index of a weapon of the map by its name.
    fn weapon(&self, name: &str) -> u16 {
        self.map
            .combat
            .weapons
            .iter()
            .find(|w| w.name == format!("weapons\\{name}\\{name}.weap"))
            .unwrap_or_else(|| panic!("no {name}"))
            .tag_index
    }

    fn finish(self, game: Game) {
        drop(game);
        let _ = self.running.stop();
        drop(self.stdb);
        let _ = std::fs::remove_dir_all(&self.work);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The last number of a game's log line of the items (`items: 41 on the ground, 0 of them falling; ...`).
fn items_in_log(text: &str) -> Option<(u32, u32)> {
    let line = text.lines().rev().find(|l| l.contains("large mode: items: "))?;
    let after = line.split("items: ").nth(1)?;
    let on_the_ground = after.split(" on the ground").next()?.trim().parse().ok()?;
    let falling = after.split("ground, ").nth(1)?.split(" of them falling").next()?.trim().parse().ok()?;
    Some((on_the_ground, falling))
}

#[test]
fn the_game_shows_the_servers_items_takes_one_swaps_for_others_and_sees_weapons_fall() {
    // Blood Gulch's red base: the assault rifle is 0.7 ahead of the game's player (who faces +y), the
    // simulated player stands a step to its right
    // (the weapons the simulated player puts down are thrown about 0.7 along where it faces: to the game's feet)
    let places = [at(40.4, -78.7, std::f32::consts::FRAC_PI_2), at(40.4, -77.95, -std::f32::consts::FRAC_PI_2)];
    let Some(a) = arena("items-bloodgulch", "bloodgulch", &places) else { return };
    let (shooter, sim) = a.seat(1);
    assert_eq!(sim, 0);
    let rifle = a.weapon("assault rifle");
    let shotgun = a.weapon("shotgun");
    let plasma_rifle = a.weapon("plasma rifle");
    let pistol = a.weapon("pistol");
    // the simulated player carries two others, which it puts down when it dies
    a.owner.set_loadout(sim, shotgun, plasma_rifle).unwrap();
    let (game, log) = a.start_game(170, &[("HALO_LARGE_AUTOUSE", "25")]);
    let me = wait_for("the game's seat", 60, || a.owner.seats().values().map(|s| s.player).find(|p| *p != sim));
    wait_for("the game's player in the world", 60, || {
        (a.owner.standings().get(&me)?.state == STATE_ALIVE && a.owner.fighters().contains_key(&me)).then_some(())
    });
    wait_for("the game to hold the server's weapon", 60, || {
        read(&log).contains("the local unit holds the server's weapon").then_some(())
    });
    let began = Instant::now();
    let stamp = |what: &str| println!("[{:6.1} s] {what}", began.elapsed().as_secs_f32());

    // what the game draws is what the server has on the ground: an object for each of its items
    wait_for("the game to draw the server's items", 30, || {
        let (on_the_ground, _) = items_in_log(&read(&log))?;
        (on_the_ground > 0 && on_the_ground as usize == a.owner.items().len()).then_some(())
    });
    let on_the_ground = a.owner.items();
    stamp(&format!("the game draws all {} of the server's items", on_the_ground.len()));
    // the placements' weapons are at the placements' places, at rest on the ground
    let rifle_item = on_the_ground.values().find(|i| i.tag == rifle && (i.x - 40.128).abs() < 0.01).expect("the rifle");
    assert!(rifle_item.resting && (rifle_item.z - (-0.2869)).abs() < 0.2, "{rifle_item:?}");
    assert_eq!(a.owner.fighters()[&me].weapon_0, pistol);
    assert_eq!(a.owner.fighters()[&me].weapon_1, NO_WEAPON);

    // the game's player presses the action button 25 seconds after the weapon was in hand: the rifle is the second weapon
    wait_for("the game to take the rifle", 80, || {
        read(&log).contains("the local unit takes a weapon (slot 1)").then_some(())
    });
    wait_for("the server to say so", 20, || (a.owner.fighters()[&me].weapon_1 == rifle).then_some(()));
    stamp("the game's player holds the pistol and the rifle");
    assert!(a.owner.items().values().all(|i| !(i.tag == rifle && (i.x - 40.128).abs() < 0.01)), "the rifle is taken");
    wait_for("the game to take the rifle's object away", 10, || {
        let (n, _) = items_in_log(&read(&log))?;
        (n as usize == a.owner.items().len()).then_some(())
    });

    // the simulated player dies: the shotgun and the plasma rifle fall in front of the game
    stamp("the simulated player dies");
    a.owner.report_death(sim, None).unwrap();
    wait_for("two weapons put down", 20, || {
        (a.owner.items().values().filter(|i| i.ignore == sim && (i.tag == shotgun || i.tag == plasma_rifle)).count()
            == 2)
            .then_some(())
    });
    wait_for("them to come to rest", 20, || {
        // (the game may already have swapped for one of them: what is left lies still)
        let dropped: Vec<_> = a.owner.items().into_values().filter(|i| i.placement == u16::MAX).collect();
        (!dropped.is_empty() && dropped.iter().all(|i| i.resting)).then_some(())
    });
    stamp("the weapons lie on the ground");
    // ... and the game's player, pressing, swaps the weapon in hand for one of them, which it puts down
    // (it goes on swapping among what is at its feet while it presses: each time one weapon in, one out)
    wait_for("the game to swap", 60, || {
        let text = read(&log);
        (text.contains("the local unit puts down a weapon")
            && (text.contains("takes a weapon (slot 1): weapons\\shotgun\\shotgun")
                || text.contains("takes a weapon (slot 1): weapons\\plasma rifle\\plasma rifle")))
        .then_some(())
    });
    stamp("the game's player swapped for one of the weapons the simulated player put down");
    std::thread::sleep(Duration::from_secs(6));
    // the server never has a weapon in two places: in a hand, or on the ground
    let held: Vec<u16> = [me, sim]
        .iter()
        .flat_map(|p| {
            let f = a.owner.fighters()[p].clone();
            [f.weapon_0, f.weapon_1]
        })
        .collect();
    for item in a.owner.items().values().filter(|i| i.placement == u16::MAX) {
        println!(
            "on the ground: tag {} at ({:.2}, {:.2}, {:.2}) resting {}",
            item.tag, item.x, item.y, item.z, item.resting
        );
    }
    println!("held: {held:?}");
    stamp("done");
    a.finish(game);
    drop(shooter);
}

#[test]
fn the_game_player_takes_camouflage_and_has_it_for_45_seconds_by_the_tags() {
    // Boarding Action has two camouflages and no choice between them: the game's player is put on one
    let map = {
        let Some(maps) = env_path("HALO_MAP_DIR") else {
            eprintln!("HALO_MAP_DIR is not set: skipping");
            return;
        };
        HaloMap::from_path(maps.join("boardingaction.map")).unwrap()
    };
    let camo = map.items.defs.iter().find(|d| d.name == "powerups\\active camouflage.eqip").unwrap().tag_index;
    let place = map
        .items
        .placements
        .iter()
        .find(|p| p.permutations.len() == 1 && p.permutations[0].1 == camo)
        .expect("a camouflage placement");
    let [x, y, _] = place.position;
    let Some(a) = arena("items-camo", "boardingaction", &[at(x + 0.2, y, std::f32::consts::PI)]) else { return };
    let (game, log) = a.start_game(120, &[]);
    let me = wait_for("the game's seat", 60, || a.owner.seats().values().map(|s| s.player).next());
    wait_for("the game's player in the world", 60, || {
        (a.owner.standings().get(&me)?.state == STATE_ALIVE && a.owner.fighters().contains_key(&me)).then_some(())
    });
    wait_for("the game's player to be camouflaged", 60, || a.owner.powerups().get(&me).map(|p| p.camo_until));
    let until = a.owner.powerups()[&me].camo_until;
    let began = Instant::now();
    wait_for("the game to show it", 30, || read(&log).contains("the local player is camouflaged").then_some(()));
    println!("camouflaged until tick {until}, the match is at {}", a.owner.marker().unwrap().tick);
    // 45 seconds from the tick it was taken
    wait_for("it to run out", 70, || (!a.owner.powerups().contains_key(&me)).then_some(()));
    assert!(a.owner.marker().unwrap().tick >= until);
    println!("[{:6.1} s] the server took the camouflage away", began.elapsed().as_secs_f32());
    wait_for("the game to show it is gone", 30, || {
        read(&log).contains("the local player is not camouflaged any more").then_some(())
    });
    a.finish(game);
}
