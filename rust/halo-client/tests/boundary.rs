//! The client library as the C client uses it: through its `extern "C"`
//! functions, against a real local SpacetimeDB and a real gateway, with
//! simulated UDP players walking beside it. Asserts on what the library hands
//! across the boundary, compared with what the server held.
//!
//! Like the driver's and the gateway's tests these start their own Standalone
//! and need `HALO_STDB_BIN` (a SpacetimeDB 2.10.x release directory); without
//! it they print a note and pass without testing. They run on a flat floor,
//! which needs no game data. One at a time: the library holds one session.

use std::ffi::{c_char, CString};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use halo_client::ffi::*;
use halo_gateway::harness::{Crowd, Impairment, Truth};
use halo_gateway::{Gateway, GatewayConfig, UdpTransport};
use halo_match_driver::server::{build_module, stdb_bin_dir, Server};
use halo_match_driver::walkers::Walkers;
use halo_match_driver::MatchClient;
use halo_sim::fixtures::flat_floor_map;
use halo_sim::PlayerInput;
use halo_wire::unit::Bounds;

fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wasm() -> &'static PathBuf {
    static WASM: OnceLock<PathBuf> = OnceLock::new();
    WASM.get_or_init(build_module)
}

const WAIT: Duration = Duration::from_secs(20);

/// A running match with a gateway in front of it, `others` simulated players
/// (ids `0..others`) and one more player, `ME`, for the library to be.
struct Rig {
    gateway: Gateway,
    client: MatchClient,
    walkers: Walkers,
    crowd: Crowd,
    uri: String,
    name: String,
    _server: Server,
}

const OTHERS: u16 = 40;
const ME: u16 = OTHERS;

fn rig(name: &str) -> Option<Rig> {
    let Some(bin) = stdb_bin_dir() else {
        eprintln!("HALO_STDB_BIN is not set: skipping, this test needs a SpacetimeDB 2.10.x release");
        return None;
    };
    let server = Server::start(&bin);
    server.publish(wasm(), name);
    let client = MatchClient::connect(&server.uri(), name);
    let map = flat_floor_map();
    client.load_map(map.to_bytes()).unwrap();
    // players on a grid over a square; the library's player is the last
    let anchors: Vec<[f32; 3]> =
        (0..=OTHERS).map(|i| [-20.0 + (i % 7) as f32 * 6.0, -20.0 + (i / 7) as f32 * 6.0, 0.0]).collect();
    let (walkers, spawn) = Walkers::new(map, &anchors, OTHERS + 1, 7);
    client.add_players(&spawn).unwrap();
    client.start();
    let config = GatewayConfig::new(server.uri(), name);
    let transport = Arc::new(UdpTransport::bind("127.0.0.1:0".parse().unwrap()).unwrap());
    let gateway = Gateway::start(config, transport).expect("start the gateway");
    let crowd = Crowd::connect(gateway.local_addr(), 0..OTHERS, Impairment::none(), OTHERS as usize + 1, false);
    crowd.join_all(WAIT).unwrap();
    Some(Rig { gateway, client, walkers, crowd, uri: server.uri(), name: name.to_string(), _server: server })
}

/// Start the library as the C client does.
fn start(rig: &Rig, player: u16) -> bool {
    let gateway = CString::new(rig.gateway.local_addr().to_string()).unwrap();
    let uri = CString::new(rig.uri.clone()).unwrap();
    let database = CString::new(rig.name.clone()).unwrap();
    unsafe {
        halo_large_start(
            gateway.as_ptr() as *const c_char,
            uri.as_ptr() as *const c_char,
            database.as_ptr() as *const c_char,
            player as u32,
        ) == 1
    }
}

/// `halo_large_status`: (joined, slow state connected, map version).
fn status() -> (u32, u32, u32) {
    let mut out = [0u32; 8];
    let joined = unsafe { halo_large_status(out.as_mut_ptr()) };
    (joined, out[0], out[1])
}

/// What `halo_large_frame` and `halo_large_unit` hold now: the newest datagram
/// tick, and each unit as (player, datagram tick, [x y z vx vy vz yaw pitch]).
fn frame() -> (u32, Vec<(u32, u32, [f32; 8])>) {
    let mut tick = 0u32;
    let count = unsafe { halo_large_frame(&mut tick) };
    let units = (0..count)
        .map(|i| {
            let (mut player, mut at) = (0u32, 0u32);
            let mut out = [0f32; 8];
            assert_eq!(unsafe { halo_large_unit(i, &mut player, &mut at, out.as_mut_ptr()) }, 1);
            (player, at, out)
        })
        .collect();
    (tick, units)
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_players_the_library_is_sent_are_the_servers_to_within_the_packing() {
    let _serial = serial();
    let Some(mut rig) = rig("boundary-content") else { return };
    assert!(start(&rig, ME));
    wait_for("the library never joined", || status().0 == 1);

    // walk the others, sampling what the library holds as the ticks go by
    let mut truth = Truth::new(OTHERS as usize + 1);
    let mut seen = Vec::new();
    let until = Instant::now() + Duration::from_secs(4);
    while Instant::now() < until {
        let tick = rig.client.next_tick(WAIT).expect("a tick");
        truth.record(&tick);
        rig.walkers.sync_with_server(tick.players.values());
        rig.crowd.send_inputs(&rig.walkers.next_inputs());
        seen.push(frame());
    }
    // the datagrams of the last ticks may still be on their way to the truth
    std::thread::sleep(Duration::from_millis(100));
    let (last_tick, _) = frame();
    assert!(last_tick > 60, "the library was sent {last_tick} ticks");

    let bounds = Bounds::from_world(flat_floor_map().world_bounds);
    let mut checked = 0;
    let mut distinct = std::collections::BTreeSet::new();
    for (_, units) in &seen {
        for (player, tick, state) in units {
            let Some(at) = truth.ticks.get(tick) else { continue };
            let want = at.positions[*player as usize].expect("a state of a player the server has");
            assert_ne!(*player, ME as u32, "the library was sent its own player");
            for axis in 0..3 {
                let step = (bounds.max[axis] - bounds.min[axis]) / 65535.0;
                assert!((state[axis] - want[axis]).abs() <= step, "tick {tick}, player {player}, axis {axis}");
            }
            distinct.insert(*player);
            checked += 1;
        }
    }
    assert!(checked > 1000, "only {checked} states checked");
    assert_eq!(distinct.len(), OTHERS as usize, "every other player was heard of");
}

#[test]
fn the_slow_state_comes_over_the_direct_connection() {
    let _serial = serial();
    let Some(rig) = rig("boundary-slow") else { return };
    assert!(start(&rig, ME));
    wait_for("the slow state never connected", || status().1 == 1);

    // the library's own player as the server has it, and the map's bounds
    let mut own = [0f32; 5];
    wait_for("no local state", || unsafe { halo_large_local(own.as_mut_ptr()) } == 1);
    let row = rig.client.players()[&ME].clone();
    assert_eq!(own, [row.x, row.y, row.z, row.yaw, row.pitch]);
    let (_, _, map_version) = status();
    assert!(map_version >= 1);
    let mut bounds = [0f32; 6];
    wait_for("no bounds", || unsafe { halo_large_bounds(bounds.as_mut_ptr()) } == 1);
    assert_eq!(bounds, flat_floor_map().world_bounds);
}

#[test]
fn the_librarys_input_moves_its_player() {
    let _serial = serial();
    let Some(rig) = rig("boundary-input") else { return };
    assert!(start(&rig, ME));
    wait_for("the library never joined", || status().0 == 1);
    let start_row = rig.client.players()[&ME].clone();
    let moved =
        PlayerInput { player: ME, position: [start_row.x + 0.05, start_row.y, start_row.z], yaw: 1.25, pitch: -0.25 };
    rig.client.discard_ticks();
    let from = rig.client.next_tick(WAIT).unwrap().marker.tick;
    let until = Instant::now() + WAIT;
    loop {
        halo_large_send_input(moved.position[0], moved.position[1], moved.position[2], 1.25, -0.25);
        let seen = rig.client.next_tick(WAIT).unwrap();
        let row = &seen.players[&ME];
        if row.yaw == 1.25 {
            assert_eq!((row.x, row.y, row.z), (moved.position[0], moved.position[1], moved.position[2]));
            assert_eq!(row.rejected_moves, 0);
            break;
        }
        assert!(Instant::now() < until && seen.marker.tick < from + 300, "the input never applied");
    }
}

#[test]
fn a_session_can_be_stopped_and_started_again_and_a_bad_start_is_refused() {
    let _serial = serial();
    let Some(rig) = rig("boundary-restart") else { return };
    assert!(start(&rig, ME));
    wait_for("the library never joined", || status().0 == 1);
    halo_large_stop();
    assert_eq!(status().0, 0, "stopped");
    assert_eq!(frame().1.len(), 0);

    assert!(start(&rig, ME));
    wait_for("the library never joined again", || status().0 == 1);
    halo_large_stop();

    // a gateway address that is no address
    let bad = CString::new("not an address").unwrap();
    let uri = CString::new(rig.uri.clone()).unwrap();
    let database = CString::new(rig.name.clone()).unwrap();
    assert_eq!(unsafe { halo_large_start(bad.as_ptr(), uri.as_ptr(), database.as_ptr(), 1) }, 0);
    let mut message = [0 as c_char; 128];
    let length = unsafe { halo_large_error(message.as_mut_ptr(), message.len() as u32) };
    assert!(length > 0, "a reason for the refusal");
    // stopping what never started, and a null pointer, are harmless
    halo_large_stop();
    assert_eq!(unsafe { halo_large_start(std::ptr::null(), uri.as_ptr(), database.as_ptr(), 1) }, 0);
}
