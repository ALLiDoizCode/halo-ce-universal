//! A throwaway local SpacetimeDB Standalone: its own port and data directory,
//! the match module published to it, stopped when dropped.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// The directory holding `spacetimedb-standalone` and `spacetimedb-cli`
/// (a SpacetimeDB 2.10.x release unpacked), from `HALO_STDB_BIN`. `None`
/// when it is not set: the tests that need a server then skip themselves.
pub fn stdb_bin_dir() -> Option<PathBuf> {
    std::env::var_os("HALO_STDB_BIN").map(PathBuf::from)
}

/// Build the module for WebAssembly and return the `.wasm`'s path.
pub fn build_module() -> PathBuf {
    let module = Path::new(env!("CARGO_MANIFEST_DIR")).join("../halo-match-module");
    let status = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args(["build", "--release", "--locked", "--target", "wasm32-unknown-unknown"])
        .current_dir(&module)
        .status()
        .expect("run cargo");
    assert!(status.success(), "building the module failed");
    module.join("target/wasm32-unknown-unknown/release/halo_match_module.wasm")
}

pub struct Server {
    bin: PathBuf,
    dir: PathBuf,
    port: u16,
    child: Option<Child>,
}

impl Server {
    /// Start a Standalone on a free port with a fresh data directory.
    pub fn start(bin: &Path) -> Server {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let dir = std::env::temp_dir().join(format!("halo-match-{}-{port}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut server = Server { bin: bin.to_path_buf(), dir, port, child: None };
        server.spawn();
        server
    }

    fn spawn(&mut self) {
        let log = std::fs::OpenOptions::new().create(true).append(true).open(self.dir.join("server.log")).unwrap();
        let child = Command::new(self.bin.join("spacetimedb-standalone"))
            .arg("start")
            .args(["--listen-addr", &format!("127.0.0.1:{}", self.port), "--non-interactive"])
            .arg("--data-dir")
            .arg(self.dir.join("data"))
            .arg("--jwt-pub-key-path")
            .arg(self.dir.join("id_ecdsa.pub"))
            .arg("--jwt-priv-key-path")
            .arg(self.dir.join("id_ecdsa"))
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .expect("start spacetimedb-standalone");
        self.child = Some(child);
        let deadline = Instant::now() + Duration::from_secs(30);
        while http_get(self.port, "/v1/ping").is_none() {
            assert!(
                Instant::now() < deadline,
                "the server did not come up; see {}",
                self.dir.join("server.log").display()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    pub fn uri(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn pid(&self) -> u32 {
        self.child.as_ref().expect("running").id()
    }

    /// Publish the module under `name` (a new database, or an update of it).
    pub fn publish(&self, wasm: &Path, name: &str) {
        let out = Command::new(self.bin.join("spacetimedb-cli"))
            .env("XDG_CONFIG_HOME", self.dir.join("cli-config"))
            .args(["publish", "--server", &self.uri(), "--anonymous", "--no-config", "-y", "-b"])
            .arg(wasm)
            .arg(name)
            .output()
            .expect("run spacetimedb-cli");
        assert!(out.status.success(), "publish failed: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// Stop the server the way an operator would (SIGTERM, so the log is
    /// flushed), then start it again on the same data and port. The module's
    /// memory starts fresh.
    pub fn restart(&mut self) {
        self.stop();
        self.spawn();
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = Command::new("kill").arg("-TERM").arg(child.id().to_string()).status();
            let deadline = Instant::now() + Duration::from_secs(20);
            while child.try_wait().ok().flatten().is_none() {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }

    /// The server's Prometheus metrics, parsed for the tick reducer.
    pub fn tick_metrics(&self) -> TickMetrics {
        let text = http_get(self.port, "/v1/metrics").expect("metrics");
        TickMetrics::parse(&text)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
        if std::env::var_os("HALO_KEEP_SERVER_DIR").is_none() {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

fn http_get(port: u16, path: &str) -> Option<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    write!(stream, "GET {path} HTTP/1.0\r\nHost: localhost\r\n\r\n").ok()?;
    let mut body = String::new();
    stream.read_to_string(&mut body).ok()?;
    let ok = body.starts_with("HTTP/1.0 200") || body.starts_with("HTTP/1.1 200");
    ok.then_some(body)
}

/// Cumulative figures from the server's metrics, for one reducer, summed over
/// databases. Subtract two readings for a window.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TickMetrics {
    /// Calls of the `tick` reducer.
    pub ticks: f64,
    /// Seconds spent in them, including evaluating subscription queries.
    pub seconds_with_queries: f64,
    /// Seconds spent in them inside the module (WebAssembly time only).
    pub wasm_seconds: f64,
    /// Calls that finished within 5 ms (including queries).
    pub within_5ms: f64,
    /// Calls of `submit_inputs`.
    pub submits: f64,
}

impl TickMetrics {
    fn parse(text: &str) -> TickMetrics {
        let mut m = TickMetrics::default();
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            let Some((name_labels, value)) = line.rsplit_once(' ') else { continue };
            let Ok(value) = value.parse::<f64>() else { continue };
            let has = |s: &str| name_labels.contains(s);
            if has("reducer=\"tick\"") {
                if name_labels.starts_with("spacetime_reducer_plus_query_duration_sec_count") {
                    m.ticks += value;
                } else if name_labels.starts_with("spacetime_reducer_plus_query_duration_sec_sum") {
                    m.seconds_with_queries += value;
                } else if name_labels.starts_with("reducer_wasm_time_usec") {
                    m.wasm_seconds += value / 1e6;
                } else if name_labels.starts_with("spacetime_reducer_plus_query_duration_sec_bucket")
                    && has("le=\"0.005\"")
                {
                    m.within_5ms += value;
                }
            } else if has("reducer=\"submit_inputs\"")
                && name_labels.starts_with("spacetime_reducer_plus_query_duration_sec_count")
            {
                m.submits += value;
            }
        }
        m
    }

    pub fn since(&self, earlier: &TickMetrics) -> TickMetrics {
        TickMetrics {
            ticks: self.ticks - earlier.ticks,
            seconds_with_queries: self.seconds_with_queries - earlier.seconds_with_queries,
            wasm_seconds: self.wasm_seconds - earlier.wasm_seconds,
            within_5ms: self.within_5ms - earlier.within_5ms,
            submits: self.submits - earlier.submits,
        }
    }

    /// Mean milliseconds per tick call, subscription evaluation included.
    pub fn mean_ms_with_queries(&self) -> f64 {
        self.seconds_with_queries * 1e3 / self.ticks
    }

    /// Mean milliseconds per tick call spent in the module.
    pub fn mean_ms_wasm(&self) -> f64 {
        self.wasm_seconds * 1e3 / self.ticks
    }
}
