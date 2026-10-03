//! The player's wire: simulated UDP players against a real local SpacetimeDB
//! and a real gateway, asserting on what a player sends and receives.
//!
//! Like the driver's tests these start their own Standalone and need
//! `HALO_STDB_BIN` (a SpacetimeDB 2.10.x release directory); without it they
//! print a note and pass without testing. They run on a flat floor, which
//! needs no game data; the 500-player tests on Blood Gulch also need
//! `HALO_MAP_DIR` and skip without it.
//!
//! They run one at a time: a gateway, a database and hundreds of player
//! threads share the machine, and timing assertions would otherwise measure
//! the tests' crowding of each other.

use std::net::{SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use halo_gateway::harness::{
    analyze, run_walk, sim_seed, Crowd, Impairment, Rig, RigSetup, Seats, Truth, DEFAULT_BANDS,
};
use halo_match_driver::server::{build_module, stdb_bin_dir};
use halo_match_driver::PlayerClient;
use halo_sim::fixtures::flat_floor_map;
use halo_sim::{MapData, PlayerInput};
use halo_wire::auth;
use halo_wire::datagram::{
    Ack, Challenge, ClientMessage, Refused, ServerMessage, REFUSED_BAD_PROOF, REFUSED_NO_SEAT, REFUSED_STALE,
};
use halo_wire::planner::{PlannerConfig, STALENESS_BOUND_TICKS};
use halo_wire::unit::Bounds;

fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wasm() -> &'static PathBuf {
    static WASM: OnceLock<PathBuf> = OnceLock::new();
    WASM.get_or_init(build_module)
}

/// Start everything on `map`, with `players` seated walkers placed around
/// `anchors`. `None` (skip the test) without a server.
fn rig(name: &str, map: MapData, anchors: &[[f32; 3]], players: u16, budget: u32) -> Option<Rig> {
    rig_tuned(name, map, anchors, players, budget, |_| {})
}

fn rig_tuned(
    name: &str,
    map: MapData,
    anchors: &[[f32; 3]],
    players: u16,
    budget: u32,
    tune: impl FnOnce(&mut halo_gateway::GatewayConfig),
) -> Option<Rig> {
    let Some(bin) = stdb_bin_dir() else {
        eprintln!("HALO_STDB_BIN is not set: skipping, this test needs a SpacetimeDB 2.10.x release");
        return None;
    };
    Some(Rig::start(&bin, wasm(), RigSetup { name, map, anchors, players, budget }, tune))
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

/// A bare UDP socket that speaks the protocol by hand, to try what a client
/// that follows it never would.
struct Raw {
    socket: UdpSocket,
    gateway: SocketAddr,
}

impl Raw {
    fn new(gateway: SocketAddr) -> Raw {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
        Raw { socket, gateway }
    }

    fn send(&self, m: ClientMessage) {
        self.send_bytes(&m.encode());
    }

    fn send_bytes(&self, bytes: &[u8]) {
        self.socket.send_to(bytes, self.gateway).unwrap();
    }

    /// The next datagram that is not a Snapshot, if one comes.
    fn answer(&self) -> Option<ServerMessage> {
        let mut buf = [0u8; 2048];
        let deadline = Instant::now() + Duration::from_millis(800);
        while Instant::now() < deadline {
            let Ok(len) = self.socket.recv(&mut buf) else { continue };
            match ServerMessage::decode(&buf[..len]) {
                Some(ServerMessage::Snapshot(_)) | None => continue,
                other => return other,
            }
        }
        None
    }

    fn challenge(&self, player: u16) -> Challenge {
        self.send(ClientMessage::Hello { player });
        match self.answer() {
            Some(ServerMessage::Challenge(c)) => c,
            other => panic!("no challenge for a hello for player {player}: {other:?}"),
        }
    }

    /// The Auth a player with `seed` makes for a challenge.
    fn proof(player: u16, seed: &[u8; 32], c: &Challenge) -> ClientMessage {
        ClientMessage::Auth {
            player,
            stamp: c.stamp,
            cookie: c.cookie,
            signature: auth::sign_challenge(seed, player, c.stamp, &c.cookie),
        }
    }

    fn input(&self, player: u16, seq: u32, position: [f32; 3]) {
        let input = PlayerInput { player, position, yaw: 0.0, pitch: 0.0, flags: 0 };
        self.send(ClientMessage::Input { seq, input, ack: Ack::NONE });
    }
}

fn refused(player: u16, reason: u8) -> Option<ServerMessage> {
    Some(ServerMessage::Refused(Refused { player, reason }))
}

#[test]
fn a_player_is_welcomed_with_the_maps_bounds_after_proving_who_they_are() {
    let _serial = serial();
    let Some(rig) = rig("welcome", flat_floor_map(), &grid(2, 5.0), 2, 90_000) else { return };
    let crowd = Crowd::connect(rig.gateway.local_addr(), 0..2, Impairment::none(), rig.capacity, false);
    crowd.join_all(WAIT).unwrap();
    let welcome = crowd.player(1).unwrap().welcome().unwrap();
    assert_eq!(welcome.player, 1);
    assert_eq!(welcome.bounds, Bounds::from_world(flat_floor_map().world_bounds));
    assert!(welcome.tick as u64 <= rig.client.marker().unwrap().tick, "a tick the match has not reached");
    assert_eq!(rig.gateway.sessions(), 2);
    let stats = rig.gateway.stats();
    assert!(stats.challenges >= 2 && stats.auths_accepted >= 2 && stats.auths_refused == 0);
}

#[test]
fn a_udp_address_cannot_be_a_player_it_cannot_prove_it_is() {
    let _serial = serial();
    let Some(rig) = rig("cheat", flat_floor_map(), &grid(3, 5.0), 3, 90_000) else { return };
    let gateway = rig.gateway.local_addr();
    let none_bound = |what: &str| assert_eq!(rig.gateway.sessions(), 0, "{what}: an address was bound");

    // a player who has no seat: nothing to prove against
    let a = Raw::new(gateway);
    a.send(ClientMessage::Hello { player: 40 });
    assert_eq!(a.answer(), refused(40, REFUSED_NO_SEAT));

    // a proof made up: no cookie from this gateway
    let c = a.challenge(0);
    let signature = auth::sign_challenge(&sim_seed(0), 0, c.stamp, &[0u8; 16]);
    a.send(ClientMessage::Auth { player: 0, stamp: c.stamp, cookie: [0; 16], signature });
    assert_eq!(a.answer(), refused(0, REFUSED_BAD_PROOF), "a forged cookie");
    // a cookie from somewhere else is as bad: the gateway makes them for an address, a player and a time
    let c = a.challenge(0);
    let ClientMessage::Auth { stamp, cookie, signature, .. } = Raw::proof(0, &sim_seed(0), &c) else { unreachable!() };
    a.send(ClientMessage::Auth { player: 1, stamp, cookie, signature });
    assert_eq!(a.answer(), refused(1, REFUSED_BAD_PROOF), "a challenge for player 0, claimed for player 1");

    // the right challenge, signed with another player's key
    let c = a.challenge(0);
    a.send(Raw::proof(0, &sim_seed(1), &c));
    assert_eq!(a.answer(), refused(0, REFUSED_BAD_PROOF), "someone else's key");

    // a challenge made for another address, answered with the right key from this one
    let b = Raw::new(gateway);
    let for_b = b.challenge(0);
    a.send(Raw::proof(0, &sim_seed(0), &for_b));
    assert_eq!(a.answer(), refused(0, REFUSED_BAD_PROOF), "a challenge for another address");
    none_bound("refused proofs");
    // input from an address that proved nothing goes nowhere
    let start = rig.client.players()[&0].clone();
    let at = |dx: f32| [start.x + dx, start.y, start.z];
    a.input(0, 1, at(0.05));
    b.input(0, 1, at(0.05));

    // the real thing works
    let legit = Raw::new(gateway);
    let c = legit.challenge(0);
    let proof = Raw::proof(0, &sim_seed(0), &c);
    legit.send(proof);
    assert!(matches!(legit.answer(), Some(ServerMessage::Welcome(w)) if w.player == 0));
    assert_eq!(rig.gateway.sessions(), 1);

    // a proof sent again by someone who saw it: from their own address it was not made for
    let thief = Raw::new(gateway);
    thief.send(proof);
    assert_eq!(thief.answer(), refused(0, REFUSED_BAD_PROOF), "a replay from another address");
    assert_eq!(rig.gateway.sessions(), 1);
    assert_eq!(rig.gateway.stats().inputs_unbound, 2, "only the two inputs from addresses with no proof so far");

    // the legitimate player moves to a new address; the old proof is then worthless even from its own address
    let moved = Raw::new(gateway);
    let c2 = moved.challenge(0);
    moved.send(Raw::proof(0, &sim_seed(0), &c2));
    assert!(matches!(moved.answer(), Some(ServerMessage::Welcome(_))));
    legit.send(proof);
    assert_eq!(legit.answer(), refused(0, REFUSED_STALE), "the earlier proof, replayed");
    assert_eq!(rig.gateway.sessions(), 1);

    // and only the address that holds the player now plays them
    legit.input(0, 1, at(0.05));
    moved.input(0, 1, at(0.07));
    // the address of player 0 is not player 1's to move
    moved.input(1, 2, [9.0, 9.0, 0.01]);
    rig.client.discard_ticks();
    let tick = rig.client.next_tick(WAIT).unwrap().marker.tick;
    let later = rig.client.wait_for_tick(tick + 6, WAIT);
    assert_eq!(later.players[&0].x, start.x + 0.07, "the new address moves player 0, the old one does not");
    assert_eq!(later.players[&1].x, rig.client.players()[&1].x, "and nobody moved player 1");
    assert_eq!(rig.gateway.stats().inputs_unbound, 4);
    assert_eq!(rig.gateway.stats().auths_accepted, 2);
}

#[test]
fn an_input_counts_only_from_the_address_its_player_is_bound_to() {
    let _serial = serial();
    let Some(rig) = rig("unbound", flat_floor_map(), &grid(2, 5.0), 2, 90_000) else { return };
    let crowd = Crowd::connect(rig.gateway.local_addr(), 0..2, Impairment::none(), rig.capacity, false);
    crowd.join_all(WAIT).unwrap();
    let start = rig.client.players()[&0].clone();

    // player 1's socket claims to move player 0, and a socket that never said hello does too
    let moved =
        |x: f32| PlayerInput { player: 0, position: [start.x + x, start.y, start.z], yaw: 1.5, pitch: 0.0, flags: 0 };
    crowd.player(1).unwrap().send_input(&moved(0.05));
    let stranger = UdpSocket::bind("127.0.0.1:0").unwrap();
    stranger
        .send_to(
            &ClientMessage::Input { seq: 1, input: moved(0.05), ack: Ack::NONE }.encode(),
            rig.gateway.local_addr(),
        )
        .unwrap();

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
fn a_players_shots_and_reload_reach_the_players_who_get_them_in_the_state_the_server_holds() {
    let _serial = serial();
    let Some(rig) = rig("shots", flat_floor_map(), &grid(2, 3.0), 2, 90_000) else { return };
    let crowd = Crowd::connect(rig.gateway.local_addr(), 0..2, Impairment::none(), rig.capacity, true);
    crowd.join_all(WAIT).unwrap();
    let shooter = crowd.player(0).unwrap();
    let start = rig.client.players()[&0].clone();

    // player 0 fires three shots a third of a second apart and then reloads, saying so with each input
    // (as the client does: halo_sim::weapon::Hands counts the shots)
    for n in 0..60u32 {
        let shots = (n / 10).min(3) as u8;
        let reloading = if n >= 40 { halo_sim::FLAG_RELOADING } else { 0 };
        let flags = halo_sim::with_shot_counter(reloading, shots);
        let input = PlayerInput { player: 0, position: [start.x, start.y, start.z], yaw: 0.0, pitch: 0.0, flags };
        shooter.send_input(&input);
        std::thread::sleep(Duration::from_millis(33));
    }
    let last_tick = rig.client.marker().unwrap().tick;
    rig.client.wait_for_tick(last_tick + 3, WAIT);

    // the server holds them, and passes them on: the other player's states of player 0 carry them
    let held = rig.client.players()[&0].flags;
    assert_eq!(held, halo_sim::with_shot_counter(halo_sim::FLAG_RELOADING, 3), "the server's row");
    let seen: Vec<u8> =
        crowd.player(1).unwrap().states().iter().filter(|(_, s)| s.player() == 0).map(|(_, s)| s.0[15]).collect();
    assert!(!seen.is_empty());
    let counters: Vec<u8> = seen.iter().map(|f| halo_sim::shot_counter(*f)).collect();
    assert!(counters.windows(2).all(|w| w[0] <= w[1]), "the count only goes up: {counters:?}");
    assert_eq!(*counters.last().unwrap(), 3, "{counters:?}");
    assert!(counters.contains(&0), "the first states were from before any shot");
    let at_reload = seen.iter().filter(|f| **f & halo_sim::FLAG_RELOADING != 0).count();
    assert!(at_reload > 0 && at_reload < seen.len(), "the reload shows for a while: {at_reload} of {}", seen.len());
}

#[test]
fn the_newest_input_wins_and_a_late_one_is_dropped() {
    let _serial = serial();
    let Some(rig) = rig("late", flat_floor_map(), &grid(1, 0.0), 1, 90_000) else { return };
    let crowd = Crowd::connect(rig.gateway.local_addr(), 0..1, Impairment::none(), rig.capacity, false);
    crowd.join_all(WAIT).unwrap();
    let me = crowd.player(0).unwrap();
    let start = rig.client.players()[&0].clone();
    let at =
        |x: f32| PlayerInput { player: 0, position: [start.x + x, start.y, start.z], yaw: 0.0, pitch: 0.0, flags: 0 };

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
    let before = rig.server.tick_metrics();
    let mut truth = Truth::new(rig.capacity);
    run_walk(&rig.client, &mut rig.walkers, &crowd, &mut truth, Duration::from_secs(4));
    let used = rig.server.tick_metrics().since(&before);

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
    assert!(used.submits >= used.ticks - 10.0, "{} submits in {} ticks", used.submits, used.ticks);
    let stats = rig.gateway.stats();
    assert_eq!(stats.inputs_late, 0);
    assert_eq!(stats.send_errors, 0);
}

/// The wire bar for near players at `budget`: see `Report::near_service`.
fn assert_near_service(report: &halo_gateway::harness::Report, budget: u32) {
    let capacity = PlannerConfig::with_budget(budget).near_capacity();
    println!("{}", report.near_summary(capacity));
    if let Err(why) = report.near_service(capacity) {
        panic!("{why}");
    }
}

#[test]
fn a_budget_holds_and_nearby_players_are_updated_every_tick_while_far_ones_less_often() {
    let _serial = serial();
    const PLAYERS: u16 = 200;
    const BUDGET: u32 = 40_000;
    let Some(mut rig) = rig("budget", flat_floor_map(), &grid(PLAYERS as usize, 40.0), PLAYERS, BUDGET) else {
        return;
    };
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
    assert_near_service(&report, BUDGET);
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
    let Some(mut rig) = rig("lossy", flat_floor_map(), &grid(PLAYERS as usize, 30.0), PLAYERS, BUDGET) else {
        return;
    };
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
    // players kept moving all the same, and the inputs that were lost cost them no rejection
    assert_eq!(truth.ticks.values().last().unwrap().rejected_total, 0);
}

/// Run a crowd on a lossy link for `secs` and report on it.
fn lossy_run(rig: &mut Rig, players: u16, loss: f32, delay_ms: u64, secs: u64) -> halo_gateway::harness::Report {
    let link = Impairment::loss(loss).delayed(Duration::from_millis(delay_ms));
    let crowd = Crowd::connect(rig.gateway.local_addr(), 0..players, link, rig.capacity, false);
    crowd.join_all(Duration::from_secs(60)).unwrap();
    let mut truth = Truth::new(rig.capacity);
    run_walk(&rig.client, &mut rig.walkers, &crowd, &mut truth, Duration::from_secs(secs));
    let report = analyze(&crowd, &truth, truth.window(60, 3), &DEFAULT_BANDS);
    println!("{report}");
    report
}

#[test]
fn with_5_percent_loss_each_way_no_state_is_staler_than_the_bound() {
    let _serial = serial();
    const PLAYERS: u16 = 150;
    // a budget for about a sixth of the players a tick, so that a far player's turn comes every few ticks and a lost one matters
    const BUDGET: u32 = 12_000;
    let Some(mut rig) = rig("stale", flat_floor_map(), &grid(PLAYERS as usize, 40.0), PLAYERS, BUDGET) else { return };
    let report = lossy_run(&mut rig, PLAYERS, 0.05, 10, 25);
    assert!(
        report.max_age_ticks <= STALENESS_BOUND_TICKS,
        "a state was {} ticks old for someone (bound {STALENESS_BOUND_TICKS})",
        report.max_age_ticks
    );
    assert!(report.bands[0].pairs > 1000, "players were near each other");
    assert!(report.max_age_ticks > 0, "the report saw something");
}

#[test]
fn with_2_percent_loss_stalls_over_100_ms_for_nearby_players_are_under_a_tenth_of_a_percent() {
    let _serial = serial();
    const PLAYERS: u16 = 150;
    let Some(mut rig) = rig("stalls", flat_floor_map(), &grid(PLAYERS as usize, 40.0), PLAYERS, 90_000) else {
        return;
    };
    let report = lossy_run(&mut rig, PLAYERS, 0.02, 10, 25);
    let near = &report.bands[0];
    assert!(near.pairs > 1000 && near.updated > 1000, "players were near each other");
    assert!(
        near.stall_fraction < 0.001,
        "{:.4}% of updates within 10 wu came after a gap over 100 ms",
        near.stall_fraction * 100.0
    );
}

#[test]
fn a_player_who_joins_mid_match_is_brought_up_to_date_and_one_who_leaves_stops_being_sent_and_listed() {
    let _serial = serial();
    const PLAYERS: u16 = 20;
    let Some(mut rig) = rig("midjoin", flat_floor_map(), &grid(PLAYERS as usize, 10.0), PLAYERS, 90_000) else {
        return;
    };
    let gateway = rig.gateway.local_addr();
    let crowd = Crowd::connect(gateway, 0..PLAYERS, Impairment::none(), 32, false);
    crowd.join_all(WAIT).unwrap();
    let mut truth = Truth::new(32);
    run_walk(&rig.client, &mut rig.walkers, &crowd, &mut truth, Duration::from_secs(2));

    // a 21st player takes a seat and comes in
    let joiner = Seats::join_ids(&rig.server, "midjoin", &rig.client, PLAYERS..PLAYERS + 1);
    let late = Crowd::connect(gateway, PLAYERS..PLAYERS + 1, Impairment::none(), 32, true);
    late.join_all(WAIT).unwrap();
    let me = late.player(PLAYERS).unwrap();
    assert_eq!(rig.gateway.sessions(), PLAYERS as usize + 1);
    run_walk(&rig.client, &mut rig.walkers, &crowd, &mut truth, Duration::from_secs(1));

    // they have the whole picture within a few ticks of arriving
    let first = me.ticks_received()[0];
    let mut seen = std::collections::BTreeSet::new();
    for (tick, state) in me.states() {
        if tick <= first + 5 {
            seen.insert(state.player());
        }
    }
    assert_eq!(seen, (0..PLAYERS).collect(), "the 20 players already there, all within 5 ticks of the first datagram");
    // and what they were told is what the server held
    let bounds = Bounds::from_world(flat_floor_map().world_bounds);
    let mut checked = 0;
    for (tick, packed) in me.states() {
        let Some(at) = truth.ticks.get(&tick) else { continue };
        let state = packed.unpack(&bounds);
        let want = at.positions[state.player as usize].expect("a player the server has");
        for (axis, want) in want.iter().enumerate() {
            let step = (bounds.max[axis] - bounds.min[axis]) / 65535.0;
            assert!((state.position[axis] - want).abs() <= step, "tick {tick}, player {}", state.player);
        }
        checked += 1;
    }
    assert!(checked > 100, "only {checked} states checked");
    // the others are told about them too
    let told = crowd
        .player(0)
        .unwrap()
        .ticks_received()
        .into_iter()
        .filter_map(|t| crowd.player(0)?.receipt(t))
        .any(|r| r.has_state_of(PLAYERS));
    assert!(told, "player 0 was never sent the newcomer");

    // they leave: the match and the player list forget them, the gateway stops sending to them and about them
    joiner.clients[0].leave().unwrap();
    let gone_at = loop {
        let seen = rig.client.next_tick(WAIT).unwrap();
        truth.record(&seen);
        if !seen.players.contains_key(&PLAYERS) {
            break seen.marker.tick;
        }
    };
    assert!(!rig.client.seats().contains_key(&PLAYERS) && !rig.client.players().contains_key(&PLAYERS));
    run_walk(&rig.client, &mut rig.walkers, &crowd, &mut truth, Duration::from_secs(1));
    assert_eq!(rig.gateway.sessions(), PLAYERS as usize, "unbound");
    let datagrams = me.ticks_received().len();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(me.ticks_received().len(), datagrams, "datagrams still arrive for the one who left");
    for p in crowd.players() {
        for tick in (gone_at as u32 + 3)..*truth.ticks.keys().last().unwrap() {
            if let Some(r) = p.receipt(tick) {
                assert!(!r.has_state_of(PLAYERS), "player {} was sent the one who left, at tick {tick}", p.id);
            }
        }
    }
    // an address that is still sending as them counts for nothing
    me.send_input(&PlayerInput { player: PLAYERS, position: [0.0, 0.0, 0.01], yaw: 0.0, pitch: 0.0, flags: 0 });
    std::thread::sleep(Duration::from_millis(200));
    assert!(rig.gateway.stats().inputs_unbound >= 1);
}

#[test]
fn a_player_who_reconnects_from_a_new_address_resumes_as_the_same_player() {
    let _serial = serial();
    const PLAYERS: u16 = 6;
    let Some(mut rig) = rig("moved", flat_floor_map(), &grid(PLAYERS as usize, 8.0), PLAYERS, 90_000) else { return };
    let gateway = rig.gateway.local_addr();
    let crowd = Crowd::connect(gateway, 0..PLAYERS, Impairment::none(), 8, false);
    crowd.join_all(WAIT).unwrap();
    let mut truth = Truth::new(8);
    run_walk(&rig.client, &mut rig.walkers, &crowd, &mut truth, Duration::from_secs(1));
    let before = rig.client.players()[&3].clone();

    // player 3's connection comes back from a new socket
    let moved = Crowd::connect(gateway, 3..4, Impairment::none(), 8, false);
    moved.join_all(WAIT).unwrap();
    let new_me = moved.player(3).unwrap();
    assert_eq!(new_me.welcome().unwrap().player, 3, "the same player");
    assert_eq!(rig.gateway.sessions(), PLAYERS as usize, "not an additional one");
    assert_ne!(new_me.local_addr(), crowd.player(3).unwrap().local_addr());

    // it is told what is going on, and the others still hear of player 3
    let old_datagrams = crowd.player(3).unwrap().ticks_received().len();
    std::thread::sleep(Duration::from_millis(600));
    assert!(new_me.ticks_received().len() >= 10, "the new address gets Snapshots");
    assert_eq!(crowd.player(3).unwrap().ticks_received().len(), old_datagrams, "the old address no longer does");
    assert!(
        crowd
            .player(0)
            .unwrap()
            .ticks_received()
            .into_iter()
            .rev()
            .take(5)
            .filter_map(|t| crowd.player(0)?.receipt(t))
            .any(|r| r.has_state_of(3)),
        "player 0 is still told about player 3"
    );

    // the new address moves the same player on from where they stood
    let input =
        PlayerInput { player: 3, position: [before.x + 0.05, before.y, before.z], yaw: 0.7, pitch: 0.0, flags: 0 };
    crowd.player(3).unwrap().send_input(&PlayerInput { yaw: 0.2, ..input }); // the old address: ignored
    new_me.send_input(&input);
    let seen = loop {
        let seen = rig.client.next_tick(WAIT).unwrap();
        if seen.players[&3].yaw == 0.7 {
            break seen;
        }
        assert!(seen.marker.tick < before.updated_tick + 300, "the input from the new address never applied");
    };
    assert_eq!(seen.players[&3].x, before.x + 0.05);
    assert_eq!(
        seen.players[&3].rejected_moves, before.rejected_moves,
        "no rejection: the same player, in the same place"
    );
    assert!(rig.gateway.stats().inputs_unbound >= 1);
}

#[test]
fn a_player_whose_connection_dropped_comes_back_as_the_same_player_and_authenticates_again() {
    let _serial = serial();
    const PLAYERS: u16 = 3;
    let Some(rig) = rig("comeback", flat_floor_map(), &grid(PLAYERS as usize, 8.0), PLAYERS, 90_000) else { return };
    let gateway = rig.gateway.local_addr();
    let crowd = Crowd::connect(gateway, 0..PLAYERS, Impairment::none(), 8, false);
    crowd.join_all(WAIT).unwrap();

    // player 1's SpacetimeDB connection drops: the seat is held
    rig.seats.clients[1].disconnect();
    let deadline = Instant::now() + WAIT;
    while rig.client.seats()[&1].away_since == 0 {
        assert!(Instant::now() < deadline, "the seat was never marked away");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(rig.client.players().contains_key(&1));
    let identity = rig.seats.accounts[1].identity();

    // the same identity on a new connection takes the same seat (with a new key) ...
    let back = PlayerClient::connect(&rig.server.uri(), "comeback", &rig.seats.accounts[1].token);
    let new_seed = [0x77u8; 32];
    back.join(auth::public_key(&new_seed)).unwrap();
    let seat = back.seat().expect("a seat");
    assert_eq!((seat.player, seat.owner, seat.away_since), (1, identity, 0));
    assert_eq!(rig.client.seats().len(), PLAYERS as usize);

    // ... and the old key no longer proves anything, the new one does
    let raw = Raw::new(gateway);
    let c = raw.challenge(1);
    raw.send(Raw::proof(1, &sim_seed(1), &c));
    assert_eq!(raw.answer(), refused(1, REFUSED_BAD_PROOF), "the key it replaced");
    let c = raw.challenge(1);
    raw.send(Raw::proof(1, &new_seed, &c));
    assert!(matches!(raw.answer(), Some(ServerMessage::Welcome(w)) if w.player == 1));
    assert_eq!(rig.gateway.sessions(), PLAYERS as usize);
}

#[test]
fn a_silent_address_is_unbound_after_the_idle_timeout_and_may_come_back() {
    let _serial = serial();
    const PLAYERS: u16 = 2;
    let tune = |c: &mut halo_gateway::GatewayConfig| c.idle_timeout = Duration::from_millis(1500);
    let Some(rig) = rig_tuned("idle", flat_floor_map(), &grid(PLAYERS as usize, 8.0), PLAYERS, 90_000, tune) else {
        return;
    };
    let crowd = Crowd::connect(rig.gateway.local_addr(), 0..PLAYERS, Impairment::none(), 8, false);
    crowd.join_all(WAIT).unwrap();
    let (talker, silent) = (crowd.player(0).unwrap(), crowd.player(1).unwrap());
    let start = rig.client.players()[&0].clone();
    let input = PlayerInput { player: 0, position: [start.x, start.y, start.z], yaw: 0.0, pitch: 0.0, flags: 0 };
    let until = Instant::now() + Duration::from_millis(3500);
    while Instant::now() < until {
        talker.send_input(&input);
        std::thread::sleep(Duration::from_millis(33));
    }
    assert_eq!(rig.gateway.sessions(), 1, "the one that kept talking stays");
    assert_eq!(rig.gateway.stats().sessions_expired, 1);
    let datagrams = silent.ticks_received().len();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(silent.ticks_received().len(), datagrams, "nothing more is sent to a silent address");

    // it says hello again and is back
    silent.restart_session();
    let deadline = Instant::now() + WAIT;
    while silent.welcome().is_none() {
        assert!(Instant::now() < deadline, "not welcomed back");
        silent.hello();
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(rig.gateway.sessions(), 2);
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
    assert_near_service(&report, BUDGET);
    assert!(report.tick_age_ms.p50 < 10.0);
    assert!(stats.send_ms.max < 10.0 || stats.send_ms.p99 < 10.0);
}
