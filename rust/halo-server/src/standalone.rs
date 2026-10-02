//! SpacetimeDB Standalone as a child process, for `[spacetimedb] start = true`.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::admin::Admin;

pub struct Standalone {
    child: Option<Child>,
}

impl Standalone {
    /// Start `spacetimedb-standalone` (from `bin_dir`) listening where `url`
    /// says, keeping its data and its signing keys in `data_dir`, and wait
    /// for it to answer. The keys are made by the first start and kept: the
    /// identities and tokens it gave stay good across restarts.
    pub fn start(bin_dir: &Path, data_dir: &Path, url: &str, admin: &Admin) -> Result<Standalone, String> {
        let listen = url.trim_start_matches("http://").trim_end_matches('/');
        std::fs::create_dir_all(data_dir).map_err(|e| format!("{}: {e}", data_dir.display()))?;
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(data_dir.join("spacetimedb.log"))
            .map_err(|e| format!("{}: {e}", data_dir.display()))?;
        let binary = bin_dir.join("spacetimedb-standalone");
        let child = Command::new(&binary)
            .arg("start")
            .args(["--listen-addr", listen, "--non-interactive"])
            .arg("--data-dir")
            .arg(data_dir.join("data"))
            .arg("--jwt-pub-key-path")
            .arg(data_dir.join("id_ecdsa.pub"))
            .arg("--jwt-priv-key-path")
            .arg(data_dir.join("id_ecdsa"))
            .stdout(Stdio::from(log.try_clone().map_err(|e| e.to_string())?))
            .stderr(Stdio::from(log))
            .spawn()
            .map_err(|e| format!("starting {}: {e}", binary.display()))?;
        let mut standalone = Standalone { child: Some(child) };
        let deadline = Instant::now() + Duration::from_secs(60);
        while !admin.ping() {
            if let Some(status) = standalone.child.as_mut().and_then(|c| c.try_wait().ok().flatten()) {
                return Err(format!(
                    "SpacetimeDB exited at start ({status}); see {}",
                    data_dir.join("spacetimedb.log").display()
                ));
            }
            if Instant::now() > deadline {
                return Err(format!("SpacetimeDB did not come up; see {}", data_dir.join("spacetimedb.log").display()));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(standalone)
    }

    /// Stop it the way an operator would (SIGTERM, so its log is flushed).
    pub fn stop(&mut self) {
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
}

impl Drop for Standalone {
    fn drop(&mut self) {
        self.stop();
    }
}
