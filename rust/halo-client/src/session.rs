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
    join, leave, report_ammo, report_hits, use_item, DbConnection, FighterRow, FighterTableAccess, GameStateRow,
    GameStateTableAccess, ItemRow, ItemTableAccess, KitRow, KitTableAccess, MapInfo, MapInfoTableAccess,
    PlayerTableAccess, PowerupRow, PowerupTableAccess, RemoteReducers, RosterRow, RosterTableAccess, Seat,
    SeatTableAccess, StandingRow, StandingTableAccess,
};
use halo_match_driver::PlayerRow;
use halo_sim::combat::{Fighter, HitReport, Loadout};
use halo_sim::damage::Vitals;
use halo_sim::items::{Ammo, Item, Kit};
use halo_sim::wire::encode_hits;
use halo_sim::{MapData, PlayerInput};
use halo_wire::auth::{self, CHALLENGE_LIFETIME_US, SEED_SIZE};
use halo_wire::datagram::{
    seq_newer, Ack, Challenge, ClientMessage, ServerMessage, Welcome, MAX_DATAGRAM, REFUSED_BAD_PROOF, REFUSED_NO_SEAT,
    REFUSED_STALE,
};
use halo_wire::unit::{Bounds, UnitState};
use spacetimedb_sdk::{Compression, DbContext, Table, TableWithPrimaryKey};

use crate::identity::{is_rejected_token, IdentityFile};
use crate::local::MapHandle;
use crate::remote::{self, Track};

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
    /// before; `None` is the one kept in `identity` or, with none kept, a new
    /// one. (A session keeps the token of the identity it got, to come back as
    /// it after a dropped connection.)
    pub token: Option<String>,
    /// Where the identity's token is kept between sessions: read when the
    /// session starts, written when the server has said who it is.
    pub identity: IdentityFile,
}

/// Why the match would not seat the player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalKind {
    /// The identity is banned (from this match, or from the server).
    Banned,
    /// The match holds all the players it may.
    Full,
    /// Anything else: no spawn point, a bad key.
    Other,
}

/// A refusal of `join`, with what to tell the player.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub kind: RefusalKind,
    pub message: String,
}

impl Refusal {
    /// What a match's `join` said (`banned: <reason>`, `full: <why>`, or
    /// something else), as a refusal with a message fit to show.
    pub fn from_join_error(text: &str) -> Refusal {
        if let Some(reason) = text.strip_prefix("banned: ") {
            Refusal { kind: RefusalKind::Banned, message: format!("you are banned from this server: {reason}") }
        } else if let Some(why) = text.strip_prefix("full: ") {
            Refusal { kind: RefusalKind::Full, message: format!("this server is full ({why})") }
        } else {
            Refusal { kind: RefusalKind::Other, message: format!("the server would not let you join: {text}") }
        }
    }
}

/// The newest state the gateway sent of one other player.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RemoteUnit {
    pub state: UnitState,
    /// The tick of the datagram that carried it.
    pub tick: u32,
}

/// A player in range as the game draws them: the held state, and where the
/// player is drawn by now (see [`crate::remote`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DrawnUnit {
    /// The newest state the gateway sent: what the facing, the flags and the player are.
    pub state: UnitState,
    /// The tick of the datagram that carried it: the state's age is the frame's tick less this.
    pub tick: u32,
    /// Where the player is drawn, world units: the state's position carried forward to now.
    pub position: [f32; 3],
    /// The velocity to hand the engine, world units a second: the state's, or zero for a player
    /// the extrapolation has given up on.
    pub velocity: [f32; 3],
    /// How far the position the player was drawn at was from where the held state's extrapolation
    /// put them, when that state arrived (0 for a player's first): what a late update cost.
    pub arrival_error: f32,
}

/// Who a player is, from the match's roster.
#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    /// The engine's number for the team: 0 red, 1 blue.
    pub team: u8,
    pub name: String,
}

/// Whether a player is in the world: how the match's `standing` says they are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifeState {
    Alive,
    /// Dead until the tick `due_tick` of their [`Standing`].
    Dead,
    /// No starting location was free: they spawn in the wave at `due_tick`.
    Waiting,
}

/// How a player is doing, from the match's `standing` table: the scoreboard,
/// and whether they are in the world and when they will be. The server holds
/// all of it (spawns, deaths, score).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Standing {
    pub team: u8,
    pub score: i32,
    pub deaths: u32,
    pub state: LifeState,
    /// The server tick (the one the gateway's datagrams carry) the player
    /// spawns on, when dead (their respawn timer's end) or waiting (the
    /// wave); 0 when alive.
    pub due_tick: u64,
    /// Counts the player's spawns: a change says they are somewhere new.
    pub spawns: u32,
    /// The tick the player last spawned on: a state of them from before it is
    /// from where they were.
    pub spawned_tick: u64,
    /// Where the player last spawned: `x y z yaw`.
    pub spawn: [f32; 4],
}

impl Standing {
    fn from_row(row: &StandingRow) -> Standing {
        Standing {
            team: row.team,
            score: row.score,
            deaths: row.deaths,
            state: match row.state {
                0 => LifeState::Alive,
                1 => LifeState::Dead,
                _ => LifeState::Waiting,
            },
            due_tick: row.due_tick,
            spawns: row.spawns,
            spawned_tick: row.spawned_tick,
            spawn: [row.x, row.y, row.z, row.yaw],
        }
    }
}

/// The game, from the match's `game_state` table: its rules, its clock, the
/// team scores and how it ended.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct GameInfo {
    /// Team Slayer.
    pub teams: bool,
    pub score_limit: u32,
    /// Ticks the match lasts from `started_tick`; 0 for no limit.
    pub time_limit_ticks: u32,
    pub started_tick: u64,
    /// The match's tick as of the latest word of it (twice a second).
    pub tick: u64,
    /// Ticks between the waves.
    pub wave_ticks: u32,
    pub red_score: i32,
    pub blue_score: i32,
    /// 0 while it is on; 1 when a score limit ended it, 2 a time limit.
    pub ending: u8,
    /// 0 nobody, 1 a player, 2 a team.
    pub winner_kind: u8,
    pub winner: u16,
}

impl GameInfo {
    fn from_row(row: &GameStateRow) -> GameInfo {
        GameInfo {
            teams: row.teams,
            score_limit: row.score_limit,
            time_limit_ticks: row.time_limit_ticks,
            started_tick: row.started_tick,
            tick: row.tick,
            wave_ticks: row.wave_ticks,
            red_score: row.red_score,
            blue_score: row.blue_score,
            ending: row.ending,
            winner_kind: row.winner_kind,
            winner: row.winner,
        }
    }
}

/// The other players as of now, sorted by player id: those in range. A player
/// is in range while they are on the match's roster (one the gateway has sent
/// but the roster does not hold yet is not shown, and one who has left the
/// match is gone at once), they are alive, and the gateway has sent a state of
/// them from after they spawned, within [`OUT_OF_RANGE_TICKS`] of the newest
/// tick; one the gateway sends again is back.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Frame {
    /// The newest tick any datagram has carried; 0 before the first.
    pub tick: u32,
    pub units: Vec<DrawnUnit>,
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
    /// Hits the game reported, and the calls of `report_hits` they went in.
    pub hits_reported: u64,
    pub hit_calls: u64,
}

#[derive(Default)]
struct Shared {
    /// The player this session is, from the seat the module gave.
    player: Option<u16>,
    welcome: Option<Welcome>,
    /// The gateway's challenge to answer, and when it came.
    challenge: Option<(Challenge, Instant)>,
    units: BTreeMap<u16, RemoteUnit>,
    /// What each held state's arrival left to fade (see [`crate::remote`]).
    tracks: BTreeMap<u16, Track>,
    /// When `newest_tick` came: the ticks since are the time the extrapolation runs on.
    newest_at: Option<Instant>,
    /// Everyone in the match, from the direct connection.
    roster: BTreeMap<u16, Member>,
    /// How each of them is doing (score, alive, when they spawn), likewise.
    standings: BTreeMap<u16, Standing>,
    /// Each one's health, shields and weapons, likewise.
    fighters: BTreeMap<u16, Fighter>,
    /// Hits the game has reported, not yet sent (the network thread sends them
    /// over the direct connection, which is reliable).
    hits: Vec<HitReport>,
    /// The items on the ground as the server last wrote them (a falling one is where its row says it began:
    /// see `halo_sim::items`), who is camouflaged until which server tick, and this player's own rounds.
    items: BTreeMap<u32, Item>,
    powerups: BTreeMap<u16, u64>,
    kit: Option<Kit>,
    /// The game, likewise, and when its row last came.
    game: Option<GameInfo>,
    game_at: Option<Instant>,
    newest_tick: u32,
    /// The Snapshots received, which every Input says.
    ack: Ack,
    last_snapshot: Option<Instant>,
    /// The latest Input, and when it was sent: what the keepalive repeats.
    last_input: Option<([f32; 3], f32, f32, u8)>,
    last_input_at: Option<Instant>,
    counters: Counters,
    slow: Slow,
    /// Why the match turned the player away, while it does.
    refusal: Option<Refusal>,
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
    /// That identity, hex, once the server has said.
    identity: Mutex<Option<String>>,
    /// The SpacetimeDB connection, for the thread that stops the session to
    /// leave and close.
    connection: Mutex<Option<DbConnection>>,
    /// The player's own copy of the map, once it is being read: the ground the players who are in
    /// the air are drawn down to.
    ground: Mutex<Option<MapHandle>>,
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
    fn send_input(&self, shared: &mut Shared, player: u16, position: [f32; 3], yaw: f32, pitch: f32, flags: u8) {
        let seq = self.seq.fetch_add(1, Relaxed);
        let input = PlayerInput { player, position, yaw, pitch, flags };
        self.send(&ClientMessage::Input { seq, input, ack: shared.ack });
        shared.last_input = Some((position, yaw, pitch, flags));
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
        let token = config.token.clone().or_else(|| config.identity.load());
        let inner = Arc::new(Inner {
            token: Mutex::new(token),
            identity: Mutex::new(None),
            config,
            gateway,
            socket,
            seed,
            shared: Mutex::new(Shared::default()),
            seq: AtomicU32::new(1),
            stop: AtomicBool::new(false),
            connection: Mutex::new(None),
            ground: Mutex::new(None),
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

    /// Why the match has not given the player a seat (banned, full), until it does.
    pub fn refusal(&self) -> Option<Refusal> {
        self.inner.shared().refusal.clone()
    }

    /// The SpacetimeDB identity this session is, hex, once the server has said.
    pub fn identity(&self) -> Option<String> {
        self.inner.identity.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Where to find the ground for the players in the air: the map the local player moves on, which
    /// the game loads on a thread of its own (until it is in, they are drawn for a few ticks only).
    pub fn use_map(&self, map: MapHandle) {
        *self.inner.ground.lock().unwrap_or_else(|p| p.into_inner()) = Some(map);
    }

    fn ground(&self) -> Option<MapHandle> {
        self.inner.ground.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Every other player in range, with where each is drawn now.
    pub fn frame(&self) -> Frame {
        self.frame_at(Instant::now())
    }

    /// [`Session::frame`] at a time of the caller's.
    pub fn frame_at(&self, at: Instant) -> Frame {
        let ground = self.ground();
        let map = ground.as_ref().and_then(|g| g.get()).and_then(|r| r.as_ref().ok());
        frame_with(&self.inner.shared(), at, map)
    }

    /// Who a player is, if the roster has them.
    pub fn member(&self, player: u16) -> Option<Member> {
        self.inner.shared().roster.get(&player).cloned()
    }

    /// How a player is doing, if the match has them.
    pub fn standing(&self, player: u16) -> Option<Standing> {
        self.inner.shared().standings.get(&player).copied()
    }

    /// How this session's own player is doing, once the match has said.
    pub fn life(&self) -> Option<Standing> {
        let shared = self.inner.shared();
        shared.standings.get(&shared.player?).copied()
    }

    /// A player's health, shields and weapons, as the match last said (the
    /// shield's recharge since is [`Fighter::vitals_at`]'s to count).
    pub fn fighter(&self, player: u16) -> Option<Fighter> {
        self.inner.shared().fighters.get(&player).copied()
    }

    /// Report a hit the game's engine saw the local player's weapon make: `damage` is the damage
    /// effect's tag index (what hurt the target: a bullet, an explosion, a blow), `material` the part
    /// of `target` hit (-1 for none), `scale` the scale the engine dealt it at, `origin`
    /// where the shot hit and `target_position` where the engine has the
    /// target. The report is sent over the direct connection with the others of
    /// the tick (the server's `report_hits`), and made at the newest server tick
    /// the client has heard of. Nothing is sent (false) for a session with no
    /// seat or no word from the gateway yet.
    pub fn report_hit(
        &self,
        target: u16,
        damage: u16,
        material: i16,
        scale: f32,
        origin: [f32; 3],
        target_position: [f32; 3],
    ) -> bool {
        let mut shared = self.inner.shared();
        if shared.player.is_none() || shared.welcome.is_none() {
            return false;
        }
        let host_tick = shared.newest_tick;
        shared.hits.push(HitReport { target, damage, material, scale, host_tick, origin, target_position });
        true
    }

    /// The newest server tick any datagram has carried; 0 before the first.
    pub fn newest_tick(&self) -> u32 {
        self.inner.shared().newest_tick
    }

    /// The items on the ground, as the match's `item` table last said, by id.
    pub fn items(&self) -> Vec<Item> {
        self.inner.shared().items.values().copied().collect()
    }

    /// The server tick a player's camouflage runs out at, or 0 for a player who is not camouflaged.
    pub fn camouflaged_until(&self, player: u16) -> u64 {
        self.inner.shared().powerups.get(&player).copied().unwrap_or(0)
    }

    /// This player's own rounds, as the server tracks them (the rounds in the
    /// magazine and in reserve of each slot of the loadout; `version` changes when the server changed them).
    pub fn kit(&self) -> Option<Kit> {
        self.inner.shared().kit
    }

    /// The player pressed the action button, with the weapon slot (0 or 1) in hand: the server gives
    /// them what they reach, if its rules say so. Sent over the direct connection (reliable); false
    /// without a connection or a seat.
    pub fn use_item(&self, slot: u8) -> bool {
        if self.inner.shared().player.is_none() {
            return false;
        }
        let connection = self.inner.connection.lock().unwrap_or_else(|p| p.into_inner());
        connection.as_ref().is_some_and(|c| c.reducers.use_item(slot).is_ok())
    }

    /// Say how many rounds the player's weapons have (the game's engine counts them as it fires), loaded
    /// and in reserve for slot 0 and slot 1: what the server needs for what a swap puts down and for how many
    /// rounds an ammunition pickup can give.
    pub fn report_ammo(&self, ammo: [Ammo; 2]) -> bool {
        if self.inner.shared().player.is_none() {
            return false;
        }
        let mut bytes = Vec::with_capacity(8);
        for a in ammo {
            bytes.extend_from_slice(&a.loaded.to_le_bytes());
            bytes.extend_from_slice(&a.reserve.to_le_bytes());
        }
        let connection = self.inner.connection.lock().unwrap_or_else(|p| p.into_inner());
        connection.as_ref().is_some_and(|c| c.reducers.report_ammo(bytes).is_ok())
    }

    /// Everyone in the match with how they are doing and who they are, by
    /// player id: what the scoreboard lists, in range or not.
    pub fn scoreboard(&self) -> Vec<(u16, Standing, Member)> {
        scoreboard_of(&self.inner.shared())
    }

    /// The game: its rules, clock and scores, once the match has said.
    pub fn game(&self) -> Option<GameInfo> {
        self.inner.shared().game
    }

    /// The match's tick now (30 a second): what the game's row says, which is
    /// written twice a second, and the time since it came. A player who is not
    /// in the world is sent no datagrams (the gateway sends the states to the
    /// players there are), so the datagrams' tick would not be there for the
    /// countdown to a respawn wave. 0 before the match has said.
    pub fn server_tick(&self) -> u32 {
        let shared = self.inner.shared();
        match (shared.game, shared.game_at) {
            (Some(game), Some(at)) => {
                (game.tick + (at.elapsed().as_secs_f64() * halo_sim::TICKS_PER_SECOND as f64) as u64) as u32
            }
            _ => shared.newest_tick,
        }
    }

    /// Tell the gateway where this player is now; each call is the next
    /// input. `flags` is what the player says of themselves
    /// (`halo_sim::FLAG_CROUCHED`). Not sent (false) before the gateway has
    /// welcomed the player.
    pub fn send_input(&self, position: [f32; 3], yaw: f32, pitch: f32, flags: u8) -> bool {
        let mut shared = self.inner.shared();
        let (Some(player), Some(_)) = (shared.player, shared.welcome) else { return false };
        self.inner.send_input(&mut shared, player, position, yaw, pitch, flags);
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

/// Every player of the match as of `shared`, from the slow state alone: the
/// scoreboard lists the players the gateway does not send (the ones out of
/// range) as it does the others.
fn scoreboard_of(shared: &Shared) -> Vec<(u16, Standing, Member)> {
    shared
        .standings
        .iter()
        .filter_map(|(id, standing)| Some((*id, *standing, shared.roster.get(id)?.clone())))
        .collect()
}

/// The players in range as of `shared`, with no map to draw the ones in the air down to.
#[cfg(test)]
fn frame_of(shared: &Shared) -> Frame {
    frame_with(shared, Instant::now(), None)
}

/// How old a state of tick `tick` is at `at`, in ticks: the ticks behind the newest, and the time
/// since the newest came.
fn age_of(shared: &Shared, tick: u32, at: Instant) -> f32 {
    let behind = shared.newest_tick.wrapping_sub(tick) as i32 as f32;
    let since = shared.newest_at.map_or(0.0, |newest| ticks_between(newest, at));
    behind + since
}

fn ticks_between(from: Instant, to: Instant) -> f32 {
    to.saturating_duration_since(from).as_secs_f32() * halo_sim::TICKS_PER_SECOND as f32
}

/// Where a player is drawn at `at`.
fn draw(shared: &Shared, unit: &RemoteUnit, at: Instant, map: Option<&MapData>) -> remote::Drawn {
    let age = age_of(shared, unit.tick, at);
    let late = shared.tracks.get(&unit.state.player).map(|t| (t, ticks_between(t.arrived, at)));
    remote::drawn(unit, age, late, map)
}

/// A state of a player the gateway has sent has come, at `at`, in the datagram of tick `tick` (the
/// newest tick, `shared.newest_tick`, already says so if it is newer than the ones before). What
/// the player was drawn at against where the new state puts them is kept to fade; for a first
/// state, or one of a player who was out of range, the player is drawn where it says.
fn take_state(shared: &mut Shared, state: UnitState, tick: u32, at: Instant, map: Option<&MapData>) {
    let player = state.player;
    let known = shared.units.get(&player).copied();
    if known.is_some_and(|k| !seq_newer(tick, k.tick)) {
        return;
    }
    let new = RemoteUnit { state, tick };
    let mut track = Track { offset: [0.0; 3], error: 0.0, step: remote::correction_step(state.velocity), arrived: at };
    let in_range = |unit: &RemoteUnit| age_of(shared, unit.tick, at) <= OUT_OF_RANGE_TICKS as f32;
    if let Some(old) = known.filter(in_range) {
        let was = draw(shared, &old, at, map).position;
        let should = remote::drawn(&new, age_of(shared, tick, at), None, map).position;
        track.error = remote::length([was[0] - should[0], was[1] - should[1], was[2] - should[2]]);
        if let Some(offset) = remote::late_offset(was, should) {
            track.offset = offset;
        }
    }
    shared.tracks.insert(player, track);
    shared.units.insert(player, new);
}

/// The players in range as of `shared`, and where each is drawn at `at`.
fn frame_with(shared: &Shared, at: Instant, map: Option<&MapData>) -> Frame {
    let newest = shared.newest_tick;
    let in_range = |unit: &&RemoteUnit| {
        let in_the_world = shared.standings.get(&unit.state.player).is_none_or(|s| {
            // (a state from before the player spawned is from where they were)
            s.state == LifeState::Alive && (unit.tick.wrapping_sub(s.spawned_tick as u32) as i32) >= 0
        });
        shared.roster.contains_key(&unit.state.player)
            && in_the_world
            && (newest.wrapping_sub(unit.tick) as i32) <= OUT_OF_RANGE_TICKS as i32
    };
    let units = shared
        .units
        .values()
        .filter(in_range)
        .map(|unit| {
            let drawn = draw(shared, unit, at, map);
            DrawnUnit {
                state: unit.state,
                tick: unit.tick,
                position: drawn.position,
                velocity: drawn.velocity,
                arrival_error: shared.tracks.get(&unit.state.player).map_or(0.0, |t| t.error),
            }
        })
        .collect();
    Frame { tick: newest, units }
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
        flush_hits(inner);
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

/// The hits the game has reported since the last time, as calls of the match's
/// `report_hits` over the direct connection (reliable, unlike the UDP the
/// positions travel by: a hit lost is a kill that does not count), at most
/// as many to a call as the module takes.
fn flush_hits(inner: &Inner) {
    let hits = {
        let mut shared = inner.shared();
        if shared.hits.is_empty() {
            return;
        }
        std::mem::take(&mut shared.hits)
    };
    let connection = inner.connection.lock().unwrap_or_else(|p| p.into_inner());
    let Some(connection) = connection.as_ref() else {
        // (no connection: the hits are too late to be worth keeping)
        return;
    };
    for chunk in hits.chunks(halo_sim::wire::MAX_HITS_PER_CALL) {
        let sent = connection.reducers.report_hits(encode_hits(chunk)).is_ok();
        let mut shared = inner.shared();
        if sent {
            shared.counters.hits_reported += chunk.len() as u64;
            shared.counters.hit_calls += 1;
        } else {
            shared.error = Some("a hit report could not be sent".into());
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
                let at = shared.last_input.or(shared.slow.local.map(|l| ([l[0], l[1], l[2]], l[3], l[4], 0)));
                if let Some((position, yaw, pitch, flags)) = at {
                    inner.send_input(&mut shared, player, position, yaw, pitch, flags);
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
            let at = Instant::now();
            if shared.newest_tick == 0 || seq_newer(snapshot.tick, shared.newest_tick) {
                shared.newest_tick = snapshot.tick;
                shared.newest_at = Some(at);
            }
            let ground = inner.ground.lock().unwrap_or_else(|p| p.into_inner()).clone();
            let map = ground.as_ref().and_then(|g| g.get()).and_then(|r| r.as_ref().ok());
            for packed in &snapshot.states {
                let state = packed.unpack(&welcome.bounds);
                if state.player == player {
                    continue;
                }
                take_state(&mut shared, state, snapshot.tick, at, map);
            }
        }
        Some(_) => {}
        None => shared.counters.ignored += 1,
    }
}

/// An item on the ground from the match's `item` row.
fn item_of(row: &ItemRow) -> Item {
    Item {
        id: row.id,
        tag: row.tag,
        position: [row.x, row.y, row.z],
        velocity: [row.vx, row.vy, row.vz],
        tick: row.tick,
        resting: row.resting,
        placement: row.placement,
        loaded: row.loaded,
        reserve: row.reserve,
        last_owned: row.last_owned,
        ignore: row.ignore,
    }
}

/// A player's health, shields and weapons from the match's `fighter` row.
fn fighter_of(row: &FighterRow) -> Fighter {
    Fighter {
        id: row.player,
        vitals: Vitals {
            shield: row.shield,
            body: row.body,
            shield_stun_ticks: row.shield_stun_ticks,
            flags: row.flags,
        },
        tick: row.tick,
        loadout: Loadout {
            weapons: [row.weapon_0, row.weapon_1],
            dropped: [(row.dropped_0, row.dropped_0_tick), (row.dropped_1, row.dropped_1_tick)],
        },
        hurt_tick: row.hurt_tick,
        hurt_by: row.hurt_by,
        hurt_count: row.hurt_count,
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
                    // no seat yet (no spawn point, a full match): ask again, unless
                    // the match has banned this identity, which no asking changes
                    let banned = inner.shared().refusal.as_ref().is_some_and(|r| r.kind == RefusalKind::Banned);
                    if inner.shared().player.is_none() && !banned && last_try.elapsed() >= RETRY_DELAY {
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
            Ok(Ok(())) => {
                failed.shared().refusal = None;
                return;
            }
            Ok(Err(message)) => message,
            Err(e) => {
                failed.shared().error = Some(format!("joining the match: {e}"));
                return;
            }
        };
        let refusal = Refusal::from_join_error(&reason);
        let mut shared = failed.shared();
        shared.error = Some(refusal.message.clone());
        shared.refusal = Some(refusal);
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
            connected.shared().standings.clear();
            connected.shared().fighters.clear();
            connected.shared().items.clear();
            connected.shared().powerups.clear();
            connected.shared().kit = None;
            *connected.token.lock().unwrap_or_else(|p| p.into_inner()) = Some(token.into());
            *connected.identity.lock().unwrap_or_else(|p| p.into_inner()) = Some(identity.to_hex().to_string());
            if let Err(e) = connected.config.identity.save(token) {
                connected.shared().error = Some(format!("the identity could not be kept: {e}"));
            }
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
                    "SELECT * FROM standing".to_string(),
                    "SELECT * FROM fighter".to_string(),
                    "SELECT * FROM game_state".to_string(),
                    // what is on the ground (a falling item is one row, from which the fall is worked
                    // out here: nothing a tick), and who is camouflaged
                    "SELECT * FROM item".to_string(),
                    "SELECT * FROM powerup".to_string(),
                    format!("SELECT * FROM seat WHERE owner = 0x{}", identity.to_hex()),
                ]);
            join(&connected, &connection.reducers);
        })
        .build()
        .map_err(|e| {
            let text = e.to_string();
            if is_rejected_token(&text) && inner.token.lock().unwrap_or_else(|p| p.into_inner()).take().is_some() {
                // the kept identity is no good here: the next try is a new one
                inner.config.identity.forget();
            }
            format!("SpacetimeDB {}: {text}", inner.config.spacetimedb)
        })?;

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
                shared.kit = None;
            }
            changed
        };
        if changed {
            let own = seated.clone();
            let id = seat.player;
            ctx.subscription_builder()
                .on_error(move |_, e| own.shared().error = Some(format!("the subscription to the player: {e}")))
                .subscribe([
                    format!("SELECT * FROM player WHERE id = {id}"),
                    // (the rounds are this player's own; the table is public, so this is only what the client asks for)
                    format!("SELECT * FROM kit WHERE player = {id}"),
                ]);
        }
    };
    let table = connection.db.seat();
    let on_seat_insert = on_seat.clone();
    table.on_insert(move |ctx, seat| on_seat_insert(ctx, seat));
    table.on_update(move |ctx, _, seat| on_seat(ctx, seat));
    // the seat taken away (a ban, a grace period that ran out): this session
    // has no player until it joins again, and the join says why not
    let unseated = inner.clone();
    table.on_delete(move |_, seat| {
        let mut shared = unseated.shared();
        if shared.player == Some(seat.player) {
            shared.player = None;
            shared.welcome = None;
            shared.challenge = None;
            shared.slow.local = None;
            shared.units.clear();
            shared.kit = None;
        }
    });

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

    // how everyone is doing, and the game: slow state like the roster (they change
    // when someone scores, dies or spawns, not with every move)
    let standings = inner.clone();
    let on_standing = move |row: &StandingRow| {
        standings.shared().standings.insert(row.player, Standing::from_row(row));
    };
    let table = connection.db.standing();
    let on_standing_insert = on_standing.clone();
    table.on_insert(move |_, row| on_standing_insert(row));
    table.on_update(move |_, _, row| on_standing(row));
    let gone = inner.clone();
    table.on_delete(move |_, row| {
        gone.shared().standings.remove(&row.player);
    });
    // each player's health, shields and weapons: written when they spawn, are
    // hurt or change what they carry, and not when a shield recharges
    let fighters = inner.clone();
    let on_fighter = move |row: &FighterRow| {
        fighters.shared().fighters.insert(row.player, fighter_of(row));
    };
    let table = connection.db.fighter();
    let on_fighter_insert = on_fighter.clone();
    table.on_insert(move |_, row| on_fighter_insert(row));
    table.on_update(move |_, _, row| on_fighter(row));
    let unfought = inner.clone();
    table.on_delete(move |_, row| {
        unfought.shared().fighters.remove(&row.player);
    });
    // the items on the ground, who is camouflaged, and this player's own rounds: slow state, written
    // when something appears, comes to rest, is taken or changes hands (not when an item falls)
    let items = inner.clone();
    let on_item = move |row: &ItemRow| {
        items.shared().items.insert(row.id, item_of(row));
    };
    let table = connection.db.item();
    let on_item_insert = on_item.clone();
    table.on_insert(move |_, row| on_item_insert(row));
    table.on_update(move |_, _, row| on_item(row));
    let taken = inner.clone();
    table.on_delete(move |_, row| {
        taken.shared().items.remove(&row.id);
    });
    let powerups = inner.clone();
    let on_powerup = move |row: &PowerupRow| {
        powerups.shared().powerups.insert(row.player, row.camo_until);
    };
    let table = connection.db.powerup();
    let on_powerup_insert = on_powerup.clone();
    table.on_insert(move |_, row| on_powerup_insert(row));
    table.on_update(move |_, _, row| on_powerup(row));
    let ended = inner.clone();
    table.on_delete(move |_, row| {
        ended.shared().powerups.remove(&row.player);
    });
    let kits = inner.clone();
    let on_kit = move |row: &KitRow| {
        let mut shared = kits.shared();
        if shared.player == Some(row.player) {
            shared.kit = Some(Kit {
                player: row.player,
                ammo: [
                    Ammo { loaded: row.loaded_0, reserve: row.reserve_0 },
                    Ammo { loaded: row.loaded_1, reserve: row.reserve_1 },
                ],
                camo_until: 0,
                version: row.version,
            });
        }
    };
    let table = connection.db.kit();
    let on_kit_insert = on_kit.clone();
    table.on_insert(move |_, row| on_kit_insert(row));
    table.on_update(move |_, _, row| on_kit(row));
    let game = inner.clone();
    let on_game = move |row: &GameStateRow| {
        let mut shared = game.shared();
        shared.game = Some(GameInfo::from_row(row));
        shared.game_at = Some(Instant::now());
    };
    let table = connection.db.game_state();
    let on_game_insert = on_game.clone();
    table.on_insert(move |_, row| on_game_insert(row));
    table.on_update(move |_, _, row| on_game(row));

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
        let state = UnitState {
            player,
            position: [1.0, 2.0, 3.0],
            velocity: [0.0; 3],
            yaw: 0.0,
            pitch: 0.0,
            tick: 0,
            flags: 0,
        };
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

    fn standing(state: LifeState, spawned_tick: u64) -> Standing {
        Standing { team: 0, score: 0, deaths: 0, state, due_tick: 0, spawns: 1, spawned_tick, spawn: [0.0; 4] }
    }

    #[test]
    fn a_dead_player_is_not_in_the_frame_and_one_who_has_spawned_is_from_their_spawn_on() {
        let mut shared = Shared { newest_tick: 1000, ..Shared::default() };
        for id in [1, 2, 3, 4] {
            shared.roster.insert(id, member());
            shared.units.insert(id, held(id, 1000));
        }
        shared.standings.insert(1, standing(LifeState::Alive, 10));
        shared.standings.insert(2, standing(LifeState::Dead, 10));
        shared.standings.insert(3, standing(LifeState::Waiting, 10));
        // spawned at tick 1000: a state of tick 999 is from where they were
        shared.standings.insert(4, standing(LifeState::Alive, 1000));
        shared.units.insert(4, held(4, 999));
        let ids = |frame: &Frame| frame.units.iter().map(|u| u.state.player).collect::<Vec<_>>();
        assert_eq!(ids(&frame_of(&shared)), vec![1]);
        // the state of the tick it spawned on is theirs
        shared.units.insert(4, held(4, 1000));
        assert_eq!(ids(&frame_of(&shared)), vec![1, 4]);
    }

    #[test]
    fn the_scoreboard_lists_every_player_of_the_match_the_gateway_sends_or_not() {
        let mut shared = Shared { newest_tick: 100, ..Shared::default() };
        for id in 0..300u16 {
            shared.roster.insert(id, Member { team: (id % 2) as u8, name: format!("Player {id}") });
            shared.standings.insert(id, standing(if id % 5 == 0 { LifeState::Dead } else { LifeState::Alive }, 1));
        }
        // the gateway sends ten of them
        for id in 1..=10u16 {
            shared.units.insert(id, held(id, 100));
        }
        let frame = frame_of(&shared);
        assert!(frame.units.len() <= 10, "{} players in range", frame.units.len());
        let board = scoreboard_of(&shared);
        assert_eq!(board.len(), 300, "everyone is on the scoreboard");
        assert!(board.iter().any(|(id, _, m)| *id == 299 && m.name == "Player 299"), "one who is nowhere near");
        // (a dead player is on it too, and says so)
        assert_eq!(board.iter().filter(|(_, s, _)| s.state == LifeState::Dead).count(), 60);
        // a player the roster does not have yet has no name to show
        shared.standings.insert(500, standing(LifeState::Alive, 1));
        assert_eq!(scoreboard_of(&shared).len(), 300);
    }

    #[test]
    fn a_player_the_roster_does_not_hold_is_not_in_the_frame() {
        let mut shared = Shared { newest_tick: 5, ..Shared::default() };
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

#[cfg(test)]
#[path = "remote_tests.rs"]
mod remote_tests;
