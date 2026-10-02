//! Measures the gateway with a crowd of simulated UDP players on a real map,
//! against a SpacetimeDB Standalone and a gateway this program starts itself.
//!
//!   HALO_STDB_BIN=<dir with spacetimedb-standalone and -cli> \
//!   HALO_MAP_DIR=<dir with the .map files> \
//!   cargo run --release --bin halo-wire-bench -- --players 500 --secs 30
//!
//! Options (each `--name value`): `--map bloodgulch` (or `flat`, which needs
//! no game data), `--players 500`, `--secs 30`, `--warmup 5`,
//! `--budget 90000` (bytes a second a player may be sent), `--send-threads 4`,
//! `--loss 0` (chance a datagram is lost, each way), `--delay-ms 0` (each way).
//!
//! Prints the report and checks the numbers the gateway ticket asks for;
//! exits non-zero if one is missed.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use halo_gateway::harness::{analyze, run_walk, Crowd, Impairment, Truth, DEFAULT_BANDS};
use halo_gateway::{Gateway, GatewayConfig, UdpTransport};
use halo_match_driver::server::{build_module, stdb_bin_dir, Server};
use halo_match_driver::walkers::Walkers;
use halo_match_driver::MatchClient;
use halo_sim::fixtures::flat_floor_map;
use halo_sim::MapData;

fn main() {
    let args: HashMap<String, String> = std::env::args()
        .skip(1)
        .collect::<Vec<_>>()
        .chunks(2)
        .filter_map(|c| Some((c.first()?.trim_start_matches("--").to_string(), c.get(1)?.clone())))
        .collect();
    let get = |k: &str, d: &str| args.get(k).cloned().unwrap_or_else(|| d.to_string());
    let map_name = get("map", "bloodgulch");
    let players: u16 = get("players", "500").parse().unwrap();
    let secs: u64 = get("secs", "30").parse().unwrap();
    let warmup: u64 = get("warmup", "5").parse().unwrap();
    let budget: u32 = get("budget", "90000").parse().unwrap();
    let send_threads: usize = get("send-threads", "4").parse().unwrap();
    let loss: f32 = get("loss", "0").parse().unwrap();
    let delay_ms: u64 = get("delay-ms", "0").parse().unwrap();

    let (map, anchors): (MapData, Vec<[f32; 3]>) = if map_name == "flat" {
        let side = (players as f32).sqrt().ceil() as usize;
        let anchors = (0..players as usize)
            .map(|i| {
                [-35.0 + (i % side) as f32 * 70.0 / side as f32, -35.0 + (i / side) as f32 * 70.0 / side as f32, 0.0]
            })
            .collect();
        (flat_floor_map(), anchors)
    } else {
        let dir =
            PathBuf::from(std::env::var_os("HALO_MAP_DIR").expect("HALO_MAP_DIR: the folder with the .map files"));
        let halo_map = halo_map::HaloMap::from_path(dir.join(format!("{map_name}.map"))).expect("load the map");
        let anchors = halo_map.player_starts.iter().map(|s| s.position).collect();
        (MapData::from(halo_map), anchors)
    };

    let bin = stdb_bin_dir().expect("HALO_STDB_BIN: the SpacetimeDB 2.10.x release directory");
    let server = Server::start(&bin);
    server.publish(&build_module(), "bench");
    let client = MatchClient::connect(&server.uri(), "bench");
    client.load_map(map.to_bytes()).expect("load_map");
    let (mut walkers, spawn) = Walkers::new(map, &anchors, players, 1);
    client.add_players(&spawn).expect("add_players");
    client.start();

    let mut config = GatewayConfig::new(server.uri(), "bench");
    config.budget_bytes_per_second = budget;
    config.send_threads = send_threads;
    let transport = Arc::new(UdpTransport::bind("127.0.0.1:0".parse().unwrap()).unwrap());
    let gateway = Gateway::start(config, transport).expect("start the gateway");

    let link = Impairment::loss(loss).delayed(Duration::from_millis(delay_ms));
    let crowd = Crowd::connect(gateway.local_addr(), 0..players, link, players as usize, false);
    crowd.join_all(Duration::from_secs(60)).expect("every player joins");

    let mut truth = Truth::new(players as usize);
    run_walk(&client, &mut walkers, &crowd, &mut truth, Duration::from_secs(warmup + secs));
    let warm_ticks = warmup as usize * 30;
    let report = analyze(&crowd, &truth, truth.window(warm_ticks, 3), &DEFAULT_BANDS);
    let stats = gateway.stats();

    println!("== {map_name}, {players} players, budget {budget} B/s, {send_threads} send threads, loss {loss} delay {delay_ms} ms ==");
    print!("{report}");
    println!(
        "gateway sending a tick  p50 {:.2}  p99 {:.2}  max {:.2} ms   (arrival of the tick at the gateway p50 {:.2} p99 {:.2} ms)",
        stats.send_ms.p50, stats.send_ms.p99, stats.send_ms.max, stats.arrival_ms.p50, stats.arrival_ms.p99
    );
    println!(
        "gateway: {} ticks ({} skipped), {} batches, {} inputs ({} late, {} unbound), {} datagrams, {} send errors, {} overruns",
        stats.ticks,
        stats.ticks_skipped,
        stats.batches_submitted,
        stats.inputs_received,
        stats.inputs_late,
        stats.inputs_unbound,
        stats.datagrams_sent,
        stats.send_errors,
        stats.send_overruns
    );
    println!("datagrams per thread    {:?}", stats.per_thread_datagrams);

    if loss > 0.0 || delay_ms > 0 {
        return;
    }
    let checks = [
        ("download per player within the budget", report.max_download <= budget as f64 * 1.001),
        ("every player receives every tick", report.missed_ticks == 0),
        ("median tick age under 10 ms", report.tick_age_ms.p50 < 10.0),
        ("players within 10 wu updated every tick", report.bands[0].fraction_updated >= 0.999),
        ("sending a tick to everyone under 10 ms (p99)", stats.send_ms.p99 < 10.0),
    ];
    let mut failed = false;
    for (what, ok) in checks {
        println!("{} {what}", if ok { "pass:" } else { "FAIL:" });
        failed |= !ok;
    }
    if failed {
        std::process::exit(1);
    }
}
