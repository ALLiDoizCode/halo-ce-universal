//! The server list, as the client sees it: a direct connection to the root
//! database (as the player's own, kept identity), which says who the player
//! is, and keeps the list of servers current.
//!
//! The connection is made on a thread of its own and made again, as the same
//! identity, if it drops; the game reads [`Browser::servers`] (a copy) and
//! never waits for the network.

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use halo_match_driver::root_bindings::{register, DbConnection, Server, ServerTableAccess};
use spacetimedb_sdk::{Compression, DbContext, Table, TableWithPrimaryKey};

use crate::identity::{is_rejected_token, IdentityFile};
use crate::session::Refusal;

/// How long the thread waits before it connects again.
const RETRY_DELAY: Duration = Duration::from_secs(1);
const POLL: Duration = Duration::from_millis(50);

/// One line of the server list.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerEntry {
    /// The operator's name for the server, stable across its matches.
    pub id: String,
    pub title: String,
    pub map: String,
    pub game_type: String,
    pub variant: String,
    /// The current match's database; empty between matches (nothing to join).
    pub database: String,
    /// The current match's gateway, `host:port` for UDP.
    pub gateway: String,
    pub players: u32,
    pub capacity: u32,
    pub match_number: u64,
    pub match_seconds: u32,
}

impl From<&Server> for ServerEntry {
    fn from(row: &Server) -> ServerEntry {
        ServerEntry {
            id: row.id.clone(),
            title: row.title.clone(),
            map: row.map.clone(),
            game_type: row.game_type.clone(),
            variant: row.variant.clone(),
            database: row.database.clone(),
            gateway: row.gateway.clone(),
            players: row.players,
            capacity: row.capacity,
            match_number: row.match_number,
            match_seconds: row.match_seconds,
        }
    }
}

#[derive(Default)]
struct State {
    /// Sorted by id.
    servers: Vec<ServerEntry>,
    /// The subscription is applied.
    connected: bool,
    /// The identity, hex, once the server has said.
    identity: Option<String>,
    /// Set when the root database refused to register the identity as banned.
    banned: Option<Refusal>,
    error: Option<String>,
}

struct Inner {
    spacetimedb: String,
    database: String,
    identity: IdentityFile,
    /// The name the player plays under, which the root database is told.
    name: String,
    token: Mutex<Option<String>>,
    state: Mutex<State>,
    stop: AtomicBool,
}

impl Inner {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
}

pub struct Browser {
    inner: Arc<Inner>,
    thread: Option<JoinHandle<()>>,
}

impl Browser {
    /// Start listing the servers of the root database `database` on the
    /// SpacetimeDB at `spacetimedb`; returns at once, whether or not it is reachable.
    pub fn start(spacetimedb: &str, database: &str, identity: IdentityFile, name: &str) -> Result<Browser, String> {
        let token = identity.load();
        let inner = Arc::new(Inner {
            spacetimedb: spacetimedb.to_string(),
            database: database.to_string(),
            identity,
            name: name.to_string(),
            token: Mutex::new(token),
            state: Mutex::new(State::default()),
            stop: AtomicBool::new(false),
        });
        let thread = {
            let inner = inner.clone();
            std::thread::Builder::new()
                .name("halo-large-browser".into())
                .spawn(move || run(&inner))
                .map_err(|e| format!("a thread: {e}"))?
        };
        Ok(Browser { inner, thread: Some(thread) })
    }

    /// The servers now, sorted by id.
    pub fn servers(&self) -> Vec<ServerEntry> {
        self.inner.state().servers.clone()
    }

    pub fn server(&self, id: &str) -> Option<ServerEntry> {
        self.inner.state().servers.iter().find(|s| s.id == id).cloned()
    }

    /// The list is up to date with the root database (and the player's identity is known).
    pub fn connected(&self) -> bool {
        self.inner.state().connected
    }

    /// The player's SpacetimeDB identity, hex, once the server has said.
    pub fn identity(&self) -> Option<String> {
        self.inner.state().identity.clone()
    }

    /// Set when the server list's own database turned the identity away as banned.
    pub fn banned(&self) -> Option<Refusal> {
        self.inner.state().banned.clone()
    }

    pub fn last_error(&self) -> Option<String> {
        self.inner.state().error.clone()
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        self.inner.stop.store(true, Relaxed);
        // (it sees the flag within POLL, or when a connect that is under way returns)
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(inner: &Arc<Inner>) {
    while !inner.stop.load(Relaxed) {
        match connect(inner) {
            Ok(connection) => {
                let runner = connection.run_threaded();
                while !inner.stop.load(Relaxed) && !runner.is_finished() {
                    std::thread::sleep(POLL);
                }
                let _ = connection.disconnect();
                let _ = runner.join();
                inner.state().connected = false;
            }
            Err(e) => inner.state().error = Some(e),
        }
        let mut waited = Duration::ZERO;
        while !inner.stop.load(Relaxed) && waited < RETRY_DELAY {
            std::thread::sleep(POLL);
            waited += POLL;
        }
    }
}

fn connect(inner: &Arc<Inner>) -> Result<DbConnection, String> {
    let token = inner.token.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let connected = inner.clone();
    let connection = DbConnection::builder()
        .with_uri(inner.spacetimedb.as_str())
        .with_database_name(inner.database.as_str())
        .with_token(token)
        .with_compression(Compression::None)
        .on_connect(move |connection, identity, token| {
            *connected.token.lock().unwrap_or_else(|p| p.into_inner()) = Some(token.into());
            if let Err(e) = connected.identity.save(token) {
                connected.state().error = Some(format!("the identity could not be kept: {e}"));
            }
            connected.state().identity = Some(identity.to_hex().to_string());
            let applied = connected.clone();
            let failed = connected.clone();
            connection
                .subscription_builder()
                .on_applied(move |ctx| {
                    let mut state = applied.state();
                    state.servers = ctx.db.server().iter().map(|row| ServerEntry::from(&row)).collect();
                    state.servers.sort_by(|a, b| a.id.cmp(&b.id));
                    state.connected = true;
                })
                .on_error(move |_, e| failed.state().error = Some(format!("the server list: {e}")))
                .subscribe(["SELECT * FROM server"]);
            // say who is here: the root database turns a banned identity away
            let told = connected.clone();
            let _ = connection.reducers.register_then(connected.name.clone(), move |_, result| match result {
                Ok(Ok(())) => told.state().banned = None,
                Ok(Err(message)) => {
                    let refusal = Refusal::from_join_error(&message);
                    let mut state = told.state();
                    state.error = Some(refusal.message.clone());
                    state.banned = Some(refusal);
                }
                Err(e) => told.state().error = Some(format!("registering: {e}")),
            });
        })
        .build()
        .map_err(|e| {
            let text = e.to_string();
            if is_rejected_token(&text) && inner.token.lock().unwrap_or_else(|p| p.into_inner()).take().is_some() {
                // the kept identity is no good here: the next try is a new one
                inner.identity.forget();
            }
            format!("SpacetimeDB {}: {text}", inner.spacetimedb)
        })?;

    let table = connection.db.server();
    let on_row = |inner: &Arc<Inner>, row: &Server| {
        let entry = ServerEntry::from(row);
        let mut state = inner.state();
        match state.servers.iter_mut().find(|s| s.id == entry.id) {
            Some(known) => *known = entry,
            None => {
                state.servers.push(entry);
                state.servers.sort_by(|a, b| a.id.cmp(&b.id));
            }
        }
    };
    let (on_insert, on_update, on_delete) = (inner.clone(), inner.clone(), inner.clone());
    table.on_insert(move |_, row| on_row(&on_insert, row));
    table.on_update(move |_, _, row| on_row(&on_update, row));
    table.on_delete(move |_, row| on_delete.state().servers.retain(|s| s.id != row.id));
    Ok(connection)
}
