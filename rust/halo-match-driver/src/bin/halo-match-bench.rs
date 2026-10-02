//! Measures the match module under load: N players walking on a real map,
//! one input batch per tick, on a Standalone this program starts itself.
//!
//!   HALO_STDB_BIN=<dir with spacetimedb-standalone and -cli> \
//!   HALO_MAP_DIR=<dir with the .map files> \
//!   cargo run --release --bin halo-match-bench -- --players 500 --secs 30
//!
//! Options: `--map bloodgulch`, `--players 500`, `--secs 30`, `--warmup 5`,
//! `--hz-floor 29.5` (exit non-zero below it).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use halo_match_driver::server::{build_module, stdb_bin_dir, Server};
use halo_match_driver::walkers::Walkers;
use halo_match_driver::SeenTick;
use halo_sim::MapData;

fn cpu_seconds(pid: u32) -> f64 {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else { return 0.0 };
    let after = &stat[stat.rfind(')').unwrap() + 2..];
    let f: Vec<&str> = after.split_whitespace().collect();
    (f[11].parse::<u64>().unwrap() + f[12].parse::<u64>().unwrap()) as f64 / 100.0
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    sorted.get(((sorted.len().max(1) - 1) as f64 * p).round() as usize).copied().unwrap_or(0.0)
}

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
    let hz_floor: f64 = get("hz-floor", "0").parse().unwrap();

    let dir = PathBuf::from(std::env::var_os("HALO_MAP_DIR").expect("HALO_MAP_DIR: the folder with the .map files"));
    let halo_map = halo_map::HaloMap::from_path(dir.join(format!("{map_name}.map"))).expect("load the map");
    let anchors: Vec<[f32; 3]> = halo_map.player_starts.iter().map(|s| s.position).collect();
    let map = MapData::from(halo_map);
    let blob = map.to_bytes();

    let bin = stdb_bin_dir().expect("HALO_STDB_BIN: the SpacetimeDB 2.10.x release directory");
    let server = Server::start(&bin);
    server.publish(&build_module(), "bench");
    let client = server.connect("bench");
    client.load_map(blob).expect("load_map");
    let (mut walkers, spawn) = Walkers::new(map, &anchors, players, 1);
    client.add_players(&spawn).expect("add_players");
    client.start();

    // Each time a tick completes, plan and submit the next one's batch.
    let mut step = |seen: &SeenTick| {
        let inputs = walkers.next_inputs();
        walkers.apply(&inputs, seen.marker.tick + 1);
        client.submit(&inputs);
    };

    let warm_until = Instant::now() + Duration::from_secs(warmup);
    let mut last = client.next_tick(Duration::from_secs(10)).expect("a tick");
    while Instant::now() < warm_until {
        last = client.next_tick(Duration::from_secs(10)).expect("a tick");
        step(&last);
    }

    let (t0, m0, cpu0) = (Instant::now(), server.tick_metrics(), cpu_seconds(server.pid()));
    let (tick0, rejected0) = (last.marker.tick, last.marker.rejected_total);
    let mut intervals_ms = Vec::new();
    let mut lag_ms = Vec::new();
    let mut skipped = 0u64;
    while t0.elapsed() < Duration::from_secs(secs) {
        let seen = client.next_tick(Duration::from_secs(10)).expect("a tick");
        intervals_ms.push((seen.marker.stamped_us - last.marker.stamped_us) as f64 / 1e3);
        lag_ms.push((seen.arrived_us - seen.marker.stamped_us) as f64 / 1e3);
        skipped += seen.marker.tick - last.marker.tick - 1;
        step(&seen);
        last = seen;
    }
    let elapsed = t0.elapsed().as_secs_f64();
    let m = server.tick_metrics().since(&m0);
    let cores = (cpu_seconds(server.pid()) - cpu0) / elapsed;
    intervals_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    lag_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let hz = (last.marker.tick - tick0) as f64 / elapsed;

    println!("== {map_name}, {players} players, one batch per tick, {elapsed:.1} s measured ==");
    println!("ticks run               {} ({hz:.2} Hz)", last.marker.tick - tick0);
    println!("tick calls (metrics)    {:.0}", m.ticks);
    println!(
        "mean tick time          {:.3} ms with subscription evaluation, {:.3} ms in the module",
        m.mean_ms_with_queries(),
        m.mean_ms_wasm()
    );
    println!("ticks within 5 ms       {:.1}%", 100.0 * m.within_5ms / m.ticks);
    println!(
        "tick interval (server)  p50 {:.1}  p99 {:.1}  max {:.1} ms",
        percentile(&intervals_ms, 0.5),
        percentile(&intervals_ms, 0.99),
        percentile(&intervals_ms, 1.0)
    );
    println!(
        "tick age on arrival     p50 {:.1}  p99 {:.1}  max {:.1} ms",
        percentile(&lag_ms, 0.5),
        percentile(&lag_ms, 0.99),
        percentile(&lag_ms, 1.0)
    );
    println!("submit_inputs calls     {:.0} ({:.2} per tick)", m.submits, m.submits / m.ticks);
    println!(
        "tick numbers skipped    {skipped} (a tick the subscriber did not see)   moves rejected in the window: {}",
        last.marker.rejected_total - rejected0
    );
    println!("database process        {cores:.2} cores");
    if hz < hz_floor {
        eprintln!("below the floor of {hz_floor} Hz");
        std::process::exit(1);
    }
}
