//! The gateway as a program: run it on the machine that runs SpacetimeDB.
//!
//!   halo-gateway --database <match database> [options]
//!
//! Options (each `--name value`):
//!
//!   --spacetimedb <uri>      SpacetimeDB to connect to       (default http://127.0.0.1:3000)
//!   --database <name>        the match's database            (required)
//!   --bind <addr:port>       where players send UDP          (default 0.0.0.0:7777)
//!   --budget <bytes/s>       most a player is sent a second, IP and UDP headers
//!                            included                        (default 90000)
//!   --send-threads <n>       threads that send               (default 4)
//!   --log-secs <n>           seconds between log lines       (default 5)
//!
//! Prints a line every few seconds: ticks, players, bandwidth, the time to
//! send a tick, inputs dropped.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use halo_gateway::{Gateway, GatewayConfig, UdpTransport};

const USAGE: &str = "halo-gateway --database <name> [--spacetimedb <uri>] [--bind <addr:port>] [--budget <bytes/s>] [--send-threads <n>] [--log-secs <n>]";

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return;
    }
    let mut args: HashMap<&str, &str> = HashMap::new();
    for pair in argv.chunks(2) {
        match pair {
            [name, value] if name.starts_with("--") => {
                args.insert(&name[2..], value);
            }
            _ => fail(&format!("expected `--name value`, got {pair:?}")),
        }
    }
    let get = |name: &str, default: &'static str| args.get(name).copied().unwrap_or(default).to_string();
    let Some(database) = args.get("database") else { fail("--database is required") };
    let bind: SocketAddr = get("bind", "0.0.0.0:7777").parse().unwrap_or_else(|e| fail(&format!("--bind: {e}")));
    let mut config = GatewayConfig::new(get("spacetimedb", "http://127.0.0.1:3000"), *database);
    config.budget_bytes_per_second = get("budget", "90000").parse().unwrap_or_else(|e| fail(&format!("--budget: {e}")));
    config.send_threads = get("send-threads", "4").parse().unwrap_or_else(|e| fail(&format!("--send-threads: {e}")));
    let log_secs: u64 = get("log-secs", "5").parse().unwrap_or_else(|e| fail(&format!("--log-secs: {e}")));
    if config.send_threads == 0 {
        fail("--send-threads must be at least 1");
    }

    let transport = Arc::new(UdpTransport::bind(bind).unwrap_or_else(|e| fail(&format!("binding {bind}: {e}"))));
    let gateway = Gateway::start(config.clone(), transport).unwrap_or_else(|e| fail(&e));
    eprintln!(
        "halo-gateway: serving {} on udp {}, {} B/s a player, {} sending threads",
        config.database,
        gateway.local_addr(),
        config.budget_bytes_per_second,
        config.send_threads
    );
    let mut last = (Instant::now(), gateway.stats());
    loop {
        std::thread::sleep(Duration::from_secs(log_secs));
        let (now, stats) = (Instant::now(), gateway.stats());
        let secs = now.duration_since(last.0).as_secs_f64();
        let ticks = stats.ticks - last.1.ticks;
        eprintln!(
            "ticks {} ({:.1}/s, {} missed)  players {}  out {:.2} MB/s  send {:.2} ms p50 {:.2} ms max  batches {}  inputs late {} unbound {}  errors {}",
            stats.ticks,
            ticks as f64 / secs,
            stats.ticks_skipped,
            gateway.sessions(),
            (stats.wire_bytes_sent - last.1.wire_bytes_sent) as f64 / secs / 1e6,
            stats.send_ms.p50,
            stats.send_ms.max,
            stats.batches_submitted,
            stats.inputs_late,
            stats.inputs_unbound,
            stats.send_errors
        );
        last = (now, stats);
    }
}

fn fail(message: &str) -> ! {
    eprintln!("halo-gateway: {message}");
    std::process::exit(2);
}
