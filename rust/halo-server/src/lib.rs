//! The server orchestration of the large-scale mode: what an operator runs.
//!
//! One program, `halo-server`, started from one configuration file
//! ([`config::EXAMPLE`]), runs a whole server:
//!
//! 1. SpacetimeDB Standalone, started and stopped by it (or one that is
//!    already running: one instance holds everything, as the licence wants);
//! 2. the **root database** ([`root`], from `rust/halo-root-module`), published
//!    to it: the server list, the identities seen and the bans;
//! 3. each configured **server** ([`servers`]): a rotation of matches, each a
//!    database of its own published from the match module, with a gateway in
//!    front of it, the server's row in the list rewritten for each match, and
//!    the last match's database deleted once the players have moved on;
//! 4. the log: tick time, player count, bandwidth and rejected moves, a line
//!    every few seconds for each server.
//!
//! Run it as a service that is restarted if it exits: it exits (cleanly, after
//! taking its matches down) when SpacetimeDB or the root database is lost, and
//! a start deletes the matches an earlier run left behind.
//!
//! # Identity and bans
//!
//! The owner (the identity in `owner_token_file`) publishes every database. A
//! player's identity is the SpacetimeDB identity of the token their client
//! keeps; the same token is good on the root database and on every match of the
//! instance. `halo-server ban <identity> [reason]` bans it in the root
//! database; the orchestration, which watches the root database's bans, calls
//! each running match's `set_ban` (a module cannot read another database, so
//! this is how a ban crosses), and every new match starts with all of them.

pub mod admin;
pub mod config;
pub mod fixtures;
pub mod maps;
pub mod matches;
pub mod root;
pub mod servers;
pub mod standalone;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use admin::Admin;
use config::Config;
use maps::MapSource;
use matches::Env;
use root::Root;
use servers::{Log, ServerRun, Shared};
use standalone::Standalone;

/// A running server: the orchestration's threads (one per server of the list)
/// and what they stand on.
pub struct Running {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<Result<(), String>>>,
    // (dropped last, which stops SpacetimeDB if this started it)
    standalone: Option<Standalone>,
    /// The token of the identity that owns every database.
    pub owner_token: String,
}

/// Start everything and return once the servers are running.
pub fn start(config: Config, maps: Arc<dyn MapSource>, log: Log) -> Result<Running, String> {
    let admin = Admin::new(&config.spacetimedb.url)?;
    let standalone = if config.spacetimedb.start {
        let bin = config.spacetimedb.bin_dir.as_deref().ok_or("start = true needs bin_dir")?;
        let data = config.spacetimedb.data_dir.as_deref().ok_or("start = true needs data_dir")?;
        log.line(
            "halo-server",
            format!("starting SpacetimeDB at {} (data in {})", config.spacetimedb.url, data.display()),
        );
        Some(Standalone::start(bin, data, &config.spacetimedb.url, &admin)?)
    } else {
        if !admin.ping() {
            return Err(format!("no SpacetimeDB answers at {}", config.spacetimedb.url));
        }
        None
    };

    let owner_token = owner_token(&admin, &config.spacetimedb.owner_token_file)?;
    let read = |path: &Path| std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()));
    let root_module = read(&config.root.module)?;
    let match_module = read(&config.matches.module)?;

    admin.publish(&config.root.database, &root_module, &owner_token)?;
    let root = connect_root(&config, &owner_token)?;
    log.line("halo-server", format!("root database {} is up", config.root.database));

    // what an earlier run left: its matches, and the list that named them
    let env = Env {
        admin: admin.clone(),
        uri: config.spacetimedb.url.clone(),
        owner_token: owner_token.clone(),
        match_module,
    };
    for stale in root.servers() {
        if !stale.database.is_empty() {
            match admin.delete(&stale.database, &owner_token) {
                Ok(()) => log.line("halo-server", format!("deleted {}, left from an earlier run", stale.database)),
                Err(e) => log.line("halo-server", format!("could not delete {}: {e}", stale.database)),
            }
        }
    }
    root.clear_servers()?;

    let shared = Arc::new(Shared { env, maps, root, log: log.clone(), stop: AtomicBool::new(false) });
    let mut threads = Vec::new();
    for server in config.servers {
        let (tx, rx) = mpsc::channel();
        shared.root.watch_bans(tx);
        let run = ServerRun::new(shared.clone(), server.clone(), rx);
        let handle = std::thread::Builder::new()
            .name(format!("server-{}", server.id))
            .spawn(move || run.run())
            .map_err(|e| e.to_string())?;
        threads.push(handle);
    }
    Ok(Running { shared, threads, standalone, owner_token })
}

impl Running {
    /// The root database's connection, for tests and tools.
    pub fn root(&self) -> &Root {
        &self.shared.root
    }

    /// Wait until the servers are told to stop (by [`Running::stop`] or
    /// `request_stop` from another thread) or one of them has given up.
    pub fn wait(&self) {
        while !self.shared.stop.load(Relaxed) && self.threads.iter().all(|t| !t.is_finished()) {
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Ask the servers to stop; [`Running::stop`] then finishes the job.
    pub fn request_stop(&self) {
        self.shared.stop.store(true, Relaxed);
    }

    /// Stop every server: each leaves the list and takes its matches down
    /// (their gateways, and their databases). The first error any had.
    pub fn stop(mut self) -> Result<(), String> {
        self.shared.stop.store(true, Relaxed);
        let mut result = Ok(());
        for thread in self.threads.drain(..) {
            match thread.join() {
                Ok(Ok(())) => {}
                Ok(Err(e)) => result = result.and(Err(e)),
                Err(_) => result = result.and(Err("a server thread panicked".into())),
            }
        }
        self.shared.root.disconnect();
        self.shared.log.line("halo-server", "stopped");
        drop(self.standalone.take());
        result
    }
}

/// The owner's token: the file's, or a new identity's, kept there.
fn owner_token(admin: &Admin, file: &Path) -> Result<String, String> {
    if let Ok(text) = std::fs::read_to_string(file) {
        let token = text.trim();
        if !token.is_empty() {
            return Ok(token.to_string());
        }
    }
    let account = admin.new_identity()?;
    if let Some(parent) = file.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(file, format!("{}\n", account.token)).map_err(|e| format!("{}: {e}", file.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600));
    }
    Ok(account.token)
}

/// The owner's connection to the root database, retried for a few seconds
/// (a database just published takes a moment to take connections).
pub fn connect_root(config: &Config, owner_token: &str) -> Result<Root, String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match Root::connect(&config.spacetimedb.url, &config.root.database, owner_token) {
            Ok(root) => return Ok(root),
            Err(e) if Instant::now() > deadline => return Err(e),
            Err(_) => std::thread::sleep(Duration::from_millis(250)),
        }
    }
}

/// The token in `file` (made if there is none yet): for the tools that act
/// as the owner.
pub fn owner_token_from(config: &Config) -> Result<String, String> {
    let admin = Admin::new(&config.spacetimedb.url)?;
    owner_token(&admin, &config.spacetimedb.owner_token_file)
}
