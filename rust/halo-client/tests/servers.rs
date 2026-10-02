//! The client library's server list, identity and refusals, through the C
//! functions the game calls, against a whole server (`halo-server`: the root
//! database, the rotation, the gateways) on a real local SpacetimeDB. Asserts
//! on what the player is shown and what the operator's tools then see.
//!
//! Needs `HALO_STDB_BIN` (a SpacetimeDB 2.10.x release directory) and skips
//! itself without it; no game data (the maps are a flat floor). One test at a
//! time: the library holds one session and one server list.

use std::ffi::{c_char, CString};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use halo_client::ffi::*;
use halo_match_driver::server::{stdb_bin_dir, Server as Stdb};
use halo_match_driver::{MatchClient, PlayerClient};
use halo_server::fixtures::{scratch, test_config, FlatFloors};
use halo_server::servers::Log;
use halo_server::Running;
use spacetimedb_sdk::Identity;

const WAIT: Duration = Duration::from_secs(30);
const KEY: [u8; 32] = [3; 32];

fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wait_for<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(found) = check() {
            return found;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(30));
    }
}

/// A server on a SpacetimeDB of its own, and the folder the player's
/// identities are kept in.
struct World {
    stdb: Stdb,
    running: Option<Running>,
    identities: PathBuf,
    dir: PathBuf,
}

impl World {
    fn start(name: &str, servers: &[(&str, &str)]) -> Option<World> {
        let Some(bin) = stdb_bin_dir() else {
            eprintln!("HALO_STDB_BIN is not set: skipping, this test needs a SpacetimeDB 2.10.x release");
            return None;
        };
        let dir = scratch(name);
        let stdb = Stdb::start(&bin);
        std::fs::write(dir.join("owner.token"), &stdb.owner().token).unwrap();
        let config = test_config(&dir, &stdb.uri(), None, servers);
        let running = halo_server::start(config, Arc::new(FlatFloors), Log::new()).expect("the server starts");
        wait_for("every server to be listed", || {
            (running.root().servers().iter().filter(|s| !s.database.is_empty()).count() == servers.len()).then_some(())
        });
        let identities = dir.join("identities");
        let world = World { stdb, running: Some(running), identities, dir };
        let folder = CString::new(world.identities.to_str().unwrap()).unwrap();
        unsafe { halo_large_identity_dir(folder.as_ptr() as *const c_char) };
        Some(world)
    }

    fn uri(&self) -> String {
        self.stdb.uri()
    }

    fn root(&self) -> &halo_server::root::Root {
        self.running.as_ref().unwrap().root()
    }
}

impl Drop for World {
    fn drop(&mut self) {
        halo_large_stop();
        halo_large_browse_stop();
        if let Some(running) = self.running.take() {
            let _ = running.stop();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn cstring(text: &str) -> CString {
    CString::new(text).unwrap()
}

fn browse(world: &World) {
    let (uri, root) = (cstring(&world.uri()), cstring("halo-root"));
    assert_eq!(unsafe { halo_large_browse_start(uri.as_ptr() as *const c_char, root.as_ptr() as *const c_char) }, 1);
}

/// Wait for the list and freeze it; the number of servers in it.
fn list(count: u32) -> u32 {
    wait_for("the server list", || {
        let mut out = [0u32; 2];
        let current = unsafe { halo_large_browse_status(out.as_mut_ptr()) };
        (current == 1 && out[1] == count).then_some(())
    });
    halo_large_browse_list()
}

fn text(index: u32, field: u32) -> String {
    let mut buffer = [0 as c_char; 256];
    let length = unsafe { halo_large_browse_text(index, field, buffer.as_mut_ptr(), buffer.len() as u32) };
    buffer[..length as usize].iter().map(|c| *c as u8 as char).collect()
}

/// `[players, capacity, match number, time limit, joinable]`
fn numbers(index: u32) -> [u32; 5] {
    let mut out = [0u32; 5];
    assert_eq!(unsafe { halo_large_browse_entry(index, out.as_mut_ptr()) }, 1);
    out
}

fn find(id: &str) -> u32 {
    let at = unsafe { halo_large_browse_find(cstring(id).as_ptr() as *const c_char) };
    assert!(at > 0, "no server {id:?} in the list");
    at - 1
}

fn identity() -> String {
    let mut buffer = [0 as c_char; 128];
    let length = unsafe { halo_large_identity(buffer.as_mut_ptr(), buffer.len() as u32) };
    buffer[..length as usize].iter().map(|c| *c as u8 as char).collect()
}

fn refusal() -> (u32, String) {
    let mut buffer = [0 as c_char; 256];
    let kind = unsafe { halo_large_refusal(buffer.as_mut_ptr(), buffer.len() as u32) };
    let text = buffer.iter().take_while(|c| **c != 0).map(|c| *c as u8 as char).collect();
    (kind, text)
}

/// Join the server at `index` of the frozen list, as the client does.
fn join(world: &World, index: u32) {
    let gateway = cstring(&text(index, 6));
    let database = cstring(&text(index, 5));
    let uri = cstring(&world.uri());
    assert_eq!(
        unsafe {
            halo_large_start(
                gateway.as_ptr() as *const c_char,
                uri.as_ptr() as *const c_char,
                database.as_ptr() as *const c_char,
            )
        },
        1
    );
}

/// `(joined, player)` of the session.
fn status() -> (u32, u32) {
    let mut out = [0u32; 8];
    let joined = unsafe { halo_large_status(out.as_mut_ptr()) };
    (joined, out[6])
}

const LOUNGE: &str = "[[server.rotation]]\nmap = \"alpha\"\ngame_type = \"slayer\"\ncapacity = 5\nseconds = 0\n";
const ARENA: &str =
    "[[server.rotation]]\nmap = \"beta\"\ngame_type = \"team_slayer\"\nvariant = \"small\"\ncapacity = 7\nseconds = 0\n";

#[test]
fn a_player_sees_the_servers_with_their_map_game_type_and_players_and_joins_one_from_the_list() {
    let _serial = serial();
    let Some(world) = World::start("list", &[("lounge", LOUNGE), ("arena", ARENA)]) else { return };
    browse(&world);
    assert_eq!(list(2), 2);

    // the list, as the player reads it: sorted, with what each is playing
    let (arena, lounge) = (find("arena"), find("lounge"));
    assert_eq!(
        (text(arena, 1), text(arena, 2), text(arena, 3), text(arena, 4)),
        ("Server arena".into(), "beta".into(), "team_slayer".into(), "small".into())
    );
    assert_eq!(
        (text(lounge, 1), text(lounge, 2), text(lounge, 3)),
        ("Server lounge".into(), "alpha".into(), "slayer".into())
    );
    assert_eq!(numbers(arena), [0, 7, 1, 0, 1], "no players yet, room for 7, the first match, no time limit, joinable");
    assert_eq!(numbers(lounge)[1], 5);
    assert!(text(arena, 5).starts_with("hm-arena-"), "the match's database: {}", text(arena, 5));
    assert!(text(arena, 6).starts_with("127.0.0.1:"), "the gateway: {}", text(arena, 6));
    assert_ne!(text(arena, 6), text(lounge, 6), "each server has its own gateway");

    // joining one from the list takes a seat and the gateway welcomes the player
    join(&world, find("lounge"));
    wait_for("the gateway's welcome", || (status().0 == 1).then_some(()));
    assert_eq!(status().1, 0, "the first seat of the match");
    // and the list, which every player reads, counts them
    wait_for("the player in the list", || {
        halo_large_browse_list();
        (numbers(find("lounge"))[0] == 1).then_some(())
    });
    assert_eq!(numbers(find("arena"))[0], 0, "the other server is as empty as it was");
}

#[test]
fn the_player_is_the_same_identity_on_every_server_and_in_every_session_because_the_client_keeps_its_token() {
    let _serial = serial();
    let Some(world) = World::start("identity", &[("lounge", LOUNGE), ("arena", ARENA)]) else { return };
    browse(&world);
    list(2);
    let me = wait_for("the identity", || Some(identity()).filter(|i| !i.is_empty()));
    assert_eq!(me.len(), 64, "a hex identity: {me}");

    // on one server the match's seat is this identity's
    join(&world, find("lounge"));
    wait_for("the welcome", || (status().0 == 1).then_some(()));
    assert_eq!(identity(), me, "the session is the identity the list was read as");
    let lounge_database = text(find("lounge"), 5);
    let watcher = MatchClient::connect_as(&world.uri(), &lounge_database, Some(&world.stdb.owner().token));
    let seat_owners: Vec<String> = watcher.seats().values().map(|s| s.owner.to_hex().to_string()).collect();
    assert_eq!(seat_owners, std::slice::from_ref(&me), "the seat in the match is the player's identity");
    // the root database knows the same identity
    wait_for("the identity at the root", || {
        world.root().known_identities().contains(&Identity::from_hex(&me).unwrap()).then_some(())
    });

    // on another server: the same
    halo_large_stop();
    join(&world, find("arena"));
    wait_for("the welcome on the other server", || (status().0 == 1).then_some(()));
    assert_eq!(identity(), me, "the same identity on the next server");

    // in another session: the token is in the identities folder, and a new library session reads it
    halo_large_stop();
    halo_large_browse_stop();
    let files: Vec<_> = std::fs::read_dir(&world.identities).unwrap().collect();
    assert_eq!(files.len(), 1, "one SpacetimeDB, one token file: {files:?}");
    browse(&world);
    list(2);
    wait_for("the identity again", || Some(identity()).filter(|i| !i.is_empty()));
    assert_eq!(identity(), me, "the same identity after the list is started again");
    join(&world, find("lounge"));
    wait_for("the welcome", || (status().0 == 1).then_some(()));
    assert_eq!(identity(), me, "and in a new session");

    // without a folder to keep it in, every session is somebody new
    halo_large_stop();
    halo_large_browse_stop();
    unsafe { halo_large_identity_dir(std::ptr::null()) };
    browse(&world);
    list(2);
    let stranger = wait_for("a new identity", || Some(identity()).filter(|i| !i.is_empty()));
    assert_ne!(stranger, me);
}

#[test]
fn the_name_the_player_chose_follows_their_identity_to_every_roster() {
    let _serial = serial();
    let Some(world) = World::start("name", &[("lounge", LOUNGE), ("arena", ARENA)]) else { return };
    let name = cstring("Alice <3 Halo");
    unsafe { halo_large_set_name(name.as_ptr() as *const c_char) };
    browse(&world);
    list(2);
    let me = Identity::from_hex(wait_for("the identity", || Some(identity()).filter(|i| !i.is_empty()))).unwrap();
    // the root database knows the identity under a name the game can show
    wait_for("the name at the root", || (world.root().names() == [(me, "Alice 3 Hal".to_string())]).then_some(()));

    // on one server and then the other, the roster shows it
    for server in ["lounge", "arena"] {
        halo_large_stop();
        join(&world, find(server));
        wait_for("the welcome", || (status().0 == 1).then_some(()));
        let watcher = MatchClient::connect_as(&world.uri(), &text(find(server), 5), Some(&world.stdb.owner().token));
        wait_for("the name on the roster", || {
            (watcher.roster().values().map(|r| r.name.clone()).collect::<Vec<_>>() == ["Alice 3 Hal"]).then_some(())
        });
    }
}

#[test]
fn a_banned_player_is_told_why_while_playing_and_when_they_look_at_the_list_again() {
    let _serial = serial();
    let Some(world) = World::start("ban", &[("lounge", LOUNGE)]) else { return };
    browse(&world);
    list(1);
    join(&world, find("lounge"));
    wait_for("the welcome", || (status().0 == 1).then_some(()));
    assert_eq!(refusal().0, 0, "nothing is refused");
    let me = identity();

    // the operator bans the identity
    world.root().ban(Identity::from_hex(&me).unwrap(), "team killing").unwrap();

    // the running match takes the player out, and the client says why
    let (kind, message) = wait_for("the refusal", || Some(refusal()).filter(|r| r.0 != 0));
    assert_eq!(kind, 1, "banned: {message}");
    assert!(message.contains("banned") && message.contains("team killing"), "told: {message}");
    assert_eq!(status(), (0, u32::MAX), "no seat and no gateway session");

    // the server list's own database refuses them too, when they come back to it
    halo_large_browse_stop();
    browse(&world);
    wait_for("the list's refusal", || {
        let mut out = [0u32; 2];
        unsafe { halo_large_browse_status(out.as_mut_ptr()) };
        (out[0] == 1).then_some(())
    });
    let mut buffer = [0 as c_char; 256];
    let length = unsafe { halo_large_browse_message(buffer.as_mut_ptr(), buffer.len() as u32) };
    let told: String = buffer[..length as usize].iter().map(|c| *c as u8 as char).collect();
    assert!(told.contains("banned") && told.contains("team killing"), "told: {told}");

    // lifting the ban lets them join again
    world.root().unban(Identity::from_hex(&me).unwrap()).unwrap();
    halo_large_stop();
    halo_large_browse_stop();
    browse(&world);
    list(1);
    // (the lifted ban reaches the match through the orchestration a moment after the root; a join
    // that came before it is refused, and a banned player's client does not ask again by itself)
    let deadline = Instant::now() + WAIT;
    loop {
        join(&world, find("lounge"));
        let tried = Instant::now();
        while status().0 != 1 && refusal().0 == 0 && tried.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(30));
        }
        if status().0 == 1 {
            break;
        }
        assert!(Instant::now() < deadline, "never welcomed after the ban was lifted; refusal {:?}", refusal());
        std::thread::sleep(Duration::from_millis(200));
    }
    assert_eq!(refusal().0, 0);
}

#[test]
fn a_kept_token_the_server_does_not_accept_is_replaced_by_a_new_identity() {
    let _serial = serial();
    let Some(world) = World::start("badtoken", &[("lounge", LOUNGE)]) else { return };
    // a token from another instance, or from before the server's keys were made again
    let file = halo_client::IdentityFile::new(Some(&world.identities), &world.uri());
    file.save("not.a.token").unwrap();

    browse(&world);
    list(1);
    let me = wait_for("a new identity", || Some(identity()).filter(|i| !i.is_empty()));
    assert_eq!(me.len(), 64);
    let kept = file.load().expect("the new token is kept");
    assert_ne!(kept, "not.a.token", "the token the server refused is gone");
    join(&world, find("lounge"));
    wait_for("the welcome", || (status().0 == 1).then_some(()));
    assert_eq!(identity(), me);
}

#[test]
fn a_full_server_says_it_is_full_and_the_player_gets_in_when_a_seat_is_free() {
    let _serial = serial();
    let one = "[[server.rotation]]\nmap = \"alpha\"\ncapacity = 1\nseconds = 0\n";
    let Some(world) = World::start("full", &[("tiny", one)]) else { return };
    browse(&world);
    list(1);
    let index = find("tiny");
    let database = text(index, 5);

    // another player has the only seat
    let other = world.stdb.new_account();
    let squatter = PlayerClient::connect(&world.uri(), &database, &other.token);
    squatter.join(KEY).unwrap();

    join(&world, index);
    let (kind, message) = wait_for("the refusal", || Some(refusal()).filter(|r| r.0 != 0));
    assert_eq!(kind, 2, "full: {message}");
    assert!(message.contains("full"), "told: {message}");
    assert_eq!(status(), (0, u32::MAX));

    // the seat is freed; the client asks again by itself
    squatter.leave().unwrap();
    wait_for("the welcome once there is room", || (status().0 == 1).then_some(()));
    assert_eq!(refusal().0, 0, "nothing is refused now");
}
