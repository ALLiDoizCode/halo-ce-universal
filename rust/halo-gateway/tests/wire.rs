//! The player's wire: simulated UDP players against a real local SpacetimeDB
//! and a real gateway, asserting on what a player sends and receives.
//!
//! Like the driver's tests these start their own Standalone and need
//! `HALO_STDB_BIN` (a SpacetimeDB 2.10.x release directory); without it they
//! print a note and pass without testing. They run on a flat floor, which
//! needs no game data; the 500-player test on Blood Gulch also needs
//! `HALO_MAP_DIR` and skips without it.
//!
//! They run one at a time: a gateway, a database and hundreds of player
//! threads share the machine, and timing assertions would otherwise measure
//! the tests' crowding of each other.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use halo_gateway::harness::{analyze, run_walk, Crowd, Impairment, Truth, DEFAULT_BANDS};
use halo_gateway::{Gateway, GatewayConfig, UdpTransport};
use halo_match_driver::server::{build_module, stdb_bin_dir, Server};
use halo_match_driver::walkers::Walkers;
use halo_match_driver::MatchClient;
use halo_sim::fixtures::flat_floor_map;
use halo_sim::{MapData, PlayerInput};
use halo_wire::datagram::ClientMessage;
use halo_wire::unit::Bounds;

fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wasm() -> &'static PathBuf {
    static WASM: OnceLock<PathBuf> = OnceLock::new();
    WASM.get_or_init(build_module)
}

/// A running match with a gateway in front of it. Fields drop in order: the
/// gateway and the subscriber go before the server.
struct Rig {
    gateway: Gateway,
    client: MatchClient,
    walkers: Walkers,
    capacity: usize,
    _server: Server,
}

/// Start everything on `map`, with `players` walkers placed around `anchors`.
/// `None` (skip the test) without a server.
fn rig(name: &str, map: MapData, anchors: &[[f32; 3]], players: u16, budget: u32) -> Option<Rig> {
    let Some(bin) = stdb_bin_dir() else {
        eprintln!("HALO_STDB_BIN is not set: skipping, this test needs a SpacetimeDB 2.10.x release");
        return None;
    };
    let server = Server::start(&bin);
    server.publish(wasm(), name);
    let client = MatchClient::connect(&server.uri(), name);
    client.load_map(map.to_bytes()).unwrap();
    let (walkers, spawn) = Walkers::new(map, anchors, players, 7);
    client.add_players(&spawn).unwrap();
    client.start();
    let mut config = GatewayConfig::new(server.uri(), name);
    config.budget_bytes_per_second = budget;
    let transport = Arc::new(UdpTransport::bind("127.0.0.1:0".parse().unwrap()).unwrap());
    let gateway = Gateway::start(config, transport).expect("start the gateway");
    Some(Rig { gateway, client, walkers, capacity: players as usize, _server: server })
}

/// `n` points on a grid over a square of half-side `half`, on the floor.
fn grid(n: usize, half: f32) -> Vec<[f32; 3]> {
    let side = (n as f32).sqrt().ceil() as usize;
    let step = 2.0 * half / side as f32;
    (0..n).map(|i| [-half + (i % side) as f32 * step, -half + (i / side) as f32 * step, 0.0]).collect()
}

fn real_map(name: &str) -> Option<halo_map::HaloMap> {
    let Some(dir) = std::env::var_os("HALO_MAP_DIR") else {
        eprintln!("HALO_MAP_DIR is not set: skipping, this test needs the game's own map files");
        return None;
    };
    let path = PathBuf::from(dir).join(format!("{name}.map"));
    Some(halo_map::HaloMap::from_path(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
}

const WAIT: Duration = Duration::from_secs(20);

#[test]
fn a_player_is_welcomed_with_the_maps_bounds_only_if_the_match_has_them() {
    let _serial = serial();
    let Some(rig) = rig("welcome", flat_floor_map(), &grid(2, 5.0), 2, 90_000) else { return };
    let crowd = Crowd::connect(rig.gateway.local_addr(), 0..2, Impairment::none(), rig.capacity, false);
    crowd.join_all(WAIT).unwrap();
    let welcome = crowd.player(1).unwrap().welcome().unwrap();
    assert_eq!(welcome.player, 1);
    assert_eq!(welcome.bounds, Bounds::from_world(flat_floor_map().world_bounds));
    assert!(welcome.tick as u64 <= rig.client.marker().unwrap().tick, "a tick the match has not reached");

    // a player the match does not have gets no answer
    let stranger = Crowd::connect(rig.gateway.local_addr(), 40..41, Impairment::none(), 64, false);
    stranger.player(40).unwrap().hello();
    std::thread::sleep(Duration::from_millis(500));
    assert!(stranger.player(40).unwrap().welcome().is_none());
    assert_eq!(rig.gateway.sessions(), 2);
}

#[test]
fn an_input_counts_only_from_the_address_its_player_is_bound_to() {
    let _serial = serial();
    let Some(rig) = rig("unbound", flat_floor_map(), &grid(2, 5.0), 2, 90_000) else { return };
    let crowd = Crowd::connect(rig.gateway.local_addr(), 0..2, Impairment::none(), rig.capacity, false);
    crowd.join_all(WAIT).unwrap();
    let start = rig.client.players()[&0].clone();

    // player 1's socket claims to move player 0, and a socket that never said hello does too
    let moved = |x: f32| PlayerInput { player: 0, position: [start.x + x, start.y, start.z], yaw: 1.5, pitch: 0.0 };
    crowd.player(1).unwrap().send_input(&moved(0.05));
    let stranger = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    stranger.send_to(&ClientMessage::Input { seq: 1, input: moved(0.05) }.encode(), rig.gateway.local_addr()).unwrap();

    rig.client.discard_ticks();
    let tick = rig.client.next_tick(WAIT).unwrap().marker.tick;
    let later = rig.client.wait_for_tick(tick + 5, WAIT);
    assert_eq!(later.players[&0].yaw, start.yaw, "player 0 was not moved by anyone but their own address");
    assert_eq!(rig.gateway.stats().inputs_unbound, 2);

    // the right address works
    crowd.player(0).unwrap().send_input(&moved(0.05));
    let tick = later.marker.tick;
    let seen = loop {
        let seen = rig.client.next_tick(WAIT).unwrap();
        if seen.players[&0].yaw == 1.5 {
            break seen;
        }
        assert!(seen.marker.tick < tick + 10, "the input never applied");
    };
    assert_eq!(seen.players[&0].x, start.x + 0.05);
}

#[test]
fn the_newest_input_wins_and_a_late_one_is_dropped() {
    let _serial = serial();
    let Some(rig) = rig("late", flat_floor_map(), &grid(1, 0.0), 1, 90_000) else { return };
    let crowd = Crowd::connect(rig.gateway.local_addr(), 0..1, Impairment::none(), rig.capacity, false);
    crowd.join_all(WAIT).unwrap();
    let me = crowd.player(0).unwrap();
    let start = rig.client.players()[&0].clone();
    let at = |x: f32| PlayerInput { player: 0, position: [start.x + x, start.y, start.z], yaw: 0.0, pitch: 0.0 };

    // right after a tick completes, so that both arrive before the next batch
    rig.client.discard_ticks();
    let seen = rig.client.next_tick(WAIT).unwrap();
    me.send_input_with_seq(10, &at(0.05));
    me.send_input_with_seq(9, &at(0.10));
    me.send_input_with_seq(10, &at(0.12));
    let after = rig.client.wait_for_tick(seen.marker.tick + 4, WAIT);
    assert_eq!(after.players[&0].x, start.x + 0.05, "seq 10 was newest; 9 and the repeat of 10 were dropped");
    assert_eq!(after.players[&0].rejected_moves, 0);
    assert_eq!(rig.gateway.stats().inputs_late, 2);
}

#[test]
fn every_player_is_sent_every_tick_and_the_states_match_the_server() {
    let _serial = serial();
    const PLAYERS: u16 = 50;
    let Some(mut rig) = rig("content", flat_floor_map(), &grid(PLAYERS as usize, 20.0), PLAYERS, 90_000) else {
        return;
    };
    let crowd = Crowd::connect(rig.gateway.local_addr(), 0..PLAYERS, Impairment::none(), rig.capacity, true);
    crowd.join_all(WAIT).unwrap();
    let before = rig._server.tick_metrics();
    let mut truth = Truth::new(rig.capacity);
    run_walk(&rig.client, &mut rig.walkers, &crowd, &mut truth, Duration::from_secs(4));
    std::thread::sleep(Duration::from_millis(200));
    let used = rig._server.tick_metrics().since(&before);

    let report = analyze(&crowd, &truth, truth.window(10, 3), &DEFAULT_BANDS);
    println!("{report}");
    assert_eq!(report.missed_ticks, 0, "every player got a datagram every tick");

    // the content: every state received is the server's, to within the packing's resolution
    let bounds = Bounds::from_world(flat_floor_map().world_bounds);
    let mut checked = 0;
    for player in crowd.players() {
        for (tick, packed) in player.states() {
            let Some(at) = truth.ticks.get(&tick) else { continue };
            let state = packed.unpack(&bounds);
            let want = at.positions[state.player as usize].expect("a state of a player the server has");
            for (axis, want) in want.iter().enumerate() {
                let step = (bounds.max[axis] - bounds.min[axis]) / 65535.0;
                assert!((state.position[axis] - want).abs() <= step, "tick {tick}, player {}", state.player);
            }
            assert_eq!(
                state.tick, at.updated_tick[state.player as usize] as u8,
                "tick {tick}, player {}",
                state.player
            );
            assert_ne!(state.player, player.id, "sent to its own player");
            checked += 1;
        }
    }
    assert!(checked > 40_000, "only {checked} states checked");

    // one batch a tick, not one per player
    assert!(used.submits <= used.ticks + 3.0, "{} submits in {} ticks", used.submits, used.ticks);
    assert!(used.submits >= used.ticks - 8.0, "{} submits in {} ticks", used.submits, used.ticks);
    let stats = rig.gateway.stats();
    assert_eq!(stats.inputs_late, 0);
    assert_eq!(stats.send_errors, 0);
}

#[test]
fn a_budget_holds_and_nearby_players_are_updated_every_tick_while_far_ones_less_often() {
    let _serial = serial();
    const PLAYERS: u16 = 200;
    const BUDGET: u32 = 40_000;
    let Some(mut rig) = rig("budget", flat_floor_map(), &grid(PLAYERS as usize, 40.0), PLAYERS, BUDGET) else { return };
    let crowd = Crowd::connect(rig.gateway.local_addr(), 0..PLAYERS, Impairment::none(), rig.capacity, false);
    crowd.join_all(WAIT).unwrap();
    let mut truth = Truth::new(rig.capacity);
    run_walk(&rig.client, &mut rig.walkers, &crowd, &mut truth, Duration::from_secs(10));

    let report = analyze(&crowd, &truth, truth.window(30, 3), &DEFAULT_BANDS);
    println!("{report}");
    let stats = rig.gateway.stats();
    println!(
        "gateway: sending a tick p50 {:.2} p99 {:.2} max {:.2} ms",
        stats.send_ms.p50, stats.send_ms.p99, stats.send_ms.max
    );

    assert!(
        report.max_download <= BUDGET as f64 * 1.02,
        "downloaded {:.0} B/s of a {BUDGET} B/s budget",
        report.max_download
    );
    assert!(report.mean_download >= BUDGET as f64 * 0.85, "the budget is being used: {:.0} B/s", report.mean_download);
    assert!(report.missed_ticks <= report.expected_receipts / 1000, "{} ticks missed", report.missed_ticks);
    let bands = &report.bands;
    assert!(bands[0].pairs > 1000, "players were near each other");
    assert!(
        bands[0].fraction_updated >= 0.999,
        "players within 10 wu were updated in {:.3}% of ticks",
        bands[0].fraction_updated * 100.0
    );
    assert!(bands[0].hz > bands[1].hz && bands[1].hz > bands[2].hz && bands[2].hz > bands[3].hz, "{bands:?}");
    assert!(bands[3].updated > 0, "far players are still updated now and then");
    // tick 30 Hz, state age
    assert!(report.tick_age_ms.p50 < 10.0, "median tick age {:.2} ms", report.tick_age_ms.p50);

    // sending is spread over the threads
    assert_eq!(stats.per_thread_datagrams.len(), 4);
    assert!(stats.per_thread_datagrams.iter().all(|n| *n > 0), "{:?}", stats.per_thread_datagrams);
    assert!(stats.send_ms.p99 < 10.0, "sending a tick took {:.2} ms at p99", stats.send_ms.p99);
}

#[test]
fn the_harness_injects_loss_and_delay_in_both_directions() {
    let _serial = serial();
    const PLAYERS: u16 = 60;
    // a budget that buys one datagram a tick, so each lost datagram is a missed tick
    const BUDGET: u32 = 3_000;
    let Some(mut rig) = rig("lossy", flat_floor_map(), &grid(PLAYERS as usize, 30.0), PLAYERS, BUDGET) else { return };
    let link = Impairment::loss(0.05).delayed(Duration::from_millis(20));
    let crowd = Crowd::connect(rig.gateway.local_addr(), 0..PLAYERS, link, rig.capacity, false);
    crowd.join_all(WAIT).unwrap();
    let mut truth = Truth::new(rig.capacity);
    run_walk(&rig.client, &mut rig.walkers, &crowd, &mut truth, Duration::from_secs(10));

    let report = analyze(&crowd, &truth, truth.window(30, 3), &DEFAULT_BANDS);
    println!("{report}");
    let missed = report.missed_ticks as f64 / report.expected_receipts as f64;
    assert!((0.03..=0.07).contains(&missed), "5% loss missed {:.2}% of ticks", missed * 100.0);
    // 20 ms of delay shows in the age of a tick on arrival, on top of the usual few ms
    assert!((20.0..35.0).contains(&report.tick_age_ms.p50), "median tick age {:.1} ms", report.tick_age_ms.p50);
    // and 5% of the inputs never reached the gateway (each of 60 players sends once a tick)
    let stats = rig.gateway.stats();
    let sent = report.ticks as f64 * PLAYERS as f64; // roughly: the window is most of the run
    assert!(
        stats.inputs_received as f64 > 0.90 * sent
            && (stats.inputs_received as f64) < 1.0 * (sent + 33.0 * PLAYERS as f64)
    );
    // players kept moving all the same, the server's rejections aside
    assert!(truth.ticks.values().last().unwrap().rejected_total < report.ticks as u64);
}

#[test]
fn five_hundred_players_on_blood_gulch_hold_a_90_kb_s_budget() {
    let _serial = serial();
    const PLAYERS: u16 = 500;
    const BUDGET: u32 = 90_000;
    let Some(halo_map) = real_map("bloodgulch") else { return };
    let anchors: Vec<[f32; 3]> = halo_map.player_starts.iter().map(|s| s.position).collect();
    let map = MapData::from(halo_map);
    let Some(mut rig) = rig("bloodgulch", map, &anchors, PLAYERS, BUDGET) else { return };
    let crowd = Crowd::connect(rig.gateway.local_addr(), 0..PLAYERS, Impairment::none(), rig.capacity, false);
    crowd.join_all(WAIT).unwrap();
    let mut truth = Truth::new(rig.capacity);
    run_walk(&rig.client, &mut rig.walkers, &crowd, &mut truth, Duration::from_secs(20));

    let report = analyze(&crowd, &truth, truth.window(60, 3), &DEFAULT_BANDS);
    println!("{report}");
    let stats = rig.gateway.stats();
    println!(
        "gateway: sending a tick p50 {:.2} p99 {:.2} max {:.2} ms",
        stats.send_ms.p50, stats.send_ms.p99, stats.send_ms.max
    );
    assert!(report.max_download <= BUDGET as f64 * 1.02);
    assert_eq!(report.missed_ticks, 0);
    assert!(report.bands[0].fraction_updated >= 0.999);
    assert!(report.tick_age_ms.p50 < 10.0);
    assert!(stats.send_ms.max < 10.0 || stats.send_ms.p99 < 10.0);
}
