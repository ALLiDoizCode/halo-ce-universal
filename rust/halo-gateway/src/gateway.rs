//! The gateway: one connection to the match module on SpacetimeDB, and UDP
//! (or whatever [`Transport`] it is given) to the players.
//!
//! Three kinds of thread:
//!
//! - The SpacetimeDB connection's thread. Each time a tick completes (the
//!   `match_tick` row updates, and the tick's `player` rows arrived with it)
//!   it hands the module every input that arrived since the last tick as
//!   one `submit_inputs` call, then packs the tick's states and hands the
//!   work to the sending threads. It never sends.
//! - One receiving thread: Hellos bind addresses to players, Inputs go onto
//!   the board (newest wins, late ones are dropped).
//! - `send_threads` sending threads, each owning the recipients whose id
//!   is congruent to its index and each recipient's [`Planner`].
//!
//! Inputs are submitted the moment a tick completes, so at most one batch
//! reaches the module per tick, and an input that arrives during tick N is
//! applied by tick N+1 or N+2.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use halo_match_driver::module_bindings::*;
use halo_match_driver::now_us;
use halo_sim::wire::encode_inputs;
use halo_sim::TICKS_PER_SECOND;
use halo_wire::datagram::{ClientMessage, Welcome, IP_UDP_OVERHEAD, MAX_DATAGRAM};
use halo_wire::planner::{Entry, Observer, Planner, PlannerConfig};
use halo_wire::unit::{Bounds, PackedState, UnitState};
use spacetimedb_sdk::{Compression, DbContext, Table, TableWithPrimaryKey};

use crate::board::{Bound, InputBoard, Sessions};
use crate::stats::{Stats, StatsSnapshot};
use crate::transport::Transport;

#[derive(Debug, Clone)]
pub struct GatewayConfig {
    /// The SpacetimeDB server, such as `http://127.0.0.1:3000`.
    pub spacetimedb_uri: String,
    /// The match's database.
    pub database: String,
    /// What a player may be sent, in bytes a second counting IP and UDP headers.
    pub budget_bytes_per_second: u32,
    /// Threads that send; the recipients are divided among them.
    pub send_threads: usize,
    /// The priority rule's numbers; its budget is replaced by `budget_bytes_per_second`.
    pub planner: PlannerConfig,
}

impl GatewayConfig {
    pub fn new(spacetimedb_uri: impl Into<String>, database: impl Into<String>) -> GatewayConfig {
        GatewayConfig {
            spacetimedb_uri: spacetimedb_uri.into(),
            database: database.into(),
            budget_bytes_per_second: 90_000,
            send_threads: 4,
            planner: PlannerConfig::with_budget(90_000),
        }
    }
}

/// Everything the threads share.
struct Shared {
    sessions: RwLock<Sessions>,
    board: Mutex<InputBoard>,
    bounds: RwLock<Option<Bounds>>,
    /// Ticks handed to the sending threads and not yet finished.
    in_flight: AtomicUsize,
    stop: AtomicBool,
    stats: Stats,
}

/// One tick's states, packed once and read by every sending thread.
struct TickWork {
    tick: u32,
    entries: Vec<Entry>,
    /// Where each entry's player faces.
    facing: Vec<(f32, f32)>,
    /// Index into `entries` by player id.
    index: HashMap<u16, usize>,
}

/// A tick being sent: finished when every sending thread has done its share.
struct Job {
    work: TickWork,
    started: Instant,
    remaining: AtomicUsize,
}

pub struct Gateway {
    shared: Arc<Shared>,
    local_addr: SocketAddr,
    conn: Option<Arc<DbConnection>>,
    recv: Option<JoinHandle<()>>,
    threads: Vec<JoinHandle<()>>,
}

impl Gateway {
    /// Connect to the match, subscribe, and start serving `transport`.
    pub fn start(config: GatewayConfig, transport: Arc<dyn Transport>) -> Result<Gateway, String> {
        assert!(config.send_threads >= 1, "at least one sending thread");
        let shared = Arc::new(Shared {
            sessions: RwLock::new(Sessions::default()),
            board: Mutex::new(InputBoard::default()),
            bounds: RwLock::new(None),
            in_flight: AtomicUsize::new(0),
            stop: AtomicBool::new(false),
            stats: Stats::new(config.send_threads),
        });
        let mut planner_config = config.planner;
        planner_config.budget_bytes_per_second = config.budget_bytes_per_second;

        // sending threads
        let mut threads = Vec::new();
        let mut senders: Vec<Sender<Arc<Job>>> = Vec::new();
        for index in 0..config.send_threads {
            let (tx, rx) = mpsc::channel();
            senders.push(tx);
            let (shared, transport) = (shared.clone(), transport.clone());
            let modulus = config.send_threads;
            threads.push(
                std::thread::Builder::new()
                    .name(format!("gateway-send-{index}"))
                    .spawn(move || send_loop(index, modulus, planner_config, rx, &shared, &*transport))
                    .map_err(|e| e.to_string())?,
            );
        }

        // the connection to the match
        let conn = DbConnection::builder()
            .with_uri(config.spacetimedb_uri.as_str())
            .with_database_name(config.database.as_str())
            // on loopback compression only costs CPU; and the match's ticks
            // need not wait for the database's log to reach the disk
            .with_compression(Compression::None)
            .with_confirmed_reads(false)
            .build()
            .map_err(|e| format!("connecting to SpacetimeDB: {e}"))?;
        {
            let on_insert = shared.clone();
            conn.db.map_info().on_insert(move |_, row| set_bounds(&on_insert, row));
            let on_update = shared.clone();
            conn.db.map_info().on_update(move |_, _, row| set_bounds(&on_update, row));
        }
        {
            let shared = shared.clone();
            let mut previous: HashMap<u16, ([f32; 3], u64)> = HashMap::new();
            let mut last_tick = 0u64;
            conn.db.match_tick().on_update(move |ctx, _old, marker| {
                let started = Instant::now();
                let tick = marker.tick;
                let stats = &shared.stats;
                stats.ticks.fetch_add(1, Relaxed);
                if last_tick != 0 && tick > last_tick + 1 {
                    stats.ticks_skipped.fetch_add(tick - last_tick - 1, Relaxed);
                }
                last_tick = tick;
                stats.record_arrival(now_us() - marker.stamped_us);

                // the batch first: the sooner the module has it the better
                let inputs = shared.board.lock().unwrap().take_fresh();
                if !inputs.is_empty() {
                    stats.batches_submitted.fetch_add(1, Relaxed);
                    stats.inputs_submitted.fetch_add(inputs.len() as u64, Relaxed);
                    if let Err(e) = ctx.reducers.submit_inputs(encode_inputs(&inputs)) {
                        eprintln!("gateway: submit_inputs: {e}");
                    }
                }

                let Some(bounds) = *shared.bounds.read().unwrap() else { return };
                let mut rows: Vec<PlayerRow> = ctx.db.player().iter().collect();
                rows.sort_unstable_by_key(|r| r.id);
                let mut work = TickWork {
                    tick: tick as u32,
                    entries: Vec::with_capacity(rows.len()),
                    facing: Vec::with_capacity(rows.len()),
                    index: HashMap::with_capacity(rows.len()),
                };
                for row in &rows {
                    let position = [row.x, row.y, row.z];
                    let velocity = match previous.get(&row.id) {
                        Some((before, was)) if row.updated_tick > *was => {
                            let dt = (row.updated_tick - was) as f32 / TICKS_PER_SECOND as f32;
                            [
                                (position[0] - before[0]) / dt,
                                (position[1] - before[1]) / dt,
                                (position[2] - before[2]) / dt,
                            ]
                        }
                        _ => [0.0; 3],
                    };
                    previous.insert(row.id, (position, row.updated_tick));
                    let state = UnitState {
                        player: row.id,
                        position,
                        velocity,
                        yaw: row.yaw,
                        pitch: row.pitch,
                        tick: row.updated_tick as u8,
                        flags: 0,
                    };
                    work.index.insert(row.id, work.entries.len());
                    work.entries.push(Entry { position, packed: PackedState::pack(&state, &bounds) });
                    work.facing.push((row.yaw, row.pitch));
                }
                if previous.len() > rows.len() {
                    previous.retain(|id, _| work.index.contains_key(id));
                }
                let job = Arc::new(Job { work, started, remaining: AtomicUsize::new(senders.len()) });
                if shared.in_flight.fetch_add(1, Relaxed) > 0 {
                    stats.send_overruns.fetch_add(1, Relaxed);
                }
                for tx in &senders {
                    let _ = tx.send(job.clone());
                }
            });
        }
        let (applied_tx, applied) = mpsc::channel();
        conn.subscription_builder()
            .on_applied(move |_| {
                let _ = applied_tx.send(());
            })
            .on_error(|_, err| eprintln!("gateway: subscription error: {err}"))
            .subscribe(["SELECT * FROM match_tick", "SELECT * FROM player", "SELECT * FROM map_info"]);
        conn.run_threaded();
        applied.recv_timeout(Duration::from_secs(30)).map_err(|_| "the subscription did not apply".to_string())?;
        let conn = Arc::new(conn);

        // the receiving thread
        let recv;
        {
            let (shared, conn, transport) = (shared.clone(), conn.clone(), transport.clone());
            recv = Some(
                std::thread::Builder::new()
                    .name("gateway-recv".into())
                    .spawn(move || recv_loop(&shared, &conn, &*transport))
                    .map_err(|e| e.to_string())?,
            );
        }
        let local_addr = transport.local_addr();
        Ok(Gateway { shared, local_addr, conn: Some(conn), recv, threads })
    }

    /// Where players send their datagrams.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn stats(&self) -> StatsSnapshot {
        self.shared.stats.snapshot()
    }

    /// Players bound to an address now.
    pub fn sessions(&self) -> usize {
        self.shared.sessions.read().unwrap().len()
    }

    /// Stop every thread and disconnect.
    pub fn stop(&mut self) {
        self.shared.stop.store(true, Relaxed);
        if let Some(conn) = self.conn.take() {
            let _ = conn.disconnect();
            // the receiving thread holds the last other handle on the connection,
            // which owns the senders the sending threads wait on: join it first
            if let Some(recv) = self.recv.take() {
                let _ = recv.join();
            }
            drop(conn);
        }
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        self.stop();
    }
}

fn set_bounds(shared: &Shared, row: &MapInfo) {
    *shared.bounds.write().unwrap() = Some(Bounds::from_world([row.x_0, row.x_1, row.y_0, row.y_1, row.z_0, row.z_1]));
}

fn recv_loop(shared: &Shared, conn: &DbConnection, transport: &dyn Transport) {
    let stats = &shared.stats;
    let mut buf = vec![0u8; MAX_DATAGRAM + 64];
    while !shared.stop.load(Relaxed) {
        let (len, from) = match transport.recv(&mut buf) {
            Ok(Some(got)) => got,
            Ok(None) => continue,
            Err(e) => {
                eprintln!("gateway: receive: {e}");
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
        };
        match ClientMessage::decode(&buf[..len]) {
            Some(ClientMessage::Hello { player }) => {
                stats.hellos.fetch_add(1, Relaxed);
                let bounds = *shared.bounds.read().unwrap();
                // only a player the match has, and only once the map's bounds are known
                let (true, Some(bounds)) = (conn.db.player().id().find(&player).is_some(), bounds) else { continue };
                let bound = shared.sessions.write().unwrap().bind(from, player);
                if bound != Bound::Again {
                    // a fresh session numbers its inputs from the start again
                    shared.board.lock().unwrap().forget(player);
                }
                let tick = conn.db.match_tick().iter().next().map_or(0, |t| t.tick) as u32;
                let welcome = Welcome { player, tick, bounds };
                let _ = transport.send_to(&welcome.encode(), from);
            }
            Some(ClientMessage::Input { seq, input }) => {
                stats.inputs_received.fetch_add(1, Relaxed);
                if shared.sessions.read().unwrap().player_at(from) != Some(input.player) {
                    stats.inputs_unbound.fetch_add(1, Relaxed);
                } else if !shared.board.lock().unwrap().offer(seq, input) {
                    stats.inputs_late.fetch_add(1, Relaxed);
                }
            }
            None => {
                stats.malformed.fetch_add(1, Relaxed);
            }
        }
    }
}

fn send_loop(
    index: usize,
    modulus: usize,
    config: PlannerConfig,
    jobs: Receiver<Arc<Job>>,
    shared: &Shared,
    transport: &dyn Transport,
) {
    let stats = &shared.stats;
    let mut planners: HashMap<u16, Planner> = HashMap::new();
    while let Ok(job) = jobs.recv() {
        let recipients = shared.sessions.read().unwrap().partition(index, modulus);
        planners.retain(|id, _| recipients.iter().any(|(r, _)| r == id));
        let work = &job.work;
        let (mut datagrams, mut bytes, mut states) = (0u64, 0u64, 0u64);
        for (player, addr) in recipients {
            let Some(&at) = work.index.get(&player) else { continue };
            let me = &work.entries[at];
            let (yaw, pitch) = work.facing[at];
            let observer = Observer { player, position: me.position, yaw, pitch };
            let plan = planners.entry(player).or_insert_with(|| Planner::new(config)).plan(
                &observer,
                &work.entries,
                work.tick,
            );
            states += plan.states as u64;
            for datagram in &plan.datagrams {
                match transport.send_to(datagram, addr) {
                    Ok(()) => {
                        datagrams += 1;
                        bytes += (datagram.len() + IP_UDP_OVERHEAD) as u64;
                    }
                    Err(_) => {
                        stats.send_errors.fetch_add(1, Relaxed);
                    }
                }
            }
        }
        stats.datagrams_sent.fetch_add(datagrams, Relaxed);
        stats.wire_bytes_sent.fetch_add(bytes, Relaxed);
        stats.states_sent.fetch_add(states, Relaxed);
        stats.per_thread_datagrams[index].fetch_add(datagrams, Relaxed);
        if job.remaining.fetch_sub(1, Relaxed) == 1 {
            stats.record_send(job.started.elapsed().as_micros() as u32);
            shared.in_flight.fetch_sub(1, Relaxed);
        }
    }
}
