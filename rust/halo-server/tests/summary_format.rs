//! The server's log line and the check's summary of it agree (issue #48): the line carries the tick's
//! age on reaching the gateway beside the gateway's send time, and `check/summarise.py` reads those,
//! and the load report's tick age, into the three parts of tick age side by side.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use halo_server::config::{Config, EXAMPLE};
use halo_server::matches::Report;
use halo_server::servers::format_report;

fn line(at: u32, tick_ms: (f64, f64), arrival: (f64, f64), send: (f64, f64)) -> String {
    let config = Config::parse(EXAMPLE, Path::new("/etc/halo")).unwrap();
    let step = &config.servers[0].rotation[0];
    let report = Report {
        seconds: 1.0,
        players: 500,
        capacity: 510,
        tick_ms: Some(tick_ms),
        ticks: 30.0,
        out_bytes_per_second: 20e6,
        arrival_ms_p50: arrival.0,
        arrival_ms_max: arrival.1,
        send_ms_p50: send.0,
        send_ms_max: send.1,
        ..Report::default()
    };
    format!(
        "[{at:>4}s] lounge: match 1 bloodgulch {}",
        format_report(&report, 500, Duration::from_secs(at as u64), step)
    )
}

#[test]
fn the_log_line_carries_the_arrival_figure_beside_the_send_time() {
    let text = line(10, (2.5, 2.0), (3.1, 5.2), (1.7, 4.4));
    assert!(text.contains("arrival p50 3.10 max 5.20 ms, send p50 1.70 max 4.40 ms"), "{text}");
}

#[test]
fn the_summary_sets_the_three_parts_of_tick_age_beside_what_the_players_saw() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("summary-format");
    std::fs::create_dir_all(&dir).unwrap();
    let log: String =
        (1..=5).map(|s| line(s * 10, (2.0 + s as f64 * 0.1, 1.5), (3.0, 5.0), (1.5, 4.0)) + "\n").collect();
    std::fs::write(dir.join("server.log"), log).unwrap();
    std::fs::write(
        dir.join("load-report.txt"),
        "500 players over 9000 ticks (300.0 s)\ntick age on arrival   p50 4.60  p99 9.10  max 15.00 ms\n",
    )
    .unwrap();
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("check").join("summarise.py");
    let out = Command::new("python3").arg(script).arg(dir.join("server.log")).output().expect("run python3");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{text}{}", String::from_utf8_lossy(&out.stderr));
    // (the mean of 2.1 to 2.5 over the five seconds is 2.30)
    assert!(
        text.contains(
            "tick age in parts (ms): tick time 2.30 mean | on reaching the gateway p50 3.00 max 5.00 \
             (so commit and delivery add about 0.70 to the tick time) | gateway send p50 1.50 max 4.00"
        ),
        "{text}"
    );
    assert!(
        text.contains("tick age the players saw (load report): p50 4.60  p99 9.10  max 15.00 ms; p50 less the gateway's arrival p50 is 1.60 ms"),
        "{text}"
    );
}
