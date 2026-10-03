//! The wire test harness: simulated UDP players that talk to a real gateway
//! the way a client will, and assertions on what they send and receive.
//!
//! - [`Rig`] starts a SpacetimeDB, the match module, a gateway and the
//!   players' seats (one SpacetimeDB identity each, as [`Seats`]).
//! - [`SimPlayer`] is one player's end of the UDP wire: a socket, a thread
//!   that receives and logs every datagram, answers the gateway's challenge
//!   with the key of its seat, acknowledges Snapshots, and `send_input`.
//!   [`Impairment`] makes its link lossy and slow, in both directions.
//! - [`Crowd`] is many of them.
//! - [`Truth`] is what the server held at every tick, recorded from a
//!   subscriber of the match's tables.
//! - [`analyze`] compares the two: download per player against the budget,
//!   ticks received, how old a tick was on arrival, how often players at
//!   each distance were updated, and how stale any state got.
//!
//! # Writing a wire test
//!
//! A test starts a [`Rig`] (`tests/wire.rs` has a `rig(...)` that skips the
//! test without `HALO_STDB_BIN`), connects a [`Crowd`] to its gateway and
//! walks it:
//!
//! ```ignore
//! let mut rig = rig("name", flat_floor_map(), &anchors, 50, 90_000).unwrap();
//! let crowd = Crowd::connect(rig.gateway.local_addr(), 0..50, Impairment::none(), 64, false);
//! crowd.join_all(Duration::from_secs(10))?;
//! let mut truth = Truth::new(64);
//! run_walk(&rig.client, &mut rig.walkers, &crowd, &mut truth, Duration::from_secs(5));
//! let report = analyze(&crowd, &truth, truth.window(30, 3), &DEFAULT_BANDS);
//! assert!(report.max_download <= budget as f64);
//! ```
//!
//! `tests/wire.rs` has working ones, among them the ones that try to cheat
//! (`Raw` there speaks the protocol by hand). Take the serial lock first, and
//! call `rig.client.discard_ticks()` before waiting on a tick.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};
use std::fmt;
use std::net::{SocketAddr, UdpSocket};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use halo_match_driver::server::{Account, Server};
use halo_match_driver::walkers::Walkers;
use halo_match_driver::{now_us, MatchClient, PlayerClient, SeenTick};
use halo_sim::{PlayerInput, Rng, TICKS_PER_SECOND};
use halo_wire::auth;
use halo_wire::datagram::{Ack, ClientMessage, Refused, ServerMessage, Welcome, IP_UDP_OVERHEAD};
use halo_wire::unit::PackedState;

use crate::stats::Spread;
use crate::{Gateway, GatewayConfig, UdpTransport};

/// Distance bands, in world units: `[0, 10)`, `[10, 25)`, `[25, 60)`, `[60, ..)`.
pub const DEFAULT_BANDS: [f32; 3] = [10.0, 25.0, 60.0];

/// Trouble on a player's link, in each direction independently.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Impairment {
    /// Chance each datagram is dropped, 0 to 1.
    pub loss: f32,
    /// How much later each datagram arrives.
    pub delay: Duration,
}

impl Impairment {
    pub fn none() -> Impairment {
        Impairment::default()
    }

    pub fn loss(loss: f32) -> Impairment {
        Impairment { loss, ..Impairment::default() }
    }

    pub fn delayed(self, delay: Duration) -> Impairment {
        Impairment { delay, ..self }
    }
}

/// A datagram waiting for its time.
struct Pending {
    due: Instant,
    id: u64,
    socket: Arc<UdpSocket>,
    to: SocketAddr,
    bytes: Vec<u8>,
}

impl PartialEq for Pending {
    fn eq(&self, other: &Self) -> bool {
        (self.due, self.id) == (other.due, other.id)
    }
}
impl Eq for Pending {}
impl PartialOrd for Pending {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Pending {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.due, self.id).cmp(&(other.due, other.id))
    }
}

/// Sends datagrams at the time they are due.
struct DelayLine {
    queue: Mutex<BinaryHeap<Reverse<Pending>>>,
    wake: Condvar,
    next: std::sync::atomic::AtomicU64,
    stop: AtomicBool,
}

impl DelayLine {
    fn start() -> (Arc<DelayLine>, JoinHandle<()>) {
        let line = Arc::new(DelayLine {
            queue: Mutex::new(BinaryHeap::new()),
            wake: Condvar::new(),
            next: Default::default(),
            stop: AtomicBool::new(false),
        });
        let worker = line.clone();
        let handle = std::thread::spawn(move || worker.run());
        (line, handle)
    }

    fn push(&self, due: Instant, socket: Arc<UdpSocket>, to: SocketAddr, bytes: Vec<u8>) {
        let id = self.next.fetch_add(1, Relaxed);
        self.queue.lock().unwrap().push(Reverse(Pending { due, id, socket, to, bytes }));
        self.wake.notify_one();
    }

    fn run(&self) {
        let mut queue = self.queue.lock().unwrap();
        while !self.stop.load(Relaxed) {
            let now = Instant::now();
            match queue.peek().map(|Reverse(p)| p.due) {
                Some(due) if due <= now => {
                    let Reverse(p) = queue.pop().unwrap();
                    let _ = p.socket.send_to(&p.bytes, p.to);
                }
                Some(due) => queue = self.wake.wait_timeout(queue, due - now).unwrap().0,
                None => queue = self.wake.wait_timeout(queue, Duration::from_millis(100)).unwrap().0,
            }
        }
    }
}

/// What one tick looked like to one player.
#[derive(Debug, Clone)]
pub struct TickReceipt {
    /// When the first datagram of the tick arrived (this machine's clock,
    /// microseconds since the epoch, plus the link's delay).
    pub first_at_us: i64,
    pub datagrams: u32,
    /// Bytes on the wire: payloads plus IP and UDP headers.
    pub wire_bytes: u32,
    /// Bit `p` is set if a state of player `p` arrived for this tick.
    senders: Vec<u64>,
}

impl TickReceipt {
    pub fn has_state_of(&self, player: u16) -> bool {
        self.senders.get(player as usize / 64).is_some_and(|w| w >> (player % 64) & 1 == 1)
    }
}

#[derive(Default)]
struct Log {
    welcome: Option<Welcome>,
    refused: Vec<Refused>,
    /// Which Snapshots have arrived, as every Input says.
    ack: Ack,
    ticks: BTreeMap<u32, TickReceipt>,
    /// Every state received with the tick of its datagram; kept only when asked.
    states: Vec<(u32, PackedState)>,
    datagrams: u64,
    wire_bytes: u64,
}

/// The secret seed of a simulated player's UDP key pair (they are all
/// different and all known, so that a test can both join and forge).
pub fn sim_seed(id: u16) -> [u8; 32] {
    let mut seed = [0x42u8; 32];
    seed[..2].copy_from_slice(&id.to_le_bytes());
    seed
}

/// The public key a simulated player's seat holds.
pub fn sim_public_key(id: u16) -> [u8; 32] {
    auth::public_key(&sim_seed(id))
}

/// One simulated player's end of the wire, shared with its receiving thread.
struct Link {
    socket: Arc<UdpSocket>,
    gateway: SocketAddr,
    impairment: Impairment,
    rng: Mutex<Rng>,
    delay_line: Option<Arc<DelayLine>>,
}

impl Link {
    fn send(&self, bytes: Vec<u8>) {
        if self.rng.lock().unwrap().next_f32() < self.impairment.loss {
            return;
        }
        match &self.delay_line {
            Some(line) if !self.impairment.delay.is_zero() => {
                line.push(Instant::now() + self.impairment.delay, self.socket.clone(), self.gateway, bytes)
            }
            _ => {
                let _ = self.socket.send_to(&bytes, self.gateway);
            }
        }
    }
}

/// One simulated player.
///
/// It plays the client's part of the join: a Hello is answered, as a client
/// does, by signing the Challenge it gets back with the player's key
/// ([`sim_seed`]) and sending the Auth. Once welcomed it acknowledges the
/// Snapshots it receives in every Input.
pub struct SimPlayer {
    pub id: u16,
    link: Arc<Link>,
    seq: AtomicU32,
    log: Arc<Mutex<Log>>,
    stop: Arc<AtomicBool>,
}

impl SimPlayer {
    /// A player on its own loopback socket. `capacity` is one more than the
    /// largest player id the match will hold; `record_states` keeps every
    /// state received (for content checks; costs memory on big runs).
    fn connect(
        gateway: SocketAddr,
        id: u16,
        impairment: Impairment,
        capacity: usize,
        record_states: bool,
        delay_line: Option<Arc<DelayLine>>,
    ) -> (SimPlayer, JoinHandle<()>) {
        let socket = UdpSocket::bind("127.0.0.1:0").expect("bind a player socket");
        socket.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        let link = Arc::new(Link {
            socket: Arc::new(socket),
            gateway,
            impairment,
            rng: Mutex::new(Rng::seeded(0x5eed ^ id as u64)),
            delay_line,
        });
        let log = Arc::new(Mutex::new(Log::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let player = SimPlayer { id, link: link.clone(), seq: AtomicU32::new(1), log: log.clone(), stop: stop.clone() };
        let mut rng = Rng::seeded(0xfeed ^ id as u64);
        let seed = sim_seed(id);
        let handle = std::thread::Builder::new()
            .name(format!("sim-player-{id}"))
            .spawn(move || {
                let mut buf = vec![0u8; 2048];
                let words = capacity.div_ceil(64);
                while !stop.load(Relaxed) {
                    let Ok(len) = link.socket.recv(&mut buf) else { continue };
                    let at = now_us();
                    if rng.next_f32() < impairment.loss {
                        continue;
                    }
                    let at = at + impairment.delay.as_micros() as i64;
                    let wire = (len + IP_UDP_OVERHEAD) as u32;
                    match ServerMessage::decode(&buf[..len]) {
                        Some(ServerMessage::Challenge(c)) => {
                            let signature = auth::sign_challenge(&seed, id, c.stamp, &c.cookie);
                            link.send(
                                ClientMessage::Auth { player: id, stamp: c.stamp, cookie: c.cookie, signature }
                                    .encode(),
                            );
                        }
                        Some(ServerMessage::Welcome(w)) => log.lock().unwrap().welcome = Some(w),
                        Some(ServerMessage::Refused(r)) => log.lock().unwrap().refused.push(r),
                        Some(ServerMessage::Snapshot(s)) => {
                            let mut log = log.lock().unwrap();
                            log.datagrams += 1;
                            log.wire_bytes += wire as u64;
                            log.ack.record(s.seq);
                            let receipt = log.ticks.entry(s.tick).or_insert_with(|| TickReceipt {
                                first_at_us: at,
                                datagrams: 0,
                                wire_bytes: 0,
                                senders: vec![0; words],
                            });
                            receipt.first_at_us = receipt.first_at_us.min(at);
                            receipt.datagrams += 1;
                            receipt.wire_bytes += wire;
                            for state in &s.states {
                                let p = state.player() as usize;
                                if let Some(word) = receipt.senders.get_mut(p / 64) {
                                    *word |= 1 << (p % 64);
                                }
                            }
                            if record_states {
                                log.states.extend(s.states.iter().map(|st| (s.tick, *st)));
                            }
                        }
                        None => {}
                    }
                }
            })
            .unwrap();
        (player, handle)
    }

    /// Ask the gateway for a Challenge (which the player answers by itself).
    pub fn hello(&self) {
        self.link.send(ClientMessage::Hello { player: self.id }.encode());
    }

    /// Send an input with the next sequence number, and the acknowledgement of what has been received.
    pub fn send_input(&self, input: &PlayerInput) -> u32 {
        let seq = self.seq.fetch_add(1, Relaxed);
        self.send_input_with_seq(seq, input);
        seq
    }

    /// Send an input with a sequence number of the test's choosing.
    pub fn send_input_with_seq(&self, seq: u32, input: &PlayerInput) {
        let ack = self.log.lock().unwrap().ack;
        self.link.send(ClientMessage::Input { seq, input: *input, ack }.encode());
    }

    /// Forget the session: a client that starts again numbers its inputs from
    /// 1 and has received nothing. (Its socket, and so its address, stays.)
    pub fn restart_session(&self) {
        self.seq.store(1, Relaxed);
        let mut log = self.log.lock().unwrap();
        log.welcome = None;
        log.refused.clear();
        log.ack = Ack::NONE;
    }

    /// The gateway's Welcome, if it came.
    pub fn welcome(&self) -> Option<Welcome> {
        self.log.lock().unwrap().welcome
    }

    /// Everything the gateway refused this player.
    pub fn refusals(&self) -> Vec<Refused> {
        self.log.lock().unwrap().refused.clone()
    }

    /// The address this player sends from.
    pub fn local_addr(&self) -> SocketAddr {
        self.link.socket.local_addr().unwrap()
    }

    /// The ticks a datagram arrived for, ascending.
    pub fn ticks_received(&self) -> Vec<u32> {
        self.log.lock().unwrap().ticks.keys().copied().collect()
    }

    pub fn receipt(&self, tick: u32) -> Option<TickReceipt> {
        self.log.lock().unwrap().ticks.get(&tick).cloned()
    }

    /// Every state received, with the tick of the datagram that carried it
    /// (only if the crowd was told to record states).
    pub fn states(&self) -> Vec<(u32, PackedState)> {
        self.log.lock().unwrap().states.clone()
    }
}

impl Drop for SimPlayer {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
    }
}

/// Many simulated players.
pub struct Crowd {
    players: Vec<SimPlayer>,
    threads: Vec<JoinHandle<()>>,
    delay_line: Option<(Arc<DelayLine>, JoinHandle<()>)>,
}

impl Crowd {
    pub fn connect(
        gateway: SocketAddr,
        ids: Range<u16>,
        impairment: Impairment,
        capacity: usize,
        record_states: bool,
    ) -> Crowd {
        let delay_line = (!impairment.delay.is_zero()).then(DelayLine::start);
        let mut players = Vec::new();
        let mut threads = Vec::new();
        for id in ids {
            let (p, t) = SimPlayer::connect(
                gateway,
                id,
                impairment,
                capacity,
                record_states,
                delay_line.as_ref().map(|(l, _)| l.clone()),
            );
            players.push(p);
            threads.push(t);
        }
        Crowd { players, threads, delay_line }
    }

    pub fn players(&self) -> &[SimPlayer] {
        &self.players
    }

    pub fn player(&self, id: u16) -> Option<&SimPlayer> {
        self.players.iter().find(|p| p.id == id)
    }

    /// Say hello for every player until each has been welcomed (the players
    /// answer the challenges themselves). Hellos are resent every 200 ms (a
    /// lossy link may drop them, or the answers).
    pub fn join_all(&self, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        loop {
            let waiting: Vec<&SimPlayer> = self.players.iter().filter(|p| p.welcome().is_none()).collect();
            if waiting.is_empty() {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(format!("{} of {} players were not welcomed", waiting.len(), self.players.len()));
            }
            for p in waiting {
                p.hello();
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// Send each input from its player (inputs of players not in the crowd are ignored).
    pub fn send_inputs(&self, inputs: &[PlayerInput]) {
        let first = self.players.first().map_or(0, |p| p.id);
        for input in inputs {
            if let Some(p) =
                self.players.get(input.player.wrapping_sub(first) as usize).filter(|p| p.id == input.player)
            {
                p.send_input(input);
            }
        }
    }
}

impl Drop for Crowd {
    fn drop(&mut self) {
        for p in &self.players {
            p.stop.store(true, Relaxed);
        }
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
        if let Some((line, handle)) = self.delay_line.take() {
            line.stop.store(true, Relaxed);
            line.wake.notify_all();
            let _ = handle.join();
        }
    }
}

/// The simulated players' seats in the match: one SpacetimeDB connection per
/// player, as their own identity, which `join`ed with the key of
/// [`sim_seed`]. Kept alive for as long as the players play (a dropped
/// connection puts the seat on hold).
pub struct Seats {
    pub accounts: Vec<Account>,
    pub clients: Vec<PlayerClient>,
}

impl Seats {
    /// Make `count` identities and have each take a seat, one after another,
    /// so that player `n` gets id `n` (the match must be empty, with spawn
    /// points set). Panics unless every seat is where it should be.
    pub fn join(server: &Server, database: &str, owner: &MatchClient, count: u16) -> Seats {
        Seats::join_ids(server, database, owner, 0..count)
    }

    /// Like [`Seats::join`] for the ids `ids`, which the match must hand out next in turn.
    pub fn join_ids(server: &Server, database: &str, owner: &MatchClient, ids: Range<u16>) -> Seats {
        let mut seats = Seats { accounts: Vec::new(), clients: Vec::new() };
        let ids_again = ids.clone();
        for id in ids {
            let account = server.new_account();
            let client = PlayerClient::connect_unsubscribed(&server.uri(), database, &account.token);
            client.join(sim_public_key(id)).unwrap_or_else(|e| panic!("player {id} joining: {e}"));
            seats.accounts.push(account);
            seats.clients.push(client);
        }
        let held = Instant::now() + Duration::from_secs(30);
        for (id, account) in ids_again.zip(&seats.accounts) {
            while owner.seats().get(&id).map(|s| s.owner) != Some(account.identity()) {
                assert!(Instant::now() < held, "player {id}'s seat did not appear as the one for their identity");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        seats
    }
}

/// What the server held at one tick.
#[derive(Debug, Clone)]
pub struct TruthTick {
    pub stamped_us: i64,
    /// By player id; `None` for an id not in the match.
    pub positions: Vec<Option<[f32; 3]>>,
    /// The tick each player's position was last changed on.
    pub updated_tick: Vec<u64>,
    pub rejected_total: u64,
}

/// The server's state at every tick, recorded from a subscriber.
#[derive(Debug, Clone)]
pub struct Truth {
    capacity: usize,
    pub ticks: BTreeMap<u32, TruthTick>,
}

impl Truth {
    /// `capacity`: one more than the largest player id.
    pub fn new(capacity: usize) -> Truth {
        Truth { capacity, ticks: BTreeMap::new() }
    }

    pub fn record(&mut self, seen: &SeenTick) {
        let mut tick = TruthTick {
            stamped_us: seen.marker.stamped_us,
            positions: vec![None; self.capacity],
            updated_tick: vec![0; self.capacity],
            rejected_total: seen.marker.rejected_total,
        };
        for (id, row) in &seen.players {
            if (*id as usize) < self.capacity {
                tick.positions[*id as usize] = Some([row.x, row.y, row.z]);
                tick.updated_tick[*id as usize] = row.updated_tick;
            }
        }
        self.ticks.insert(seen.marker.tick as u32, tick);
    }

    /// The ticks recorded, without the first `skip_start` and the last `skip_end`.
    pub fn window(&self, skip_start: usize, skip_end: usize) -> Range<u32> {
        let (Some(first), Some(last)) = (self.ticks.keys().nth(skip_start), self.ticks.keys().rev().nth(skip_end))
        else {
            return 0..0;
        };
        *first..*last + 1
    }
}

/// Walk `walkers` for `duration`, the way clients under test do: each time a
/// tick completes, plan every walker's next move from what the server holds
/// and send it as that player's input. Records every tick in `truth`.
///
/// A client plans from the newest tick it has seen. The subscriber queues
/// every tick since it connected (the join, and a loaded machine, can leave
/// dozens), so the walk records every tick that is waiting but plans from the
/// last of them: planning from each in turn would send a burst of moves from
/// states the server left long ago, which no client does. A walk that starts
/// a fresh `truth` starts at the present: the ticks queued from before it
/// (while the players were still joining, and nobody was sent to) are not
/// part of what the walk is measured against.
pub fn run_walk(client: &MatchClient, walkers: &mut Walkers, crowd: &Crowd, truth: &mut Truth, duration: Duration) {
    if truth.ticks.is_empty() {
        client.discard_ticks();
    }
    let until = Instant::now() + duration;
    while Instant::now() < until {
        let Some(mut seen) = client.next_tick(Duration::from_secs(10)) else { panic!("no tick for 10 s") };
        truth.record(&seen);
        while let Some(newer) = client.next_tick(Duration::ZERO) {
            truth.record(&newer);
            seen = newer;
        }
        walkers.sync_with_server(seen.players.values());
        crowd.send_inputs(&walkers.next_inputs());
    }
}

/// How often the players of one distance band were updated.
#[derive(Debug, Clone, PartialEq)]
pub struct BandStats {
    pub from: f32,
    /// `f32::INFINITY` for the last band.
    pub to: f32,
    /// (recipient, other player, tick) triples in the band.
    pub pairs: u64,
    /// ... that were sent a state of the other player that tick.
    pub updated: u64,
    /// Updates a second for the average pair in the band.
    pub hz: f64,
    /// The fraction of pairs updated in a tick, 0 to 1.
    pub fraction_updated: f64,
    /// The most ticks between two updates of a pair that was in this band
    /// for the whole gap. A pair that changed band, or of which either player
    /// left the world, starts afresh: see `longest_entry_gap_ticks`.
    pub longest_gap_ticks: u32,
    /// Updates that came more than 100 ms (four ticks or more) after the
    /// pair's previous one, with the pair in this band throughout.
    pub stalls_over_100ms: u64,
    pub stall_fraction: f64,
    /// The most ticks between a pair's last update in another band and its
    /// first in this one (both players in the world throughout). Reported
    /// apart from the gaps above: a far player updated every 15 ticks that
    /// jumps close waits that long for its first near update, which says
    /// nothing about how near players are served.
    pub longest_entry_gap_ticks: u32,
}

/// The largest age, in ticks, the report tells apart: older ones are counted as this.
pub const MAX_AGE_TICKS: usize = 255;

#[derive(Debug, Clone)]
pub struct Report {
    pub players: usize,
    pub ticks: usize,
    pub seconds: f64,
    /// Bytes a second on the wire per player (IP and UDP headers included).
    pub mean_download: f64,
    pub max_download: f64,
    /// (player, tick) pairs where not one datagram arrived, of `expected_receipts`.
    pub missed_ticks: u64,
    pub expected_receipts: u64,
    /// How old a tick was when its first datagram reached a player, ms.
    pub tick_age_ms: Spread,
    pub bands: Vec<BandStats>,
    /// Staleness: for every (recipient, other player, tick) where the
    /// recipient had received a state of the other player at some point, how
    /// many ticks ago the newest one it had was sent for. `age_counts[a]` is
    /// the number of such triples with an age of `a` ticks (the last counts
    /// every older one).
    pub age_counts: Vec<u64>,
    /// The greatest age seen, in ticks (a tick is 33.3 ms).
    pub max_age_ticks: u32,
}

impl Report {
    /// The share of (recipient, other, tick) triples whose newest state was older than `ticks` ticks.
    pub fn share_older_than(&self, ticks: usize) -> f64 {
        let total: u64 = self.age_counts.iter().sum();
        let older: u64 = self.age_counts.iter().skip(ticks + 1).sum();
        older as f64 / total.max(1) as f64
    }

    /// How many triples were older than `ticks` ticks.
    pub fn count_older_than(&self, ticks: usize) -> u64 {
        self.age_counts.iter().skip(ticks + 1).sum()
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{} players over {} ticks ({:.1} s)", self.players, self.ticks, self.seconds)?;
        writeln!(
            f,
            "download per player   mean {:.1} KB/s   max {:.1} KB/s (wire bytes, headers included)",
            self.mean_download / 1e3,
            self.max_download / 1e3
        )?;
        writeln!(f, "ticks missed          {} of {}", self.missed_ticks, self.expected_receipts)?;
        writeln!(
            f,
            "tick age on arrival   p50 {:.2}  p99 {:.2}  max {:.2} ms",
            self.tick_age_ms.p50, self.tick_age_ms.p99, self.tick_age_ms.max
        )?;
        for b in &self.bands {
            writeln!(
                f,
                "  {:>5.0} to {:<5} wu   {:>6.2} Hz   updated in {:>6.2}% of ticks   longest gap {:>3} ticks   gaps over 100 ms {:.3}%   ({} pairs)   longest wait on entry {:>3} ticks",
                b.from,
                if b.to.is_finite() { format!("{:.0}", b.to) } else { "...".into() },
                b.hz,
                b.fraction_updated * 100.0,
                b.longest_gap_ticks,
                b.stall_fraction * 100.0,
                b.pairs,
                b.longest_entry_gap_ticks
            )?;
        }
        writeln!(
            f,
            "state staleness       oldest {} ticks ({:.0} ms)   older than 8 ticks {:.4}%   older than 15 {:.5}%   older than 40 {:.6}%",
            self.max_age_ticks,
            self.max_age_ticks as f64 * 1000.0 / TICKS_PER_SECOND as f64,
            self.share_older_than(8) * 100.0,
            self.share_older_than(15) * 100.0,
            self.share_older_than(40) * 100.0
        )
    }
}

/// Gaps between updates, by band, of every (recipient, other player) pair.
///
/// A gap belongs to a band only if the pair was in it for the whole gap. A
/// pair that changes band starts afresh in its new band, and so does one of
/// which either player is out of the world for a tick; the first update after
/// a band change is kept as a wait on entry, apart from the band's own gaps.
struct GapTracker {
    capacity: usize,
    pairs: Vec<PairGap>,
    longest: Vec<u32>,
    stalls: Vec<u64>,
    longest_entry: Vec<u32>,
}

#[derive(Clone, Copy, Default)]
struct PairGap {
    /// The tick of the pair's last update (0 if none yet, or it was forgotten).
    last_update: u32,
    /// The tick the pair was last in the world together on.
    seen: u32,
    band: usize,
    /// No band change since `last_update`.
    in_one_band: bool,
}

impl GapTracker {
    fn new(capacity: usize, bands: usize) -> GapTracker {
        GapTracker {
            capacity,
            pairs: vec![PairGap::default(); capacity * capacity],
            longest: vec![0; bands],
            stalls: vec![0; bands],
            longest_entry: vec![0; bands],
        }
    }

    /// The pair is in the world together in `band` at `tick`, whose
    /// predecessor in the record is `previous` (`None` for the first), and was
    /// `updated` this tick or not.
    fn observe(&mut self, me: usize, other: usize, tick: u32, previous: Option<u32>, band: usize, updated: bool) {
        let pair = &mut self.pairs[me * self.capacity + other];
        if previous != Some(pair.seen) {
            *pair = PairGap { band, in_one_band: true, ..PairGap::default() };
        }
        pair.seen = tick;
        if pair.band != band {
            pair.band = band;
            pair.in_one_band = false;
        }
        if !updated {
            return;
        }
        if pair.last_update != 0 {
            let gap = tick - pair.last_update;
            if pair.in_one_band {
                self.longest[band] = self.longest[band].max(gap);
                // a gap of g ticks is g / 30 s
                if gap as f64 * 1000.0 / TICKS_PER_SECOND as f64 > 100.0 {
                    self.stalls[band] += 1;
                }
            } else {
                self.longest_entry[band] = self.longest_entry[band].max(gap);
            }
        }
        pair.last_update = tick;
        pair.in_one_band = true;
    }
}

/// Compare what the crowd received with the truth, over the ticks in `window`.
/// `edges` are the upper bounds of the distance bands, ascending (see
/// [`DEFAULT_BANDS`]); a last band has no upper bound.
pub fn analyze(crowd: &Crowd, truth: &Truth, window: Range<u32>, edges: &[f32]) -> Report {
    let capacity = truth.capacity;
    let ticks: Vec<(&u32, &TruthTick)> = truth.ticks.range(window).collect();
    let bands = edges.len() + 1;
    let band_of = |d2: f32| edges.iter().position(|e| d2 < e * e).unwrap_or(edges.len());
    let mut pairs = vec![0u64; bands];
    let mut updated = vec![0u64; bands];
    let mut gaps = GapTracker::new(capacity, bands);
    // for the age figures, which carry on across band changes and absences
    let mut last_update = vec![0u32; capacity * capacity];
    let mut age_counts = vec![0u64; MAX_AGE_TICKS + 1];
    let mut max_age = 0u32;

    let mut ages = Vec::new();
    let mut bytes = vec![0u64; crowd.players.len()];
    let (mut missed, mut expected) = (0u64, 0u64);
    let mut previous = None;
    for &(&tick, at) in &ticks {
        let before = previous.replace(tick);
        for (index, player) in crowd.players.iter().enumerate() {
            let me = player.id as usize;
            let receipt = player.receipt(tick);
            // (a player the match does not have in the world, one waiting to spawn, is sent nothing)
            let in_world = at.positions[me].is_some();
            expected += in_world as u64;
            match &receipt {
                Some(receipt) => {
                    ages.push((receipt.first_at_us - at.stamped_us) as f64 / 1e3);
                    bytes[index] += receipt.wire_bytes as u64;
                }
                None => missed += in_world as u64,
            }
            let Some(from) = at.positions[me] else { continue };
            for (other, position) in at.positions.iter().enumerate() {
                let Some(position) = position else { continue };
                if other == me {
                    continue;
                }
                let d = [position[0] - from[0], position[1] - from[1], position[2] - from[2]];
                let band = band_of(d[0] * d[0] + d[1] * d[1] + d[2] * d[2]);
                pairs[band] += 1;
                let slot = &mut last_update[me * capacity + other];
                let was_updated = receipt.as_ref().is_some_and(|r| r.has_state_of(other as u16));
                gaps.observe(me, other, tick, before, band, was_updated);
                if was_updated {
                    updated[band] += 1;
                    *slot = tick;
                }
                if *slot != 0 {
                    let age = tick - *slot;
                    max_age = max_age.max(age);
                    age_counts[(age as usize).min(MAX_AGE_TICKS)] += 1;
                }
            }
        }
    }

    let seconds = match (ticks.first(), ticks.last()) {
        (Some((_, a)), Some((_, b))) => (b.stamped_us - a.stamped_us) as f64 / 1e6 + 1.0 / TICKS_PER_SECOND as f64,
        _ => 0.0,
    };
    let per_second: Vec<f64> = bytes.iter().map(|b| *b as f64 / seconds.max(1e-9)).collect();
    let edges_lo = std::iter::once(0.0).chain(edges.iter().copied());
    let edges_hi = edges.iter().copied().chain(std::iter::once(f32::INFINITY));
    Report {
        players: crowd.players.len(),
        ticks: ticks.len(),
        seconds,
        mean_download: per_second.iter().sum::<f64>() / per_second.len().max(1) as f64,
        max_download: per_second.iter().copied().fold(0.0, f64::max),
        missed_ticks: missed,
        expected_receipts: expected,
        tick_age_ms: Spread::of(ages),
        bands: edges_lo
            .zip(edges_hi)
            .enumerate()
            .map(|(i, (from, to))| BandStats {
                from,
                to,
                pairs: pairs[i],
                updated: updated[i],
                hz: updated[i] as f64 / pairs[i].max(1) as f64 * TICKS_PER_SECOND as f64,
                fraction_updated: updated[i] as f64 / pairs[i].max(1) as f64,
                longest_gap_ticks: gaps.longest[i],
                stalls_over_100ms: gaps.stalls[i],
                stall_fraction: gaps.stalls[i] as f64 / updated[i].max(1) as f64,
                longest_entry_gap_ticks: gaps.longest_entry[i],
            })
            .collect(),
        age_counts,
        max_age_ticks: max_age,
    }
}

/// A running match with its players seated and a gateway in front of it:
/// everything a wire test needs short of the simulated UDP players
/// ([`Crowd`]). Fields drop in order: the gateway and the subscriber go
/// before the seats and the server.
pub struct Rig {
    pub gateway: Gateway,
    /// The match's owner, subscribed to its tables.
    pub client: MatchClient,
    /// Walkers placed at the spawn points the players were seated at.
    pub walkers: Walkers,
    /// The players' seats: one SpacetimeDB identity each, ids `0..players`.
    pub seats: Seats,
    /// One more than the largest player id.
    pub capacity: usize,
    pub server: Server,
    /// The identity the gateway connected as, which the match accepts input from.
    pub gateway_account: Account,
}

/// What a [`Rig`] is to be: the match, its players, the gateway's budget.
pub struct RigSetup<'a> {
    /// The match's database name.
    pub name: &'a str,
    pub map: halo_sim::MapData,
    /// Where the walkers are placed around (on the ground).
    pub anchors: &'a [[f32; 3]],
    pub players: u16,
    /// Bytes a second a player may be sent.
    pub budget: u32,
}

impl Rig {
    /// Start a SpacetimeDB (the release in `bin`), publish `wasm` to it, load
    /// the map, set a spawn point for each of the players' walkers, name a
    /// gateway identity, start the match, seat the players (so player `n`
    /// stands where walker `n` does) and start a gateway on loopback UDP.
    /// `tune` may change the gateway's configuration first.
    pub fn start(
        bin: &std::path::Path,
        wasm: &std::path::Path,
        setup: RigSetup,
        tune: impl FnOnce(&mut GatewayConfig),
    ) -> Rig {
        let RigSetup { name, map, anchors, players, budget } = setup;
        let server = Server::start(bin);
        server.publish(wasm, name);
        let client = server.connect(name);
        client.load_map(map.to_bytes()).unwrap();
        let (walkers, spawn) = Walkers::new(map, anchors, players, 7);
        client.set_spawn_points(&spawn).unwrap();
        client.set_capacity(players.max(500)).unwrap();
        let gateway_account = server.new_account();
        client.set_gateway(gateway_account.identity()).unwrap();
        client.start();
        let seats = Seats::join(&server, name, &client, players);
        let mut config = GatewayConfig::new(server.uri(), name);
        config.token = Some(gateway_account.token.clone());
        config.budget_bytes_per_second = budget;
        tune(&mut config);
        let transport = Arc::new(UdpTransport::bind("127.0.0.1:0".parse().unwrap()).unwrap());
        let gateway = Gateway::start(config, transport).expect("start the gateway");
        Rig { gateway, client, walkers, seats, capacity: players as usize, server, gateway_account }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NEAR: usize = 0;
    const FAR: usize = 3;

    /// Feed a tracker one pair (0, 1): `at(tick)` is `Some((band, updated))`
    /// when both are in the world.
    fn run(ticks: Range<u32>, at: impl Fn(u32) -> Option<(usize, bool)>) -> GapTracker {
        let mut t = GapTracker::new(2, 4);
        let mut previous = None;
        for tick in ticks {
            if let Some((band, updated)) = at(tick) {
                t.observe(0, 1, tick, previous, band, updated);
            }
            previous = Some(tick);
        }
        t
    }

    #[test]
    fn a_pair_that_jumps_into_a_band_has_no_gap_there_only_a_wait_on_entry() {
        // far (over 60 wu) and updated every 15 ticks, then within 10 wu from tick 61 and updated every tick
        let t = run(1..100, |tick| if tick < 61 { Some((FAR, tick % 15 == 0)) } else { Some((NEAR, true)) });
        assert_eq!(t.longest[NEAR], 1);
        assert_eq!(t.longest[FAR], 15);
        assert_eq!(t.longest_entry[NEAR], 1, "last far update at 60, first near one at 61");
        assert_eq!(t.longest_entry[FAR], 0);
    }

    #[test]
    fn a_long_wait_on_entry_is_kept_apart_from_the_bands_gaps() {
        // updated at tick 45 far away, close from 59, first near update at 60
        let t = run(1..100, |tick| match tick {
            ..=44 => Some((FAR, false)),
            45 => Some((FAR, true)),
            46..=58 => Some((FAR, false)),
            59 => Some((NEAR, false)),
            _ => Some((NEAR, true)),
        });
        assert_eq!(t.longest_entry[NEAR], 15);
        assert_eq!(t.longest[NEAR], 1);
    }

    #[test]
    fn a_player_out_of_the_world_for_100_ticks_leaves_no_gap_of_100() {
        let t = run(1..300, |tick| match tick {
            101..=200 => None,
            _ => Some((NEAR, true)),
        });
        assert!(t.longest.iter().all(|g| *g < 100), "{:?}", t.longest);
        assert!(t.longest_entry.iter().all(|g| *g < 100), "{:?}", t.longest_entry);
        assert_eq!(t.longest[NEAR], 1);
    }

    #[test]
    fn a_pair_that_returns_to_the_band_it_left_does_not_count_the_trip_as_a_gap_there() {
        let t = run(1..100, |tick| match tick {
            1 => Some((NEAR, true)),
            2..=29 => Some((FAR, false)),
            30..=40 => Some((NEAR, false)),
            _ => Some((NEAR, true)),
        });
        assert_eq!(t.longest[NEAR], 1);
        assert_eq!(t.longest_entry[NEAR], 41 - 1);
    }

    #[test]
    fn a_pair_that_stays_in_a_band_and_misses_updates_is_reported_there() {
        // updated every 5 ticks
        let t = run(1..100, |tick| Some((1, tick % 5 == 0)));
        assert_eq!(t.longest[1], 5);
        assert_eq!(t.stalls[1], 18, "gaps of 5 ticks (167 ms), from tick 10 to 95");
        assert_eq!(t.longest_entry[1], 0);
    }
}
