//! Load generator and probe for the SpacetimeDB fan-out benchmark.
//!
//! Opens one connection per simulated player. Every connection subscribes to
//! `unit_state` (all rows, or one interest cell), sends its input at the tick
//! rate, and records how old each tick's state is when it arrives.

mod module_bindings;
use module_bindings::*;

use spacetimedb_sdk::{Compression, DbContext, TableWithPrimaryKey};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn now_us() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros() as i64
}

struct Sample {
    tick: u32,
    stamped_us: i64,
    arrival_us: i64,
}

#[derive(Default)]
struct ClientStats {
    samples: Mutex<Vec<Sample>>,
    rows: AtomicU64,
    disconnected: AtomicBool,
}

struct Args {
    players: u32,
    clients: u32,
    cells: u32,
    secs: u64,
    warmup: u64,
    hz: f64,
    confirmed: bool,
    compression: Compression,
    inputs: bool,
    server_pid: Option<u32>,
    uri: String,
    db: String,
    label: String,
}

fn parse_args() -> Args {
    let mut map: HashMap<String, String> = HashMap::new();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    for pair in argv.chunks(2) {
        if let [key, value] = pair {
            map.insert(key.trim_start_matches("--").to_string(), value.clone());
        }
    }
    let get = |key: &str, default: &str| map.get(key).cloned().unwrap_or(default.to_string());
    let players: u32 = get("players", "128").parse().unwrap();
    Args {
        players,
        clients: get("clients", &players.to_string()).parse().unwrap(),
        cells: get("cells", "1").parse().unwrap(),
        secs: get("secs", "30").parse().unwrap(),
        warmup: get("warmup", "5").parse().unwrap(),
        hz: get("hz", "30").parse().unwrap(),
        confirmed: get("confirmed", "1") == "1",
        compression: match get("compression", "brotli").as_str() {
            "none" => Compression::None,
            "gzip" => Compression::Gzip,
            _ => Compression::Brotli,
        },
        inputs: get("inputs", "1") == "1",
        server_pid: map.get("server-pid").map(|p| p.parse().unwrap()),
        uri: get("uri", "http://127.0.0.1:3777"),
        db: get("db", "halobench"),
        label: get("label", ""),
    }
}

/// User plus system CPU seconds a process has used so far.
fn cpu_seconds(pid: &str) -> f64 {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return 0.0;
    };
    let after = &stat[stat.rfind(')').unwrap() + 2..];
    let fields: Vec<&str> = after.split_whitespace().collect();
    let ticks: u64 = fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap();
    ticks as f64 / 100.0
}

fn loopback_bytes() -> u64 {
    std::fs::read_to_string("/sys/class/net/lo/statistics/tx_bytes")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

fn connect(args: &Args) -> DbConnection {
    DbConnection::builder()
        .with_uri(args.uri.as_str())
        .with_database_name(args.db.as_str())
        .with_confirmed_reads(args.confirmed)
            .with_compression(args.compression)
        .build()
        .expect("connect failed")
}

fn percentile(sorted: &[i64], p: f64) -> i64 {
    if sorted.is_empty() {
        return 0;
    }
    let index = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[index]
}

fn ms(us: i64) -> f64 {
    us as f64 / 1000.0
}

fn main() {
    let args = parse_args();
    let interval = Duration::from_secs_f64(1.0 / args.hz);

    let control = connect(&args);
    control.run_threaded();
    let (tx, rx) = mpsc::channel();
    control
        .reducers
        .setup_then(args.players, args.cells, move |_, result| {
            tx.send(format!("{result:?}")).unwrap();
        })
        .unwrap();
    let setup = rx.recv_timeout(Duration::from_secs(30)).expect("setup timed out");
    assert!(setup.starts_with("Ok(Ok"), "setup failed: {setup}");

    let applied = Arc::new(AtomicU64::new(0));
    let mut clients: Vec<(Arc<DbConnection>, Arc<ClientStats>)> = Vec::new();
    for i in 0..args.clients {
        let stats = Arc::new(ClientStats::default());
        let conn = DbConnection::builder()
            .with_uri(args.uri.as_str())
            .with_database_name(args.db.as_str())
            .with_confirmed_reads(args.confirmed)
            .with_compression(args.compression)
            .on_disconnect({
                let stats = stats.clone();
                move |_, _| stats.disconnected.store(true, Ordering::Relaxed)
            })
            .build()
            .expect("connect failed");

        // Each client times one row per tick: the lowest player id it can see.
        let probe = if args.cells > 1 { i % args.cells } else { 0 };
        conn.db.unit_state().on_update({
            let stats = stats.clone();
            move |_, _old, new| {
                stats.rows.fetch_add(1, Ordering::Relaxed);
                if new.player_id == probe {
                    stats.samples.lock().unwrap().push(Sample {
                        tick: new.tick,
                        stamped_us: new.stamped_us,
                        arrival_us: now_us(),
                    });
                }
            }
        });

        let query = if args.cells > 1 {
            format!("SELECT * FROM unit_state WHERE cell = {}", i % args.cells)
        } else {
            "SELECT * FROM unit_state".to_string()
        };
        conn.subscription_builder()
            .on_applied({
                let applied = applied.clone();
                move |_| {
                    applied.fetch_add(1, Ordering::Relaxed);
                }
            })
            .on_error(|_, err| panic!("subscription failed: {err}"))
            .subscribe(query);
        conn.run_threaded();
        clients.push((Arc::new(conn), stats));
    }

    let deadline = Instant::now() + Duration::from_secs(60);
    while applied.load(Ordering::Relaxed) < args.clients as u64 {
        assert!(Instant::now() < deadline, "subscriptions did not all apply");
        std::thread::sleep(Duration::from_millis(20));
    }

    control.reducers.start(interval.as_micros() as u64).unwrap();

    // Input senders. A few threads share the clients; each sends one input
    // per client per tick, the threads offset from each other within the tick.
    let running = Arc::new(AtomicBool::new(true));
    let mut senders = Vec::new();
    if args.inputs {
        const SENDER_THREADS: usize = 8;
        for thread_index in 0..SENDER_THREADS {
            let mine: Vec<(u32, Arc<DbConnection>)> = clients
                .iter()
                .enumerate()
                .filter(|(i, _)| i % SENDER_THREADS == thread_index)
                .map(|(i, (conn, _))| (i as u32 % args.players, conn.clone()))
                .collect();
            let running = running.clone();
            senders.push(std::thread::spawn(move || {
                let mut next = Instant::now() + interval.mul_f64(thread_index as f64 / SENDER_THREADS as f64);
                let mut tick = 0u32;
                while running.load(Ordering::Relaxed) {
                    if let Some(wait) = next.checked_duration_since(Instant::now()) {
                        std::thread::sleep(wait);
                    }
                    next += interval;
                    tick += 1;
                    for (player_id, conn) in &mine {
                        let yaw = (tick as f32 * 0.01) + *player_id as f32;
                        let _ = conn.reducers.send_input(*player_id, tick & 0xFF, yaw, 0.1, tick);
                    }
                }
            }));
        }
    }

    std::thread::sleep(Duration::from_secs(args.warmup));
    let server_pid = args.server_pid.map(|p| p.to_string());
    let measure_start_us = now_us();
    let started = Instant::now();
    let rows_before: u64 = clients.iter().map(|(_, s)| s.rows.load(Ordering::Relaxed)).sum();
    let lo_before = loopback_bytes();
    let server_cpu_before = server_pid.as_deref().map(cpu_seconds).unwrap_or(0.0);
    let client_cpu_before = cpu_seconds("self");

    std::thread::sleep(Duration::from_secs(args.secs));

    let elapsed = started.elapsed().as_secs_f64();
    let measure_end_us = now_us();
    let rows_after: u64 = clients.iter().map(|(_, s)| s.rows.load(Ordering::Relaxed)).sum();
    let lo_after = loopback_bytes();
    let server_cpu_after = server_pid.as_deref().map(cpu_seconds).unwrap_or(0.0);
    let client_cpu_after = cpu_seconds("self");

    running.store(false, Ordering::Relaxed);
    for sender in senders {
        sender.join().unwrap();
    }
    control.reducers.stop().unwrap();
    std::thread::sleep(Duration::from_millis(500));

    // Reduce.
    let mut latencies: Vec<i64> = Vec::new();
    let mut gaps: Vec<i64> = Vec::new();
    let mut tick_intervals: Vec<i64> = Vec::new();
    let mut ticks_seen: Vec<usize> = Vec::new();
    let mut first_tick = u32::MAX;
    let mut last_tick = 0u32;
    let mut disconnected = 0;
    for (index, (_, stats)) in clients.iter().enumerate() {
        if stats.disconnected.load(Ordering::Relaxed) {
            disconnected += 1;
        }
        let samples = stats.samples.lock().unwrap();
        let window: Vec<&Sample> = samples
            .iter()
            .filter(|s| s.arrival_us >= measure_start_us && s.arrival_us <= measure_end_us)
            .collect();
        ticks_seen.push(window.len());
        for pair in window.windows(2) {
            gaps.push(pair[1].arrival_us - pair[0].arrival_us);
            if index == 0 {
                tick_intervals.push(pair[1].stamped_us - pair[0].stamped_us);
            }
        }
        for sample in &window {
            latencies.push(sample.arrival_us - sample.stamped_us);
            first_tick = first_tick.min(sample.tick);
            last_tick = last_tick.max(sample.tick);
        }
    }
    latencies.sort_unstable();
    gaps.sort_unstable();
    tick_intervals.sort_unstable();
    ticks_seen.sort_unstable();
    let server_ticks = if last_tick >= first_tick { last_tick - first_tick + 1 } else { 0 };

    println!("== {} ==", args.label);
    println!(
        "players {}  clients {}  cells {}  target {} Hz  confirmed_reads {}  compression {:?}  inputs {}  measured {:.1} s",
        args.players, args.clients, args.cells, args.hz, args.confirmed, args.compression, args.inputs, elapsed
    );
    println!(
        "server ticks run        {} ({:.2} Hz achieved)",
        server_ticks,
        server_ticks as f64 / elapsed
    );
    println!(
        "server tick interval    p50 {:.1}  p99 {:.1}  max {:.1} ms",
        ms(percentile(&tick_intervals, 0.50)),
        ms(percentile(&tick_intervals, 0.99)),
        ms(percentile(&tick_intervals, 1.0))
    );
    println!(
        "ticks received/client   min {}  median {}  max {}",
        ticks_seen.first().unwrap_or(&0),
        ticks_seen.get(ticks_seen.len() / 2).unwrap_or(&0),
        ticks_seen.last().unwrap_or(&0)
    );
    println!(
        "state age on arrival    p50 {:.1}  p90 {:.1}  p99 {:.1}  p99.9 {:.1}  max {:.1} ms",
        ms(percentile(&latencies, 0.50)),
        ms(percentile(&latencies, 0.90)),
        ms(percentile(&latencies, 0.99)),
        ms(percentile(&latencies, 0.999)),
        ms(percentile(&latencies, 1.0))
    );
    println!(
        "gap between arrivals    p50 {:.1}  p99 {:.1}  max {:.1} ms   gaps over 50 ms: {}  over 100 ms: {}",
        ms(percentile(&gaps, 0.50)),
        ms(percentile(&gaps, 0.99)),
        ms(percentile(&gaps, 1.0)),
        gaps.iter().filter(|g| **g > 50_000).count(),
        gaps.iter().filter(|g| **g > 100_000).count()
    );
    println!(
        "row updates delivered   {:.0} per second",
        (rows_after - rows_before) as f64 / elapsed
    );
    println!(
        "loopback traffic        {:.2} MB/s",
        (lo_after - lo_before) as f64 / elapsed / 1e6
    );
    println!(
        "CPU cores used          server {:.2}  load generator {:.2}",
        (server_cpu_after - server_cpu_before) / elapsed,
        (client_cpu_after - client_cpu_before) / elapsed
    );
    println!("clients disconnected    {disconnected}");
    std::process::exit(0);
}
