//! One match, from the orchestration's side: a database of its own (published
//! from the match module, so always fresh), the owner's connection that sets
//! it up and watches it, and the gateway in front of it. Dropping the match
//! with [`RunningMatch::stop`] takes the gateway down and deletes the database.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use halo_gateway::{Gateway, GatewayConfig, StatsSnapshot, UdpTransport};
use halo_match_driver::server::TickMetrics;
use halo_match_driver::MatchClient;
use halo_sim::rules::Rules;
use spacetimedb_sdk::{DbContext, Identity};

use crate::admin::Admin;
use crate::maps::LoadedMap;

/// What the orchestration shares between matches.
pub struct Env {
    pub admin: Admin,
    /// The SpacetimeDB's address, as clients and the gateway connect to it.
    pub uri: String,
    /// The token of the identity that owns every database (and so is every
    /// match's owner, and, until told otherwise, its gateway).
    pub owner_token: String,
    /// The match module, built for WebAssembly.
    pub match_module: Vec<u8>,
}

/// What a match is to be.
pub struct MatchSpec<'a> {
    pub database: String,
    pub map: &'a LoadedMap,
    pub capacity: u16,
    /// The game: its rules and limits.
    pub rules: Rules,
    pub budget: u32,
    pub send_threads: usize,
    pub bind: SocketAddr,
    /// The bans in force, which the match starts with.
    pub bans: Vec<(Identity, String)>,
    /// The names players have chosen, which its roster shows.
    pub names: Vec<(Identity, String)>,
}

/// How a match ended, for the orchestration to act on and the log to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ended {
    /// Why: its score limit was reached, or its time is up.
    pub reason: &'static str,
    /// Who won.
    pub winner: String,
}

pub struct RunningMatch {
    pub database: String,
    /// The database's identity, hex: what the server's metrics are labelled with.
    pub database_identity: String,
    /// When the match began to count against its time limit: when it was
    /// announced to players, not when it was made ready.
    pub started: Instant,
    pub bind: SocketAddr,
    client: MatchClient,
    gateway: Option<Gateway>,
    /// What the figures were at the last log line.
    window: Window,
}

struct Window {
    at: Instant,
    gateway: StatsSnapshot,
    metrics: TickMetrics,
    rejected_total: u64,
    rejected_hits_total: u64,
}

/// What a match was doing over a window, for the log.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub seconds: f64,
    pub players: u32,
    pub capacity: u32,
    /// Mean milliseconds per tick (the whole call, then the module's part of
    /// it), and ticks the server ran in the window; `None` without metrics.
    pub tick_ms: Option<(f64, f64)>,
    pub ticks: f64,
    /// Bytes a second sent to players, headers included.
    pub out_bytes_per_second: f64,
    /// Moves the server rejected in the window, and since the match began.
    pub rejected: u64,
    pub rejected_total: u64,
    /// Hit reports the server refused in the window, and since the match began.
    pub rejected_hits: u64,
    pub rejected_hits_total: u64,
    /// Hit reports accepted since the match began, and those of the refused that the server judged
    /// to be of a target nowhere near where the shooter says they saw it (`Reject::TargetNotWhereSeen`).
    pub hits_total: u64,
    pub not_where_seen_total: u64,
    pub inputs_late: u64,
    pub inputs_unbound: u64,
    pub ticks_missed: u64,
    /// Sending a tick to every player (from the tick reaching the gateway to its last datagram
    /// sent) over the ticks of this report's window: the median and the slowest.
    pub send_ms_p50: f64,
    pub send_ms_max: f64,
}

impl RunningMatch {
    /// Publish the module as a new database, set the match up and start it,
    /// and put a gateway in front of it.
    pub fn start(env: &Env, spec: MatchSpec) -> Result<RunningMatch, String> {
        let database_identity = env.admin.publish(&spec.database, &env.match_module, &env.owner_token)?;
        let result = RunningMatch::set_up(env, &spec, database_identity);
        if result.is_err() {
            let _ = env.admin.delete(&spec.database, &env.owner_token);
        }
        result
    }

    fn set_up(env: &Env, spec: &MatchSpec, database_identity: String) -> Result<RunningMatch, String> {
        let client = connect_with_retry(env, &spec.database)?;
        client.load_map(spec.map.data.to_bytes())?;
        client.set_capacity(spec.capacity)?;
        client.set_game(&spec.rules)?;
        for (identity, reason) in &spec.bans {
            client.set_ban(*identity, reason)?;
        }
        for (identity, name) in &spec.names {
            client.set_name(*identity, name)?;
        }
        client.start_and_wait()?;

        let mut config = GatewayConfig::new(env.uri.as_str(), spec.database.as_str());
        config.token = Some(env.owner_token.clone());
        config.budget_bytes_per_second = spec.budget;
        config.send_threads = spec.send_threads;
        let transport = Arc::new(UdpTransport::bind(spec.bind).map_err(|e| format!("binding UDP {}: {e}", spec.bind))?);
        let gateway = Gateway::start(config, transport)?;
        let window = Window {
            at: Instant::now(),
            gateway: gateway.stats(),
            metrics: tick_metrics(env, &database_identity),
            rejected_total: 0,
            rejected_hits_total: 0,
        };
        Ok(RunningMatch {
            database: spec.database.clone(),
            database_identity,
            started: Instant::now(),
            bind: spec.bind,
            client,
            gateway: Some(gateway),
            window,
        })
    }

    /// Start the game's clock now (the time limit, the waves and the scores count
    /// from here): called when the match is announced to players.
    pub fn begin_game(&self) -> Result<(), String> {
        self.client.begin_game()
    }

    /// Why the game has ended, if it has, and who won, as the match's public
    /// `game_state` says.
    pub fn ended(&self) -> Option<Ended> {
        let game = self.client.game()?;
        let reason = match game.ending {
            0 => return None,
            1 => "its score limit was reached",
            _ => "its time is up",
        };
        let winner = match game.winner_kind {
            1 => {
                let name = self.client.roster().get(&game.winner).map(|r| r.name.clone());
                format!("won by {}", name.unwrap_or_else(|| format!("player {}", game.winner)))
            }
            2 => format!("won by the {} team", if game.winner == 0 { "red" } else { "blue" }),
            _ => "nobody won".to_string(),
        };
        Some(Ended { reason, winner })
    }

    /// Players in the match now (the tick marker's count).
    pub fn players(&self) -> u32 {
        self.client.marker().map_or(0, |m| m.players)
    }

    /// Whether the connection to the match is up.
    pub fn is_connected(&self) -> bool {
        self.client.conn.is_active()
    }

    /// Players the gateway holds a UDP session for.
    pub fn sessions(&self) -> usize {
        self.gateway.as_ref().map_or(0, |g| g.sessions())
    }

    pub fn set_ban(&self, identity: Identity, reason: &str) -> Result<(), String> {
        self.client.set_ban(identity, reason)
    }

    pub fn clear_ban(&self, identity: Identity) -> Result<(), String> {
        self.client.clear_ban(identity)
    }

    pub fn set_name(&self, identity: Identity, name: &str) -> Result<(), String> {
        self.client.set_name(identity, name)
    }

    /// What happened since the last report: tick time from the server's
    /// metrics, players, bandwidth, rejected moves and rejected hit reports.
    pub fn report(&mut self, env: &Env, capacity: u32) -> Report {
        let now = Instant::now();
        let gateway = self.gateway.as_ref().map(|g| g.stats());
        let metrics = tick_metrics(env, &self.database_identity);
        let marker = self.client.marker();
        let rejected_total = marker.as_ref().map_or(0, |m| m.rejected_total);
        let rejected_hits_total = marker.as_ref().map_or(0, |m| m.rejected_hits_total);
        let seconds = now.duration_since(self.window.at).as_secs_f64().max(1e-3);
        let mut report = Report {
            seconds,
            players: marker.as_ref().map_or(0, |m| m.players),
            capacity,
            rejected: rejected_total.saturating_sub(self.window.rejected_total),
            rejected_total,
            rejected_hits: rejected_hits_total.saturating_sub(self.window.rejected_hits_total),
            rejected_hits_total,
            hits_total: marker.as_ref().map_or(0, |m| m.hits_total),
            not_where_seen_total: marker.as_ref().map_or(0, |m| m.rejected_not_where_seen_total),
            ..Report::default()
        };
        let delta = metrics.since(&self.window.metrics);
        if delta.ticks > 0.0 {
            report.tick_ms = Some((delta.mean_ms_with_queries(), delta.mean_ms_wasm()));
            report.ticks = delta.ticks;
        }
        if let Some(stats) = &gateway {
            let before = &self.window.gateway;
            report.out_bytes_per_second = stats.wire_bytes_sent.saturating_sub(before.wire_bytes_sent) as f64 / seconds;
            report.inputs_late = stats.inputs_late.saturating_sub(before.inputs_late);
            report.inputs_unbound = stats.inputs_unbound.saturating_sub(before.inputs_unbound);
            report.ticks_missed = stats.ticks_skipped.saturating_sub(before.ticks_skipped);
        }
        if let Some(gateway) = &self.gateway {
            let window = gateway.take_send_window();
            report.send_ms_p50 = window.p50;
            report.send_ms_max = window.max;
        }
        self.window = Window {
            at: now,
            gateway: gateway.unwrap_or_else(|| self.window.gateway.clone()),
            metrics,
            rejected_total,
            rejected_hits_total,
        };
        report
    }

    /// Take the gateway down, leave the match and delete its database.
    pub fn stop(mut self, env: &Env) -> Result<(), String> {
        // the gateway first: it holds a connection to the database
        if let Some(mut gateway) = self.gateway.take() {
            gateway.stop();
        }
        let _ = self.client.conn.disconnect();
        env.admin.delete(&self.database, &env.owner_token)
    }
}

fn tick_metrics(env: &Env, database_identity: &str) -> TickMetrics {
    env.admin.metrics().map(|text| TickMetrics::parse(&text, Some(database_identity))).unwrap_or_default()
}

/// The owner's connection, retried for a few seconds: a database that has
/// just been published takes a moment to take connections.
fn connect_with_retry(env: &Env, database: &str) -> Result<MatchClient, String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match MatchClient::try_connect_as(&env.uri, database, Some(&env.owner_token)) {
            Ok(client) => return Ok(client),
            Err(e) if Instant::now() > deadline => return Err(e),
            Err(_) => std::thread::sleep(Duration::from_millis(250)),
        }
    }
}
