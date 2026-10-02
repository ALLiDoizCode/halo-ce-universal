//! One large-scale session as the client sees it: a UDP conversation with the
//! gateway for the per-tick state, and a direct SpacetimeDB connection for the
//! slow state, each on a thread of its own so that the game never waits for a
//! network.
//!
//! - The network thread says Hello until the gateway answers with a Welcome,
//!   then decodes every Snapshot into the newest state of each other player.
//!   Joining is this one function, `Session::join_step`, so that the gateway's
//!   handshake can change without touching the rest.
//! - The slow thread connects to SpacetimeDB (and connects again if the
//!   connection drops), subscribes to the map's bounds and this player's own
//!   row, and keeps them current.
//! - The game reads with [`Session::frame`] and [`Session::slow`] (copies,
//!   taken under a lock that the threads hold for a moment) and writes with
//!   [`Session::send_input`].
//!
//! The datagrams are the gateway's own: everything is decoded and encoded by
//! `halo-wire`.

use std::collections::BTreeMap;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use halo_match_driver::module_bindings::{DbConnection, MapInfo, MapInfoTableAccess, PlayerTableAccess};
use halo_match_driver::PlayerRow;
use halo_sim::PlayerInput;
use halo_wire::datagram::{seq_newer, ClientMessage, ServerMessage, Welcome, MAX_DATAGRAM};
use halo_wire::unit::{Bounds, UnitState};
use spacetimedb_sdk::{Compression, DbContext, Table, TableWithPrimaryKey};

/// How often a player who has not been welcomed says Hello again.
const HELLO_INTERVAL: Duration = Duration::from_millis(200);
/// How long the network thread waits for a datagram before it looks at the
/// clock and the stop flag.
const POLL: Duration = Duration::from_millis(50);
/// How long the slow thread waits before it connects again.
const RECONNECT_DELAY: Duration = Duration::from_secs(1);

/// Where a session connects, and as whom.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// The gateway's UDP address, `host:port`.
    pub gateway: String,
    /// SpacetimeDB's URI, such as `http://127.0.0.1:3000`.
    pub spacetimedb: String,
    /// The match's database.
    pub database: String,
    /// The player this client plays: it must be in the match already.
    pub player: u16,
}

/// The newest state the gateway sent of one other player.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RemoteUnit {
    pub state: UnitState,
    /// The tick of the datagram that carried it.
    pub tick: u32,
}

/// The other players as of now, sorted by player id.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Frame {
    /// The newest tick any datagram has carried; 0 before the first.
    pub tick: u32,
    pub units: Vec<RemoteUnit>,
}

/// What comes over the direct SpacetimeDB connection.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Slow {
    /// The subscription is applied and the connection is up.
    pub connected: bool,
    /// How many maps the match has loaded; 0 before the first. A change
    /// means a new match on a new map.
    pub map_version: u64,
    /// The map's world bounds.
    pub bounds: Option<Bounds>,
    /// This player as the server holds it: `x y z yaw pitch`.
    pub local: Option<[f32; 5]>,
}

/// Counters, for logs and tests.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Counters {
    pub hellos_sent: u32,
    pub datagrams: u64,
    pub bytes: u64,
    /// Datagrams that did not decode or were not for this player.
    pub ignored: u64,
    pub inputs_sent: u64,
}

#[derive(Default)]
struct Shared {
    welcome: Option<Welcome>,
    units: BTreeMap<u16, RemoteUnit>,
    newest_tick: u32,
    counters: Counters,
    slow: Slow,
    /// The last thing that went wrong on a thread, for the log.
    error: Option<String>,
}

struct Inner {
    config: Config,
    gateway: SocketAddr,
    socket: UdpSocket,
    shared: Mutex<Shared>,
    seq: AtomicU32,
    stop: AtomicBool,
    /// The SpacetimeDB connection, for the thread that stops the session to
    /// close.
    connection: Mutex<Option<DbConnection>>,
}

impl Inner {
    fn shared(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn send(&self, message: &ClientMessage) {
        // a send that fails (no route yet, a full buffer) is a datagram lost
        let _ = self.socket.send(&message.encode());
    }
}

pub struct Session {
    inner: Arc<Inner>,
    threads: Vec<JoinHandle<()>>,
}

impl Session {
    /// Open the socket and start both threads; returns at once, whether or not
    /// anything is reachable yet (see [`Session::joined`] and [`Slow`]). An
    /// error only for a gateway address that is no address.
    pub fn start(config: Config) -> Result<Session, String> {
        let gateway = config
            .gateway
            .to_socket_addrs()
            .map_err(|e| format!("the gateway address {:?}: {e}", config.gateway))?
            .next()
            .ok_or_else(|| format!("the gateway address {:?} names no host", config.gateway))?;
        let any: SocketAddr = if gateway.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" }.parse().unwrap();
        let socket = UdpSocket::bind(any).map_err(|e| format!("a UDP socket: {e}"))?;
        // only the gateway's datagrams are read
        socket.connect(gateway).map_err(|e| format!("the gateway {gateway}: {e}"))?;
        socket.set_read_timeout(Some(POLL)).map_err(|e| format!("a UDP socket: {e}"))?;
        let inner = Arc::new(Inner {
            config,
            gateway,
            socket,
            shared: Mutex::new(Shared::default()),
            seq: AtomicU32::new(1),
            stop: AtomicBool::new(false),
            connection: Mutex::new(None),
        });
        let network = {
            let inner = inner.clone();
            std::thread::Builder::new()
                .name("halo-large-udp".into())
                .spawn(move || network_thread(&inner))
                .map_err(|e| format!("a thread: {e}"))?
        };
        let slow = {
            let inner = inner.clone();
            std::thread::Builder::new()
                .name("halo-large-slow".into())
                .spawn(move || slow_thread(&inner))
                .map_err(|e| format!("a thread: {e}"))?
        };
        Ok(Session { inner, threads: vec![network, slow] })
    }

    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    pub fn gateway(&self) -> SocketAddr {
        self.inner.gateway
    }

    /// The gateway has welcomed this player: the states of other players flow.
    pub fn joined(&self) -> bool {
        self.inner.shared().welcome.is_some()
    }

    /// The map's world bounds as the gateway's Welcome gave them, which the
    /// states are packed against.
    pub fn welcome(&self) -> Option<Welcome> {
        self.inner.shared().welcome
    }

    pub fn counters(&self) -> Counters {
        self.inner.shared().counters
    }

    pub fn slow(&self) -> Slow {
        self.inner.shared().slow
    }

    /// The last thing that went wrong on a network thread, if anything.
    pub fn last_error(&self) -> Option<String> {
        self.inner.shared().error.clone()
    }

    /// The newest state of every other player the gateway has sent.
    pub fn frame(&self) -> Frame {
        let shared = self.inner.shared();
        Frame { tick: shared.newest_tick, units: shared.units.values().copied().collect() }
    }

    /// Tell the gateway where this player is now; each call is the next
    /// input. Not sent (false) before the gateway has welcomed the player.
    pub fn send_input(&self, position: [f32; 3], yaw: f32, pitch: f32) -> bool {
        if !self.joined() {
            return false;
        }
        let seq = self.inner.seq.fetch_add(1, Relaxed);
        let input = PlayerInput { player: self.inner.config.player, position, yaw, pitch };
        self.inner.send(&ClientMessage::Input { seq, input });
        self.inner.shared().counters.inputs_sent += 1;
        true
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.inner.stop.store(true, Relaxed);
        if let Some(connection) = self.inner.connection.lock().unwrap_or_else(|p| p.into_inner()).take() {
            let _ = connection.disconnect();
        }
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

/// The datagrams: Hello until welcomed, then Snapshots.
fn network_thread(inner: &Inner) {
    let mut buffer = vec![0u8; MAX_DATAGRAM + 64];
    let mut last_hello: Option<Instant> = None;
    while !inner.stop.load(Relaxed) {
        let welcomed = inner.shared().welcome.is_some();
        if !welcomed && last_hello.is_none_or(|at| at.elapsed() >= HELLO_INTERVAL) {
            last_hello = Some(Instant::now());
            inner.shared().counters.hellos_sent += 1;
            inner.send(&join_step(inner.config.player));
        }
        match inner.socket.recv(&mut buffer) {
            Ok(length) => handle(inner, &buffer[..length]),
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            // an ICMP port unreachable from a gateway that is not up yet shows as
            // an error on the next read of a connected socket: try again soon
            Err(e) => {
                inner.shared().error = Some(format!("UDP: {e}"));
                std::thread::sleep(POLL);
            }
        }
    }
}

/// What a player sends to be let in. The whole of the join, on this side:
/// the gateway's answer is a [`ServerMessage::Welcome`], taken in `handle`.
fn join_step(player: u16) -> ClientMessage {
    ClientMessage::Hello { player }
}

fn handle(inner: &Inner, datagram: &[u8]) {
    let mut shared = inner.shared();
    shared.counters.datagrams += 1;
    shared.counters.bytes += datagram.len() as u64;
    match ServerMessage::decode(datagram) {
        Some(ServerMessage::Welcome(welcome)) if welcome.player == inner.config.player => {
            shared.welcome = Some(welcome);
        }
        Some(ServerMessage::Snapshot(snapshot)) => {
            // states are packed against the bounds of the Welcome: none yet,
            // none to read
            let Some(welcome) = shared.welcome else {
                shared.counters.ignored += 1;
                return;
            };
            if shared.newest_tick == 0 || seq_newer(snapshot.tick, shared.newest_tick) {
                shared.newest_tick = snapshot.tick;
            }
            for packed in &snapshot.states {
                let state = packed.unpack(&welcome.bounds);
                if state.player == inner.config.player {
                    continue;
                }
                let newer = shared.units.get(&state.player).is_none_or(|known| seq_newer(snapshot.tick, known.tick));
                if newer {
                    shared.units.insert(state.player, RemoteUnit { state, tick: snapshot.tick });
                }
            }
        }
        _ => shared.counters.ignored += 1,
    }
}

/// The direct SpacetimeDB connection: connect, subscribe, keep the slow state
/// current, and connect again if it drops.
fn slow_thread(inner: &Arc<Inner>) {
    while !inner.stop.load(Relaxed) {
        match connect(inner) {
            Ok(connection) => {
                // the connection's own thread runs the callbacks until it ends
                let runner = connection.run_threaded();
                *inner.connection.lock().unwrap_or_else(|p| p.into_inner()) = Some(connection);
                while !inner.stop.load(Relaxed) && !runner.is_finished() {
                    std::thread::sleep(POLL);
                }
                if let Some(connection) = inner.connection.lock().unwrap_or_else(|p| p.into_inner()).take() {
                    let _ = connection.disconnect();
                }
                let _ = runner.join();
                inner.shared().slow.connected = false;
            }
            Err(e) => inner.shared().error = Some(e),
        }
        let until = Instant::now() + RECONNECT_DELAY;
        while !inner.stop.load(Relaxed) && Instant::now() < until {
            std::thread::sleep(POLL);
        }
    }
}

fn connect(inner: &Arc<Inner>) -> Result<DbConnection, String> {
    let connection = DbConnection::builder()
        .with_uri(inner.config.spacetimedb.as_str())
        .with_database_name(inner.config.database.as_str())
        // the server is usually this machine's or a neighbour's, and the
        // slow state is small
        .with_compression(Compression::None)
        .build()
        .map_err(|e| format!("SpacetimeDB {}: {e}", inner.config.spacetimedb))?;

    let map = inner.clone();
    let on_map = move |info: &MapInfo| {
        let mut shared = map.shared();
        shared.slow.map_version = info.version;
        shared.slow.bounds = Some(Bounds::from_world([info.x_0, info.x_1, info.y_0, info.y_1, info.z_0, info.z_1]));
    };
    let table = connection.db.map_info();
    let on_map_insert = on_map.clone();
    table.on_insert(move |_, info| on_map_insert(info));
    table.on_update(move |_, _, info| on_map(info));

    let own = inner.clone();
    let on_player = move |row: &PlayerRow| {
        if row.id == own.config.player {
            own.shared().slow.local = Some([row.x, row.y, row.z, row.yaw, row.pitch]);
        }
    };
    let table = connection.db.player();
    let on_player_insert = on_player.clone();
    table.on_insert(move |_, row| on_player_insert(row));
    table.on_update(move |_, _, row| on_player(row));

    let applied = inner.clone();
    let failed = inner.clone();
    // the map's bounds and this player's own row: the rest of the players
    // reach the client by UDP, and a subscription to all of them would send
    // every client every player's every move
    connection
        .subscription_builder()
        .on_applied(move |_| applied.shared().slow.connected = true)
        .on_error(move |_, e| failed.shared().error = Some(format!("the subscription: {e}")))
        .subscribe([
            "SELECT * FROM map_info".to_string(),
            format!("SELECT * FROM player WHERE id = {}", inner.config.player),
        ]);
    Ok(connection)
}
