//! The wire test harness: simulated UDP players that talk to a real gateway
//! the way a client will, and assertions on what they send and receive.
//!
//! - [`SimPlayer`] is one player: a UDP socket, a thread that receives and
//!   logs every datagram, and `send_input`. [`Impairment`] makes its link
//!   lossy and slow, in both directions.
//! - [`Crowd`] is many of them.
//! - [`Truth`] is what the server held at every tick, recorded from a
//!   subscriber of the match's tables.
//! - [`analyze`] compares the two: download per player against the budget,
//!   ticks received, how old a tick was on arrival, and how often players at
//!   each distance were updated.
//!
//! # Writing a wire test
//!
//! A test starts a server (`halo_match_driver::server`), publishes the
//! module, connects a `MatchClient`, loads a map and adds players, starts a
//! gateway on a loopback `UdpTransport`, then:
//!
//! ```ignore
//! let crowd = Crowd::connect(gateway.local_addr(), 0..50, Impairment::none(), 64, false);
//! crowd.join_all(Duration::from_secs(10))?;
//! let mut truth = Truth::new(64);
//! run_walk(&client, &mut walkers, &crowd, &mut truth, Duration::from_secs(5));
//! let report = analyze(&crowd, &truth, truth.window(30, 3), &DEFAULT_BANDS);
//! assert!(report.max_download_bytes_per_second <= budget as f64);
//! ```
//!
//! `tests/wire.rs` has working ones.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};
use std::fmt;
use std::net::{SocketAddr, UdpSocket};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use halo_match_driver::walkers::Walkers;
use halo_match_driver::{now_us, MatchClient, SeenTick};
use halo_sim::{PlayerInput, Rng, TICKS_PER_SECOND};
use halo_wire::datagram::{ClientMessage, ServerMessage, Welcome, IP_UDP_OVERHEAD};
use halo_wire::unit::PackedState;

use crate::stats::Spread;

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
    ticks: BTreeMap<u32, TickReceipt>,
    /// Every state received with the tick of its datagram; kept only when asked.
    states: Vec<(u32, PackedState)>,
    datagrams: u64,
    wire_bytes: u64,
}

/// One simulated player.
pub struct SimPlayer {
    pub id: u16,
    socket: Arc<UdpSocket>,
    gateway: SocketAddr,
    seq: AtomicU32,
    impairment: Impairment,
    rng: Mutex<Rng>,
    delay_line: Option<Arc<DelayLine>>,
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
        let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").expect("bind a player socket"));
        socket.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        let log = Arc::new(Mutex::new(Log::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let player = SimPlayer {
            id,
            socket: socket.clone(),
            gateway,
            seq: AtomicU32::new(1),
            impairment,
            rng: Mutex::new(Rng::seeded(0x5eed ^ id as u64)),
            delay_line,
            log: log.clone(),
            stop: stop.clone(),
        };
        let mut rng = Rng::seeded(0xfeed ^ id as u64);
        let handle = std::thread::Builder::new()
            .name(format!("sim-player-{id}"))
            .spawn(move || {
                let mut buf = vec![0u8; 2048];
                let words = capacity.div_ceil(64);
                while !stop.load(Relaxed) {
                    let Ok(len) = socket.recv(&mut buf) else { continue };
                    let at = now_us();
                    if rng.next_f32() < impairment.loss {
                        continue;
                    }
                    let at = at + impairment.delay.as_micros() as i64;
                    let wire = (len + IP_UDP_OVERHEAD) as u32;
                    match ServerMessage::decode(&buf[..len]) {
                        Some(ServerMessage::Welcome(w)) => log.lock().unwrap().welcome = Some(w),
                        Some(ServerMessage::Snapshot(s)) => {
                            let mut log = log.lock().unwrap();
                            log.datagrams += 1;
                            log.wire_bytes += wire as u64;
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

    fn send_raw(&self, bytes: Vec<u8>) {
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

    pub fn hello(&self) {
        self.send_raw(ClientMessage::Hello { player: self.id }.encode());
    }

    /// Send an input with the next sequence number.
    pub fn send_input(&self, input: &PlayerInput) -> u32 {
        let seq = self.seq.fetch_add(1, Relaxed);
        self.send_input_with_seq(seq, input);
        seq
    }

    /// Send an input with a sequence number of the test's choosing.
    pub fn send_input_with_seq(&self, seq: u32, input: &PlayerInput) {
        self.send_raw(ClientMessage::Input { seq, input: *input }.encode());
    }

    /// The gateway's Welcome, if it came.
    pub fn welcome(&self) -> Option<Welcome> {
        self.log.lock().unwrap().welcome
    }

    /// The address this player sends from.
    pub fn local_addr(&self) -> SocketAddr {
        self.socket.local_addr().unwrap()
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

    /// Say hello for every player until each has been welcomed. Hellos are
    /// resent every 200 ms (a lossy link may drop them).
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
pub fn run_walk(client: &MatchClient, walkers: &mut Walkers, crowd: &Crowd, truth: &mut Truth, duration: Duration) {
    let until = Instant::now() + duration;
    while Instant::now() < until {
        let Some(seen) = client.next_tick(Duration::from_secs(10)) else { panic!("no tick for 10 s") };
        truth.record(&seen);
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
    /// The most ticks between two updates of the same pair.
    pub longest_gap_ticks: u32,
    /// Updates that came more than 100 ms (four ticks or more) after the pair's previous one.
    pub stalls_over_100ms: u64,
    pub stall_fraction: f64,
}

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
                "  {:>5.0} to {:<5} wu   {:>6.2} Hz   updated in {:>6.2}% of ticks   longest gap {:>3} ticks   gaps over 100 ms {:.3}%   ({} pairs)",
                b.from,
                if b.to.is_finite() { format!("{:.0}", b.to) } else { "...".into() },
                b.hz,
                b.fraction_updated * 100.0,
                b.longest_gap_ticks,
                b.stall_fraction * 100.0,
                b.pairs
            )?;
        }
        Ok(())
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
    let mut longest = vec![0u32; bands];
    let mut stalls = vec![0u64; bands];
    let mut last_update = vec![0u32; capacity * capacity];

    let mut ages = Vec::new();
    let mut bytes = vec![0u64; crowd.players.len()];
    let (mut missed, mut expected) = (0u64, 0u64);
    for &(&tick, at) in &ticks {
        for (index, player) in crowd.players.iter().enumerate() {
            let me = player.id as usize;
            let receipt = player.receipt(tick);
            expected += 1;
            let Some(receipt) = &receipt else {
                missed += 1;
                continue;
            };
            ages.push((receipt.first_at_us - at.stamped_us) as f64 / 1e3);
            bytes[index] += receipt.wire_bytes as u64;
            let Some(from) = at.positions[me] else { continue };
            for (other, position) in at.positions.iter().enumerate() {
                let Some(position) = position else { continue };
                if other == me {
                    continue;
                }
                let d = [position[0] - from[0], position[1] - from[1], position[2] - from[2]];
                let band = band_of(d[0] * d[0] + d[1] * d[1] + d[2] * d[2]);
                pairs[band] += 1;
                if receipt.has_state_of(other as u16) {
                    updated[band] += 1;
                    let slot = &mut last_update[me * capacity + other];
                    if *slot != 0 {
                        let gap = tick - *slot;
                        longest[band] = longest[band].max(gap);
                        // a gap of g ticks is g / 30 s
                        if gap as f64 * 1000.0 / TICKS_PER_SECOND as f64 > 100.0 {
                            stalls[band] += 1;
                        }
                    }
                    *slot = tick;
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
                longest_gap_ticks: longest[i],
                stalls_over_100ms: stalls[i],
                stall_fraction: stalls[i] as f64 / updated[i].max(1) as f64,
            })
            .collect(),
    }
}
