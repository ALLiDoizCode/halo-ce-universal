//! The orchestration's connection to the root database, as its owner: it
//! writes the server list and the bans, and watches the bans for the matches.

use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use halo_match_driver::call_reducer;
pub use halo_match_driver::root_bindings::Banned;
use halo_match_driver::root_bindings::{
    ban_identity, clear_servers, remove_server, set_server, set_server_players, unban_identity, BannedTableAccess,
    DbConnection, KnownIdentityTableAccess, ReducerEventContext, Server as ServerRow, ServerTableAccess,
};
use spacetimedb_sdk::{Compression, DbContext, Identity, Table, TableWithPrimaryKey};

/// A change to the bans, as the root database made it.
#[derive(Debug, Clone, PartialEq)]
pub enum BanChange {
    Banned { identity: Identity, reason: String },
    Lifted { identity: Identity },
}

type Sinks = Arc<Mutex<Vec<Sender<BanChange>>>>;

pub struct Root {
    conn: DbConnection,
    sinks: Sinks,
}

impl Root {
    /// Connect as the identity of `token` and subscribe to the server list and
    /// the bans; returns once the subscription has applied.
    pub fn connect(uri: &str, database: &str, token: &str) -> Result<Root, String> {
        let conn = DbConnection::builder()
            .with_uri(uri)
            .with_database_name(database)
            .with_token(Some(token.to_string()))
            .with_compression(Compression::None)
            .build()
            .map_err(|e| format!("connecting to the root database {database} on {uri}: {e}"))?;
        let sinks: Sinks = Arc::default();
        {
            let sinks = sinks.clone();
            conn.db.banned().on_insert(move |_, row| {
                send(&sinks, BanChange::Banned { identity: row.identity, reason: row.reason.clone() })
            });
        }
        {
            let sinks = sinks.clone();
            conn.db.banned().on_update(move |_, _, row| {
                send(&sinks, BanChange::Banned { identity: row.identity, reason: row.reason.clone() })
            });
        }
        {
            let sinks = sinks.clone();
            conn.db.banned().on_delete(move |_, row| send(&sinks, BanChange::Lifted { identity: row.identity }));
        }
        let (applied_tx, applied) = mpsc::channel();
        conn.subscription_builder()
            .on_applied(move |_| {
                let _ = applied_tx.send(());
            })
            .on_error(|_, e| eprintln!("halo-server: the root database's subscription failed: {e}"))
            .subscribe(["SELECT * FROM server", "SELECT * FROM banned", "SELECT * FROM known_identity"]);
        conn.run_threaded();
        applied
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| format!("the subscription to the root database {database} did not apply"))?;
        Ok(Root { conn, sinks })
    }

    /// Whether the connection is still up.
    pub fn is_connected(&self) -> bool {
        self.conn.is_active()
    }

    /// Where every later change to the bans is sent.
    pub fn watch_bans(&self, sink: Sender<BanChange>) {
        self.sinks.lock().unwrap_or_else(|p| p.into_inner()).push(sink);
    }

    /// The bans now.
    pub fn bans(&self) -> Vec<Banned> {
        let mut bans: Vec<Banned> = self.conn.db.banned().iter().collect();
        bans.sort_by_key(|b| b.banned_us);
        bans
    }

    /// The identities that have said they are here (`register`).
    pub fn known_identities(&self) -> Vec<Identity> {
        self.conn.db.known_identity().iter().map(|k| k.identity).collect()
    }

    /// The server list now.
    pub fn servers(&self) -> Vec<ServerRow> {
        let mut servers: Vec<ServerRow> = self.conn.db.server().iter().collect();
        servers.sort_by(|a, b| a.id.cmp(&b.id));
        servers
    }

    pub fn set_server(&self, row: ServerRow) -> Result<(), String> {
        call_reducer("set_server", |cb: Box<dyn FnOnce(&ReducerEventContext, _) + Send>| {
            self.conn.reducers.set_server_then(row, cb)
        })
    }

    pub fn set_players(&self, id: &str, players: u32) -> Result<(), String> {
        call_reducer("set_server_players", |cb: Box<dyn FnOnce(&ReducerEventContext, _) + Send>| {
            self.conn.reducers.set_server_players_then(id.to_string(), players, cb)
        })
    }

    pub fn remove_server(&self, id: &str) -> Result<(), String> {
        call_reducer("remove_server", |cb: Box<dyn FnOnce(&ReducerEventContext, _) + Send>| {
            self.conn.reducers.remove_server_then(id.to_string(), cb)
        })
    }

    pub fn clear_servers(&self) -> Result<(), String> {
        call_reducer("clear_servers", |cb: Box<dyn FnOnce(&ReducerEventContext, _) + Send>| {
            self.conn.reducers.clear_servers_then(cb)
        })
    }

    pub fn ban(&self, identity: Identity, reason: &str) -> Result<(), String> {
        call_reducer("ban_identity", |cb: Box<dyn FnOnce(&ReducerEventContext, _) + Send>| {
            self.conn.reducers.ban_identity_then(identity, reason.to_string(), cb)
        })
    }

    pub fn unban(&self, identity: Identity) -> Result<(), String> {
        call_reducer("unban_identity", |cb: Box<dyn FnOnce(&ReducerEventContext, _) + Send>| {
            self.conn.reducers.unban_identity_then(identity, cb)
        })
    }

    /// Close the connection.
    pub fn disconnect(&self) {
        let _ = self.conn.disconnect();
    }
}

fn send(sinks: &Sinks, change: BanChange) {
    // a sink whose receiver is gone is dropped
    sinks.lock().unwrap_or_else(|p| p.into_inner()).retain(|sink| sink.send(change.clone()).is_ok());
}
