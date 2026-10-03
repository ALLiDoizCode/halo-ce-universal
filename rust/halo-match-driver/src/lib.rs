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
/// The root database's bindings (`rust/halo-root-module`), generated the same
/// way: `spacetime generate --lang rust --out-dir src/root_bindings --bin-path
/// ../halo-root-module/target/wasm32-unknown-unknown/release/halo_root_module.wasm`
#[rustfmt::skip]
#[allow(clippy::all, dead_code, unused_imports)]
pub mod root_bindings;
pub mod server;
pub mod walkers;

use std::collections::BTreeMap;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use halo_sim::wire::encode_inputs;
use halo_sim::PlayerInput;
use module_bindings::*;
use spacetimedb_sdk::{Compression, DbContext, Table, TableWithPrimaryKey};

pub use module_bindings::{FighterRow, GameStateRow, MatchTick, PlayerRow, RosterRow, Seat, StandingRow};

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

/// Run a reducer and wait for the module's answer. `C` is the generated
/// bindings' `ReducerEventContext`, of whichever module the reducer is in.
pub fn call_reducer<C>(
    what: &str,
    invoke: impl FnOnce(
        Box<dyn FnOnce(&C, Result<Result<(), String>, spacetimedb_sdk::__codegen::InternalError>) + Send>,
    ) -> spacetimedb_sdk::Result<()>,
) -> Result<(), String> {
    let (tx, rx) = mpsc::channel();
    invoke(Box::new(move |_, result| {
        let _ = tx.send(result);
    }))
    .map_err(|e| format!("{what}: {e}"))?;
    match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(Ok(result)) => result.map_err(|e| format!("{what}: {e}")),
        Ok(Err(e)) => Err(format!("{what}: {e}")),
        Err(_) => Err(format!("{what}: no answer")),
    }
}

pub struct MatchClient {
    pub conn: DbConnection,
    ticks: Receiver<SeenTick>,
}

impl MatchClient {
    /// Connect with a fresh identity (which owns nothing: the owner-only
    /// reducers refuse it) and subscribe to `match_tick`, `player`, `seat`, `roster`, `standing`, `fighter` and `game_state`;
    /// returns once the subscription has applied.
    pub fn connect(uri: &str, database: &str) -> MatchClient {
        MatchClient::connect_as(uri, database, None)
    }

    /// Like [`MatchClient::connect`], as the identity of `token`: the match's
    /// owner, if it is the one that published.
    pub fn connect_as(uri: &str, database: &str, token: Option<&str>) -> MatchClient {
        MatchClient::try_connect_as(uri, database, token).expect("connect")
    }

    /// [`MatchClient::connect_as`] that says what went wrong instead of panicking.
    pub fn try_connect_as(uri: &str, database: &str, token: Option<&str>) -> Result<MatchClient, String> {
        let conn = DbConnection::builder()
            .with_uri(uri)
            .with_database_name(database)
            .with_token(token.map(str::to_string))
            // the gateway will run on loopback with compression off
            .with_compression(Compression::None)
            .build()
            .map_err(|e| format!("connecting to {database} on {uri}: {e}"))?;
        let (tx, ticks) = mpsc::channel();
        conn.db.match_tick().on_update(move |ctx, _old, marker| {
            let players = ctx.db.player().iter().map(|p| (p.id, p)).collect();
            let _ = tx.send(SeenTick { marker: marker.clone(), players, arrived_us: now_us() });
        });
        let database_name = database.to_string();
        let (applied_tx, applied) = mpsc::channel();
        conn.subscription_builder()
            .on_applied(move |_| {
                let _ = applied_tx.send(());
            })
            .on_error(move |_, err| eprintln!("subscription to {database_name} failed: {err}"))
            .subscribe([
                "SELECT * FROM match_tick",
                "SELECT * FROM player",
                "SELECT * FROM seat",
                "SELECT * FROM roster",
                "SELECT * FROM standing",
                "SELECT * FROM fighter",
                "SELECT * FROM game_state",
            ]);
        conn.run_threaded();
        applied
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| format!("the subscription to {database} on {uri} did not apply"))?;
        Ok(MatchClient { conn, ticks })
    }

    /// Every player's health, shields and weapons in the subscriber's copy of
    /// the table now, by player id.
    pub fn fighters(&self) -> BTreeMap<u16, FighterRow> {
        self.conn.db.fighter().iter().map(|f| (f.player, f)).collect()
    }

    /// What a player carries (tag indices; 65535 for no weapon), for tests.
    pub fn set_loadout(&self, player: u16, weapon0: u16, weapon1: u16) -> Result<(), String> {
        call_reducer("set_loadout", |cb| self.conn.reducers.set_loadout_then(player, weapon0, weapon1, cb))
    }

    pub fn load_map(&self, data: Vec<u8>) -> Result<(), String> {
        call_reducer("load_map", |cb| self.conn.reducers.load_map_then(data, cb))
    }

    pub fn add_players(&self, players: &[PlayerInput]) -> Result<(), String> {
        let batch = encode_inputs(players);
        call_reducer("add_players", |cb| self.conn.reducers.add_players_then(batch, cb))
    }

    pub fn remove_players(&self, ids: Vec<u16>) -> Result<(), String> {
        call_reducer("remove_players", |cb| self.conn.reducers.remove_players_then(ids, cb))
    }

    /// Where players that join appear (positions and yaw of the inputs), instead of
    /// where the game's rules put them: for tests and tools that need players where they
    /// say.
    pub fn set_spawn_points(&self, points: &[PlayerInput]) -> Result<(), String> {
        let batch = encode_inputs(points);
        call_reducer("set_spawn_points", |cb| self.conn.reducers.set_spawn_points_then(batch, cb))
    }

    /// Set the game (see the module's `set_game`) and begin it.
    pub fn set_game(&self, rules: &halo_sim::rules::Rules) -> Result<(), String> {
        call_reducer("set_game", |cb| {
            self.conn.reducers.set_game_then(
                rules.teams,
                rules.score_limit,
                rules.time_limit_ticks,
                rules.respawn_ticks,
                rules.respawn_growth_ticks,
                rules.suicide_penalty_ticks,
                rules.wave_ticks,
                cb,
            )
        })
    }

    /// Start the match's clock now, and clear the scores.
    pub fn begin_game(&self) -> Result<(), String> {
        call_reducer("begin_game", |cb| self.conn.reducers.begin_game_then(cb))
    }

    /// Kill a player, crediting `killer` (`None`: nobody caused it).
    pub fn report_death(&self, victim: u16, killer: Option<u16>) -> Result<(), String> {
        let killer = killer.unwrap_or(u16::MAX);
        call_reducer("report_death", |cb| self.conn.reducers.report_death_then(victim, killer, cb))
    }

    pub fn set_capacity(&self, capacity: u16) -> Result<(), String> {
        call_reducer("set_capacity", |cb| self.conn.reducers.set_capacity_then(capacity, cb))
    }

    /// How many ticks a seat is held after its connection dropped.
    pub fn set_away_grace(&self, ticks: u64) -> Result<(), String> {
        call_reducer("set_away_grace", |cb| self.conn.reducers.set_away_grace_then(ticks, cb))
    }

    /// Ban an identity from the match, with the reason its player is told.
    pub fn set_ban(&self, identity: spacetimedb_sdk::Identity, reason: &str) -> Result<(), String> {
        call_reducer("set_ban", |cb| self.conn.reducers.set_ban_then(identity, reason.to_string(), cb))
    }

    pub fn clear_ban(&self, identity: spacetimedb_sdk::Identity) -> Result<(), String> {
        call_reducer("clear_ban", |cb| self.conn.reducers.clear_ban_then(identity, cb))
    }

    /// The name an identity plays under, which the roster shows for its player.
    pub fn set_name(&self, identity: spacetimedb_sdk::Identity, name: &str) -> Result<(), String> {
        call_reducer("set_name", |cb| self.conn.reducers.set_name_then(identity, name.to_string(), cb))
    }

    /// Name the identity that runs the gateway.
    pub fn set_gateway(&self, gateway: spacetimedb_sdk::Identity) -> Result<(), String> {
        call_reducer("set_gateway", |cb| self.conn.reducers.set_gateway_then(gateway, cb))
    }

    /// Submit one batch of inputs and wait for the module to accept it.
    pub fn submit_and_wait(&self, inputs: &[PlayerInput]) -> Result<(), String> {
        let batch = encode_inputs(inputs);
        call_reducer("submit_inputs", |cb| self.conn.reducers.submit_inputs_then(batch, cb))
    }

    pub fn start_and_wait(&self) -> Result<(), String> {
        call_reducer("start", |cb| self.conn.reducers.start_then(cb))
    }

    pub fn stop_and_wait(&self) -> Result<(), String> {
        call_reducer("stop", |cb| self.conn.reducers.stop_then(cb))
    }

    pub fn reset_and_wait(&self) -> Result<(), String> {
        call_reducer("reset", |cb| self.conn.reducers.reset_then(cb))
    }

    /// The seats in the subscriber's copy of the table now, by player id.
    pub fn seats(&self) -> BTreeMap<u16, Seat> {
        self.conn.db.seat().iter().map(|s| (s.player, s)).collect()
    }

    /// The roster (a name and a team for each player in the match) in the
    /// subscriber's copy of the table now, by player id.
    pub fn roster(&self) -> BTreeMap<u16, RosterRow> {
        self.conn.db.roster().iter().map(|r| (r.player, r)).collect()
    }

    /// How every player is doing (score, deaths, alive or when they spawn) in the
    /// subscriber's copy of the table now, by player id.
    pub fn standings(&self) -> BTreeMap<u16, StandingRow> {
        self.conn.db.standing().iter().map(|s| (s.player, s)).collect()
    }

    /// The game: its rules, its clock, the team scores and how it ended.
    pub fn game(&self) -> Option<GameStateRow> {
        self.conn.db.game_state().iter().next()
    }

    /// Submit one batch without waiting for the answer (the per-tick path).
    pub fn submit(&self, inputs: &[PlayerInput]) {
        self.conn.reducers.submit_inputs(encode_inputs(inputs)).expect("submit_inputs");
    }

    pub fn start(&self) {
        self.start_and_wait().expect("start");
    }

    pub fn stop(&self) {
        self.stop_and_wait().expect("stop");
    }

    pub fn reset(&self) {
        self.reset_and_wait().expect("reset");
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

/// A player's own connection to the match, as their SpacetimeDB identity:
/// what a client holds beside its UDP traffic. It takes and leaves the seat
/// and sees the seats (the player list).
pub struct PlayerClient {
    pub conn: DbConnection,
}

impl PlayerClient {
    /// Connect as the identity of `token` and subscribe to the seats; returns
    /// once the subscription has applied.
    pub fn connect(uri: &str, database: &str, token: &str) -> PlayerClient {
        let client = PlayerClient::connect_unsubscribed(uri, database, token);
        let conn = &client.conn;
        let (applied_tx, applied) = mpsc::channel();
        conn.subscription_builder()
            .on_applied(move |_| {
                let _ = applied_tx.send(());
            })
            .on_error(|_, err| panic!("subscription failed: {err}"))
            .subscribe(["SELECT * FROM seat"]);
        applied.recv_timeout(Duration::from_secs(30)).expect("the subscription applied");
        client
    }

    /// Connect as the identity of `token` without subscribing to anything:
    /// enough to take and leave a seat, and cheap when there are hundreds
    /// (`seat()` then finds nothing).
    pub fn connect_unsubscribed(uri: &str, database: &str, token: &str) -> PlayerClient {
        let conn = DbConnection::builder()
            .with_uri(uri)
            .with_database_name(database)
            .with_token(Some(token.to_string()))
            .with_compression(Compression::None)
            .build()
            .expect("connect");
        conn.run_threaded();
        PlayerClient { conn }
    }

    /// Take a seat (or get one's own back) with the public key of the UDP key pair.
    pub fn join(&self, udp_public_key: [u8; 32]) -> Result<(), String> {
        self.join_with(udp_public_key.to_vec())
    }

    /// `join` with any bytes as the key (for the module to refuse).
    pub fn join_with(&self, udp_public_key: Vec<u8>) -> Result<(), String> {
        call_reducer("join", |cb| self.conn.reducers.join_then(udp_public_key, cb))
    }

    pub fn leave(&self) -> Result<(), String> {
        call_reducer("leave", |cb| self.conn.reducers.leave_then(cb))
    }

    /// Report hits as this player, and wait for the module to take the call
    /// (the next tick judges them: see the module's `report_hits`).
    pub fn report_hits(&self, hits: &[halo_sim::combat::HitReport]) -> Result<(), String> {
        let batch = halo_sim::wire::encode_hits(hits);
        call_reducer("report_hits", |cb| self.conn.reducers.report_hits_then(batch, cb))
    }

    /// This player's seat in the subscriber's copy of the table, if there is one.
    pub fn seat(&self) -> Option<Seat> {
        let me = self.conn.identity();
        self.conn.db.seat().iter().find(|s| s.owner == me)
    }

    /// Close the connection (as a dropped one: the seat is held, not freed).
    pub fn disconnect(&self) {
        let _ = self.conn.disconnect();
    }
}
