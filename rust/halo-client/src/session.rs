//! One large-scale session as the client sees it: a direct SpacetimeDB
//! connection, which takes the player's seat and carries the slow state, and a
//! UDP conversation with the gateway for the per-tick state, each on a thread
//! of its own so that the game never waits for a network.
//!
//! - The slow thread connects to SpacetimeDB (and again if the connection
//!   drops, as the same identity, so the seat comes back), calls the match's
//!   `join` with the public key of this session's Ed25519 key pair, and reads
//!   the player id off its `seat` row. It also keeps the map's bounds and the
//!   player's own row current, and the roster: who each player of the match
//!   is (a name and a team), which changes only when someone joins or leaves.
//! - The network thread, once there is a player id, joins over UDP (`join_step`,
//!   the one place that knows the handshake: Hello, Challenge, Auth, Welcome),
//!   then decodes every Snapshot into the newest state of each other player,
//!   acknowledges Snapshots in every Input, and keeps the session alive with an
//!   Input when the game sends none. If the gateway goes silent it joins again.
//! - The game reads with [`Session::frame`] and [`Session::slow`] (copies,
//!   taken under a lock that the threads hold for a moment) and writes with
//!   [`Session::send_input`].
//!
//! The datagrams and the proof are the gateway's own: everything is decoded
//! and encoded by `halo-wire`.

use std::collections::BTreeMap;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use halo_match_driver::module_bindings::{
    join, leave, DbConnection, MapInfo, MapInfoTableAccess, PlayerTableAccess, RemoteReducers, RosterRow,
    RosterTableAccess, Seat, SeatTableAccess,
};
use halo_match_driver::PlayerRow;
use halo_sim::PlayerInput;
use halo_wire::auth::{self, CHALLENGE_LIFETIME_US, SEED_SIZE};
use halo_wire::datagram::{
    seq_newer, Ack, Challenge, ClientMessage, ServerMessage, Welcome, MAX_DATAGRAM, REFUSED_BAD_PROOF, REFUSED_NO_SEAT,
    REFUSED_STALE,
};
use halo_wire::unit::{Bounds, UnitState};
use spacetimedb_sdk::{Compression, DbContext, Table, TableWithPrimaryKey};

/// How often the join step is repeated until it gets its answer.
const JOIN_INTERVAL: Duration = Duration::from_millis(200);
/// How long the network thread waits for a datagram before it looks at the
/// clock and the stop flag.
const POLL: Duration = Duration::from_millis(50);
/// How long without an Input from the game before the library sends one: the
/// gateway lets go of an address that is silent for 10 s, and a game that is
/// loading sends none.
const KEEPALIVE: Duration = Duration::from_millis(100);
/// How long without a Snapshot before the gateway is taken to have let go of
/// the address (every player is sent one every tick).
const SILENCE: Duration = Duration::from_secs(3);
/// How long the slow thread waits before it connects (or joins) again.
const RETRY_DELAY: Duration = Duration::from_secs(1);
/// How long stopping a session waits for the module to take the player out.
const LEAVE_WAIT: Duration = Duration::from_millis(300);

/// How many ticks without a state of a player (the gateway sends every player at
/// least every `STALENESS_BOUND_TICKS` ticks, loss aside) before the player is
/// out of range: not in the [`Frame`], until the gateway sends them again.
pub const OUT_OF_RANGE_TICKS: u32 = 3 * halo_wire::planner::STALENESS_BOUND_TICKS;

/// Where a session connects.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// The gateway's UDP address, `host:port`.
    pub gateway: String,
    /// SpacetimeDB's URI, such as `http://127.0.0.1:3000`.
    pub spacetimedb: String,
    /// The match's database.
    pub database: String,
    /// The SpacetimeDB identity to be, as the token the server gave it
    /// before; `None` makes a new one. (A session keeps the token of the
    /// identity it got, to come back as it after a dropped connection.)
    pub token: Option<String>,
}

/// The newest state the gateway sent of one other player.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RemoteUnit {
    pub state: UnitState,
    /// The tick of the datagram that carried it.
    pub tick: u32,
}

/// Who a player is, from the match's roster.
#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    /// The engine's number for the team: 0 red, 1 blue.
    pub team: u8,
    pub name: String,
}

/// The other players as of now, sorted by player id: those in range. A player
/// is in range while they are on the match's roster (one the gateway has sent
/// but the roster does not hold yet is not shown, and one who has left the
/// match is gone at once) and the gateway has sent a state of them within
/// [`OUT_OF_RANGE_TICKS`] of the newest tick; one the gateway sends again is
/// back.
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
    /// Hellos and Auths sent.
    pub joins_sent: u32,
    pub datagrams: u64,
    pub bytes: u64,
    /// Datagrams that did not decode.
    pub ignored: u64,
    pub inputs_sent: u64,
    /// Times the gateway refused the player.
    pub refused: u32,
    /// Times the gateway fell silent and the player joined again.
    pub rejoins: u32,
}

#[derive(Default)]
struct Shared {
    /// The player this session is, from the seat the module gave.
    player: Option<u16>,
    welcome: Option<Welcome>,
    /// The gateway's challenge to answer, and when it came.
    challenge: Option<(Challenge, Instant)>,
    units: BTreeMap<u16, RemoteUnit>,
    /// Everyone in the match, from the direct connection.
    roster: BTreeMap<u16, Member>,
    newest_tick: u32,
    /// The Snapshots received, which every Input says.
    ack: Ack,
    last_snapshot: Option<Instant>,
    /// The latest Input, and when it was sent: what the keepalive repeats.
    last_input: Option<([f32; 3], f32, f32)>,
    last_input_at: Option<Instant>,
    counters: Counters,
    slow: Slow,
    /// The last thing that went wrong on a thread, for the log.
    error: Option<String>,
}

struct Inner {
    config: Config,
    gateway: SocketAddr,
    socket: UdpSocket,
    /// The secret of this session's UDP key pair, whose public key is on the seat.
    seed: [u8; SEED_SIZE],
    shared: Mutex<Shared>,
    seq: AtomicU32,
    stop: AtomicBool,
    /// The token of the identity the SpacetimeDB connection has, to connect
    /// as it again.
    token: Mutex<Option<String>>,
    /// The SpacetimeDB connection, for the thread that stops the session to
    /// leave and close.
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

    /// Send an Input saying where the player is, with what has been received.
    fn send_input(&self, shared: &mut Shared, player: u16, position: [f32; 3], yaw: f32, pitch: f32) {
        let seq = self.seq.fetch_add(1, Relaxed);
        let input = PlayerInput { player, position, yaw, pitch };
        self.send(&ClientMessage::Input { seq, input, ack: shared.ack });
        shared.last_input = Some((position, yaw, pitch));
        shared.last_input_at = Some(Instant::now());
        shared.counters.inputs_sent += 1;
    }
}

pub struct Session {
    inner: Arc<Inner>,
    network: Option<JoinHandle<()>>,
    // never joined: it can be inside a connect to a SpacetimeDB that does not
    // answer, and stopping a session must not wait for that (the game calls it
    // between maps). It sees the stop flag when the connect returns and leaves.
    _slow: JoinHandle<()>,
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
        let mut seed = [0u8; SEED_SIZE];
        getrandom::fill(&mut seed).map_err(|e| format!("random numbers for a key: {e}"))?;
        let inner = Arc::new(Inner {
            token: Mutex::new(config.token.clone()),
            config,
            gateway,
            socket,
            seed,
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
        Ok(Session { inner, network: Some(network), _slow: slow })
    }

    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    pub fn gateway(&self) -> SocketAddr {
        self.inner.gateway
    }

    /// The player this session is, once the match has given it a seat.
    pub fn player(&self) -> Option<u16> {
        self.inner.shared().player
    }

    /// The gateway has welcomed the player: the states of other players flow.
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

    /// The newest state of every other player in range.
    pub fn frame(&self) -> Frame {
        frame_of(&self.inner.shared())
    }

    /// Who a player is, if the roster has them.
    pub fn member(&self, player: u16) -> Option<Member> {
        self.inner.shared().roster.get(&player).cloned()
    }

    /// Tell the gateway where this player is now; each call is the next
    /// input. Not sent (false) before the gateway has welcomed the player.
    pub fn send_input(&self, position: [f32; 3], yaw: f32, pitch: f32) -> bool {
        let mut shared = self.inner.shared();
        let (Some(player), Some(_)) = (shared.player, shared.welcome) else { return false };
        self.inner.send_input(&mut shared, player, position, yaw, pitch);
        true
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.inner.stop.store(true, Relaxed);
        let connection = self.inner.connection.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(connection) = connection {
            // out of the match, as a player who leaves is, not held for a return
            if self.inner.shared().player.is_some() {
                let (done, left) = mpsc::channel();
                if connection
                    .reducers
                    .leave_then(move |_, _| {
                        let _ = done.send(());
                    })
                    .is_ok()
                {
                    let _ = left.recv_timeout(LEAVE_WAIT);
                }
            }
            let _ = connection.disconnect();
        }
        // (the socket's read times out in POLL)
        if let Some(network) = self.network.take() {
            let _ = network.join();
        }
    }
}

/// The players in range as of `shared`.
fn frame_of(shared: &Shared) -> Frame {
    let newest = shared.newest_tick;
    let in_range = |unit: &&RemoteUnit| {
        shared.roster.contains_key(&unit.state.player) && (newest.wrapping_sub(unit.tick) as i32) <= OUT_OF_RANGE_TICKS as i32
    };
    Frame { tick: newest, units: shared.units.values().filter(in_range).copied().collect() }
}

/// What a player sends to be let in, given what has come: Hello, and once the
/// gateway has challenged, the Auth that answers it. The whole of the join on
/// this side (`handle` takes the answers), so that the handshake can change
/// without touching the rest.
fn join_step(seed: &[u8; SEED_SIZE], player: u16, challenge: Option<&Challenge>) -> ClientMessage {
    match challenge {
        None => ClientMessage::Hello { player },
        Some(c) => ClientMessage::Auth {
            player,
            stamp: c.stamp,
            cookie: c.cookie,
            signature: auth::sign_challenge(seed, player, c.stamp, &c.cookie),
        },
    }
}

/// The datagrams: the join step until welcomed, then Snapshots, and Inputs
/// the game did not send.
fn network_thread(inner: &Inner) {
    let mut buffer = vec![0u8; MAX_DATAGRAM + 64];
    let mut last_join = Instant::now() - JOIN_INTERVAL;
    while !inner.stop.load(Relaxed) {
        maintain(inner, &mut last_join);
        match inner.socket.recv(&mut buffer) {
            Ok(length) => handle(inner, &buffer[..length], &mut last_join),
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

/// What the thread does when time has passed: repeat the join step, send the
/// keepalive Input, notice a gateway that has gone quiet.
fn maintain(inner: &Inner, last_join: &mut Instant) {
    let mut shared = inner.shared();
    let Some(player) = shared.player else { return };
    if shared.welcome.is_some() {
        if shared.last_snapshot.is_some_and(|at| at.elapsed() > SILENCE) {
            // the gateway has let go of the address: join again, keeping what is known of the others
            shared.welcome = None;
            shared.challenge = None;
            shared.counters.rejoins += 1;
        } else {
            if shared.last_input_at.is_none_or(|at| at.elapsed() >= KEEPALIVE) {
                let at = shared.last_input.or(shared.slow.local.map(|l| ([l[0], l[1], l[2]], l[3], l[4])));
                if let Some((position, yaw, pitch)) = at {
                    inner.send_input(&mut shared, player, position, yaw, pitch);
                }
            }
            return;
        }
    }
    if shared.challenge.is_some_and(|(_, at)| at.elapsed().as_micros() as u64 > CHALLENGE_LIFETIME_US * 5 / 6) {
        shared.challenge = None;
    }
    if last_join.elapsed() >= JOIN_INTERVAL {
        *last_join = Instant::now();
        shared.counters.joins_sent += 1;
        let challenge = shared.challenge.map(|(c, _)| c);
        inner.send(&join_step(&inner.seed, player, challenge.as_ref()));
    }
}

fn handle(inner: &Inner, datagram: &[u8], last_join: &mut Instant) {
    let mut shared = inner.shared();
    shared.counters.datagrams += 1;
    shared.counters.bytes += datagram.len() as u64;
    let Some(player) = shared.player else { return };
    match ServerMessage::decode(datagram) {
        Some(ServerMessage::Challenge(challenge)) => {
            // answered at once, and again until the Welcome comes
            shared.challenge = Some((challenge, Instant::now()));
            *last_join = Instant::now();
            shared.counters.joins_sent += 1;
            inner.send(&join_step(&inner.seed, player, Some(&challenge)));
        }
        Some(ServerMessage::Welcome(welcome)) if welcome.player == player => {
            if shared.welcome.is_none() {
                shared.last_snapshot = Some(Instant::now());
            }
            shared.welcome = Some(welcome);
        }
        Some(ServerMessage::Refused(refused)) if refused.player == player => {
            shared.counters.refused += 1;
            shared.error = Some(match refused.reason {
                REFUSED_NO_SEAT => "the gateway refused the player: no seat in the match".into(),
                REFUSED_BAD_PROOF => "the gateway refused the player: the proof did not check out".into(),
                REFUSED_STALE => "the gateway refused the player: the challenge was stale".into(),
                other => format!("the gateway refused the player (reason {other})"),
            });
            // start again from Hello
            shared.challenge = None;
        }
        Some(ServerMessage::Snapshot(snapshot)) => {
            // states are packed against the bounds of the Welcome: none yet,
            // none to read
            let Some(welcome) = shared.welcome else { return };
            shared.last_snapshot = Some(Instant::now());
            shared.ack.record(snapshot.seq);
            if shared.newest_tick == 0 || seq_newer(snapshot.tick, shared.newest_tick) {
                shared.newest_tick = snapshot.tick;
            }
            for packed in &snapshot.states {
                let state = packed.unpack(&welcome.bounds);
                if state.player == player {
                    continue;
                }
                let newer = shared.units.get(&state.player).is_none_or(|known| seq_newer(snapshot.tick, known.tick));
                if newer {
                    shared.units.insert(state.player, RemoteUnit { state, tick: snapshot.tick });
                }
            }
        }
        Some(_) => {}
        None => shared.counters.ignored += 1,
    }
}

/// The direct SpacetimeDB connection: connect, join, subscribe, keep the slow
/// state current, and connect again (as the same identity) if it drops.
fn slow_thread(inner: &Arc<Inner>) {
    while !inner.stop.load(Relaxed) {
        match connect(inner) {
            Ok(connection) => {
                // the connection's own thread runs the callbacks until it ends
                let runner = connection.run_threaded();
                let mut last_try = Instant::now();
                *inner.connection.lock().unwrap_or_else(|p| p.into_inner()) = Some(connection);
                while !inner.stop.load(Relaxed) && !runner.is_finished() {
                    // no seat yet (no spawn point, a full match): ask again
                    if inner.shared().player.is_none() && last_try.elapsed() >= RETRY_DELAY {
                        last_try = Instant::now();
                        if let Some(connection) = inner.connection.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
                            join(inner, &connection.reducers);
                        }
                    }
                    std::thread::sleep(POLL);
                }
                if let Some(connection) = inner.connection.lock().unwrap_or_else(|p| p.into_inner()).take() {
                    let _ = connection.disconnect();
                }
                let _ = runner.join();
                // (what is known of the seat stays: the same identity gets it back)
                let mut shared = inner.shared();
                shared.slow.connected = false;
            }
            Err(e) => inner.shared().error = Some(e),
        }
        let until = Instant::now() + RETRY_DELAY;
        while !inner.stop.load(Relaxed) && Instant::now() < until {
            std::thread::sleep(POLL);
        }
    }
}

/// Ask the match for a seat, with this session's UDP public key. Calling it
/// again, as the same identity, only moves the seat to this connection.
fn join(inner: &Arc<Inner>, reducers: &RemoteReducers) {
    let failed = inner.clone();
    let key = auth::public_key(&inner.seed).to_vec();
    let _ = reducers.join_then(key, move |_, result| {
        let reason = match result {
            Ok(Ok(())) => return,
            Ok(Err(message)) => message,
            Err(e) => e.to_string(),
        };
        failed.shared().error = Some(format!("joining the match: {reason}"));
    });
}

fn connect(inner: &Arc<Inner>) -> Result<DbConnection, String> {
    let token = inner.token.lock().unwrap_or_else(|p| p.into_inner()).clone();
    // once the server has said who this connection is (on the connection's
    // own thread): remember the identity's token, subscribe and take the seat
    let connected = inner.clone();
    let connection = DbConnection::builder()
        .with_uri(inner.config.spacetimedb.as_str())
        .with_database_name(inner.config.database.as_str())
        .with_token(token)
        // the server is usually this machine's or a neighbour's, and the
        // slow state is small
        .with_compression(Compression::None)
        .on_connect(move |connection, identity, token| {
            // (the subscription sends the whole roster again: what was left
            // while the connection was down is no longer on it)
            connected.shared().roster.clear();
            *connected.token.lock().unwrap_or_else(|p| p.into_inner()) = Some(token.into());
            let applied = connected.clone();
            let failed = connected.clone();
            // the map's bounds, who everyone is (which changes only when
            // someone joins or leaves) and this identity's own seat: the rest
            // of the players reach the client by UDP, and a subscription to all
            // of them would send every client every player's every move
            connection
                .subscription_builder()
                .on_applied(move |_| applied.shared().slow.connected = true)
                .on_error(move |_, e| failed.shared().error = Some(format!("the subscription: {e}")))
                .subscribe([
                    "SELECT * FROM map_info".to_string(),
                    "SELECT * FROM roster".to_string(),
                    format!("SELECT * FROM seat WHERE owner = 0x{}", identity.to_hex()),
                ]);
            join(&connected, &connection.reducers);
        })
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

    // the seat says which player this is; the player's own row, which the
    // seat's player id lets the session subscribe to, is where the server put it
    let seated = inner.clone();
    let on_seat = move |ctx: &halo_match_driver::module_bindings::EventContext, seat: &Seat| {
        let changed = {
            let mut shared = seated.shared();
            let changed = shared.player != Some(seat.player);
            if changed {
                // another player id than before: nothing of the old one holds
                shared.player = Some(seat.player);
                shared.welcome = None;
                shared.challenge = None;
                shared.slow.local = None;
                shared.units.clear();
            }
            changed
        };
        if changed {
            let own = seated.clone();
            let id = seat.player;
            ctx.subscription_builder()
                .on_error(move |_, e| own.shared().error = Some(format!("the subscription to the player: {e}")))
                .subscribe([format!("SELECT * FROM player WHERE id = {id}")]);
        }
    };
    let table = connection.db.seat();
    let on_seat_insert = on_seat.clone();
    table.on_insert(move |ctx, seat| on_seat_insert(ctx, seat));
    table.on_update(move |ctx, _, seat| on_seat(ctx, seat));

    let roster = inner.clone();
    let on_member = move |row: &RosterRow| {
        roster.shared().roster.insert(row.player, Member { team: row.team, name: row.name.clone() });
    };
    let table = connection.db.roster();
    let on_member_insert = on_member.clone();
    table.on_insert(move |_, row| on_member_insert(row));
    table.on_update(move |_, _, row| on_member(row));
    let left = inner.clone();
    table.on_delete(move |_, row| {
        // (and what the gateway last sent of them: a player of the same id who
        // joins later starts afresh)
        let mut shared = left.shared();
        shared.roster.remove(&row.player);
        shared.units.remove(&row.player);
    });

    let own = inner.clone();
    let on_player = move |row: &PlayerRow| {
        let mut shared = own.shared();
        if shared.player == Some(row.id) {
            shared.slow.local = Some([row.x, row.y, row.z, row.yaw, row.pitch]);
        }
    };
    let table = connection.db.player();
    let on_player_insert = on_player.clone();
    table.on_insert(move |_, row| on_player_insert(row));
    table.on_update(move |_, _, row| on_player(row));

    Ok(connection)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(player: u16, tick: u32) -> RemoteUnit {
        let state = UnitState { player, position: [1.0, 2.0, 3.0], velocity: [0.0; 3], yaw: 0.0, pitch: 0.0, tick: 0, flags: 0 };
        RemoteUnit { state, tick }
    }

    fn member() -> Member {
        Member { team: 0, name: "x".into() }
    }

    #[test]
    fn a_player_is_in_range_while_the_gateway_keeps_sending_them_and_comes_back_when_it_sends_again() {
        let mut shared = Shared::default();
        for id in [1, 2, 3] {
            shared.roster.insert(id, member());
        }
        shared.newest_tick = 1000;
        shared.units.insert(1, held(1, 1000));
        shared.units.insert(2, held(2, 1000 - OUT_OF_RANGE_TICKS));
        shared.units.insert(3, held(3, 1000 - OUT_OF_RANGE_TICKS - 1));
        let ids = |frame: &Frame| frame.units.iter().map(|u| u.state.player).collect::<Vec<_>>();
        // the last to be just in range, the next just out of it
        assert_eq!(ids(&frame_of(&shared)), vec![1, 2]);
        // sent again, and in range again
        shared.units.insert(3, held(3, 1000));
        assert_eq!(ids(&frame_of(&shared)), vec![1, 2, 3]);
        // the ticks are a counter that wraps
        shared.newest_tick = 10;
        shared.units.insert(1, held(1, u32::MAX - 5));
        shared.units.insert(2, held(2, 10u32.wrapping_sub(OUT_OF_RANGE_TICKS + 1)));
        assert!(ids(&frame_of(&shared)).contains(&1));
        assert!(!ids(&frame_of(&shared)).contains(&2));
    }

    #[test]
    fn a_player_the_roster_does_not_hold_is_not_in_the_frame() {
        let mut shared = Shared::default();
        shared.newest_tick = 5;
        shared.units.insert(1, held(1, 5));
        assert!(frame_of(&shared).units.is_empty(), "sent, but nobody knows who they are");
        shared.roster.insert(1, member());
        assert_eq!(frame_of(&shared).units.len(), 1);
    }

    #[test]
    fn the_join_step_says_hello_and_then_answers_a_challenge_with_a_proof_the_seat_key_checks() {
        let seed = [9u8; SEED_SIZE];
        assert_eq!(join_step(&seed, 7, None), ClientMessage::Hello { player: 7 });
        let challenge = Challenge { stamp: 1234, cookie: [5; 16] };
        let ClientMessage::Auth { player, stamp, cookie, signature } = join_step(&seed, 7, Some(&challenge)) else {
            panic!("an answer to a challenge is an Auth");
        };
        assert_eq!((player, stamp, cookie), (7, 1234, [5; 16]));
        // what the gateway checks it against: the public key on the seat
        assert!(auth::verify_challenge(&auth::public_key(&seed), 7, 1234, &cookie, &signature));
        assert!(!auth::verify_challenge(&auth::public_key(&[1; SEED_SIZE]), 7, 1234, &cookie, &signature));
    }
}
