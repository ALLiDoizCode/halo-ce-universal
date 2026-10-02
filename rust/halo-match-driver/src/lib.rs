//! Drives the match module the way the gateway will: one connection that
//! subscribes to the match's public tables, and submits one batch of inputs
//! each time it sees a tick complete.
//!
//! The tests in `tests/` and the `halo-match-bench` binary are built on it.
//!
//! # Running it
//!
//! Unpack a SpacetimeDB 2.10.x Linux release (`spacetimedb-standalone` and
//! `spacetimedb-cli`) anywhere and point `HALO_STDB_BIN` at that directory.
//! Both the tests and the benchmark start their own Standalone on a free
//! port with a temporary data directory, publish the module they build, and
//! stop the server when done.
//!
//! ```text
//! cd rust/halo-match-driver
//! HALO_STDB_BIN=~/spacetimedb cargo test --release
//! HALO_STDB_BIN=~/spacetimedb HALO_MAP_DIR=<the .map files> \
//!     cargo run --release --bin halo-match-bench -- --players 500 --secs 60
//! ```
//!
//! With `HALO_MAP_DIR` set, the tests also run 500 players on Blood Gulch.
//!
//! # Regenerating the bindings
//!
//! `src/module_bindings` is generated from the built module; after changing
//! a table or reducer signature:
//!
//! ```text
//! (cd ../halo-match-module && cargo build --release --target wasm32-unknown-unknown)
//! spacetime generate --lang rust --out-dir src/module_bindings \
//!     --bin-path ../halo-match-module/target/wasm32-unknown-unknown/release/halo_match_module.wasm
//! ```

#[rustfmt::skip]
#[allow(clippy::all, dead_code, unused_imports)]
pub mod module_bindings;
pub mod server;
pub mod walkers;

use std::collections::BTreeMap;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use halo_sim::wire::encode_inputs;
use halo_sim::PlayerInput;
use module_bindings::*;
use spacetimedb_sdk::{Compression, DbContext, Table, TableWithPrimaryKey};

pub use module_bindings::{MatchTick, PlayerRow};

pub fn now_us() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros() as i64
}

/// A tick's completion marker as a subscriber saw it, with every player row
/// the subscriber's copy of the table held at that moment.
#[derive(Debug, Clone)]
pub struct SeenTick {
    pub marker: MatchTick,
    pub players: BTreeMap<u16, PlayerRow>,
    /// This machine's clock when the update arrived, microseconds since the epoch.
    pub arrived_us: i64,
}

pub struct MatchClient {
    pub conn: DbConnection,
    ticks: Receiver<SeenTick>,
}

impl MatchClient {
    /// Connect and subscribe to `match_tick` and `player`; returns once the
    /// subscription has applied.
    pub fn connect(uri: &str, database: &str) -> MatchClient {
        let conn = DbConnection::builder()
            .with_uri(uri)
            .with_database_name(database)
            // the gateway will run on loopback with compression off
            .with_compression(Compression::None)
            .build()
            .expect("connect");
        let (tx, ticks) = mpsc::channel();
        conn.db.match_tick().on_update(move |ctx, _old, marker| {
            let players = ctx.db.player().iter().map(|p| (p.id, p)).collect();
            let _ = tx.send(SeenTick { marker: marker.clone(), players, arrived_us: now_us() });
        });
        let (applied_tx, applied) = mpsc::channel();
        conn.subscription_builder()
            .on_applied(move |_| {
                let _ = applied_tx.send(());
            })
            .on_error(|_, err| panic!("subscription failed: {err}"))
            .subscribe(["SELECT * FROM match_tick", "SELECT * FROM player"]);
        conn.run_threaded();
        applied.recv_timeout(Duration::from_secs(30)).expect("the subscription applied");
        MatchClient { conn, ticks }
    }

    fn call(
        &self,
        what: &str,
        invoke: impl FnOnce(
            &module_bindings::RemoteReducers,
            Box<
                dyn FnOnce(&ReducerEventContext, Result<Result<(), String>, spacetimedb_sdk::__codegen::InternalError>)
                    + Send,
            >,
        ) -> spacetimedb_sdk::Result<()>,
    ) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        invoke(
            &self.conn.reducers,
            Box::new(move |_, result| {
                let _ = tx.send(result);
            }),
        )
        .map_err(|e| format!("{what}: {e}"))?;
        match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(Ok(result)) => result.map_err(|e| format!("{what}: {e}")),
            Ok(Err(e)) => Err(format!("{what}: {e}")),
            Err(_) => Err(format!("{what}: no answer")),
        }
    }

    pub fn load_map(&self, data: Vec<u8>) -> Result<(), String> {
        self.call("load_map", |r, cb| r.load_map_then(data, cb))
    }

    pub fn add_players(&self, players: &[PlayerInput]) -> Result<(), String> {
        let batch = encode_inputs(players);
        self.call("add_players", |r, cb| r.add_players_then(batch, cb))
    }

    pub fn remove_players(&self, ids: Vec<u16>) {
        self.conn.reducers.remove_players(ids).expect("remove_players");
    }

    /// Submit one batch of inputs and wait for the module to accept it.
    pub fn submit_and_wait(&self, inputs: &[PlayerInput]) -> Result<(), String> {
        let batch = encode_inputs(inputs);
        self.call("submit_inputs", |r, cb| r.submit_inputs_then(batch, cb))
    }

    /// Submit one batch without waiting for the answer (the per-tick path).
    pub fn submit(&self, inputs: &[PlayerInput]) {
        self.conn.reducers.submit_inputs(encode_inputs(inputs)).expect("submit_inputs");
    }

    pub fn start(&self) {
        self.conn.reducers.start().expect("start");
    }

    pub fn stop(&self) {
        self.conn.reducers.stop().expect("stop");
    }

    pub fn reset(&self) {
        self.conn.reducers.reset().expect("reset");
    }

    /// The next completed tick this subscriber sees, in order.
    pub fn next_tick(&self, timeout: Duration) -> Option<SeenTick> {
        self.ticks.recv_timeout(timeout).ok()
    }

    /// Drop the ticks seen so far.
    pub fn discard_ticks(&self) {
        while self.ticks.try_recv().is_ok() {}
    }

    /// Wait until a tick with a number of at least `tick` has been seen.
    pub fn wait_for_tick(&self, tick: u64, timeout: Duration) -> SeenTick {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let seen = self.next_tick(left).unwrap_or_else(|| panic!("tick {tick} did not arrive in {timeout:?}"));
            if seen.marker.tick >= tick {
                return seen;
            }
        }
    }

    /// The players in the subscriber's copy of the table now.
    pub fn players(&self) -> BTreeMap<u16, PlayerRow> {
        self.conn.db.player().iter().map(|p| (p.id, p)).collect()
    }

    pub fn marker(&self) -> Option<MatchTick> {
        self.conn.db.match_tick().iter().next()
    }
}
