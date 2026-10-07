//! The match module: one SpacetimeDB database holds one large-scale match.
//!
//! - A scheduled reducer, [`tick`], runs `halo_sim::rules::play` (the game's
//!   rules and `halo_sim::step`) 30 times a second.
//! - [`submit_inputs`] takes every player's input for a tick as one batch (the
//!   gateway calls it once a tick); nothing is called per player.
//! - Each tick writes the players whose state changed to `player` and then the
//!   single `match_tick` row, all in one transaction, so a subscriber knows
//!   tick N is complete when it receives `match_tick` with `tick == N`: its
//!   player rows arrive in the same update.
//! - The map's collision data is one row of `map_blob`, the source of truth.
//!   It is decoded into module memory on first use and whenever `map_version`
//!   says the cache is out of date, so a module whose memory is fresh (after a
//!   restart or a republish) reloads it from the row.
//! - `map_info` (public) gives the gateway the map's world bounds, which its
//!   position packing is relative to.
//! - `roster` (public) says who each player is for the others to show: a
//!   name and a team. It changes only when a player joins or leaves, so every
//!   client subscribes to all of it, as it must not to `player`, which
//!   changes every tick.
//! - `standing` (public) says how each player is doing: score, deaths, whether
//!   they are in the world (alive, dead until a tick, or waiting for a
//!   respawn wave because no starting location is free) and where they last
//!   spawned. `game_state` (public, one row) is the game: its rules, clock, team
//!   scores and how it ended. Both are slow state, written when a player scores,
//!   dies or spawns (the clock in `game_state.tick` twice a second): clients
//!   subscribe to all of them for the scoreboard and for the player's own
//!   respawn, and the orchestration reads `game_state` for the match's end.
//! - `fighter` (public) is each living player's shield and health as of a tick
//!   (clients and the module count the recharge forward: nothing is written
//!   while a shield recharges), their weapons, and the last hit that hurt them.
//!   `shooter` and `hit_report` (private) are a shooter's bucket of hits and
//!   record of rejected reports, and the batches of reports waiting for the
//!   next tick.
//!
//! # The game
//!
//! The server owns spawns, deaths, score and the end (`halo_sim::rules`). The
//! owner sets the game (`set_game`: Slayer or Team Slayer, the score and time
//! limits, the respawn times and the waves) and starts its clock (`begin_game`).
//! A player who `join`s spawns at a starting location of the map, which the
//! map's data carries, by the engine's rules, or is told to wait for the next
//! wave when none is free. A death is applied by the next tick: `report_death`
//! queues it (the owner's way to kill), and the hits that kill become the same
//! `halo_sim::rules::Death`.
//!
//! # Fighting
//!
//! A player's own client decides its hits and says so with [`report_hits`]
//! (a reliable reducer on the player's own connection: a hit lost is a kill
//! lost, and the connection says who shot; at most 64 reports of 34 bytes a
//! call). The next tick judges them (`halo_sim::combat::resolve`, against where
//! the module saw the players over the last second, kept in module memory), deals
//! the damage of those that pass to the target's shield and health, and
//! passes a death to the rules with the shooter as killer. A report that fails
//! is counted in `shooter` and in `match_tick.rejected_hits` and logged. Each
//! spawn gives the player a fresh `fighter` with the starting weapon (the
//! owner's `set_loadout` replaces the weapons).
//!
//! # Who may call what
//!
//! - The **owner** (the identity that published the database, which `init`
//!   records) runs the match: `load_map`, `add_players`, `remove_players`,
//!   `start`, `stop`, `reset`, `set_game`, `begin_game`, `report_death`,
//!   `set_spawn_points`, `set_capacity`, `set_away_grace` and `set_gateway`.
//!   Any other caller is refused.
//! - The **gateway** (the owner until `set_gateway` names another identity)
//!   calls `submit_inputs`: nobody else may put input into the match.
//! - **Anyone** may `join` (take a seat: a player, tied to their identity, and
//!   the public key their UDP traffic is checked against) and `leave`. `join`
//!   refuses a banned identity (the owner's `set_ban`; a ban made in the root
//!   database reaches the match through the orchestration, which calls it) and
//!   a full match, each with a message that starts `banned: ` or `full: ` and
//!   says why.
//! - `tick` is called by the database itself, nobody else.
//!
//! # Seats
//!
//! A `seat` (public) ties a player to a SpacetimeDB identity: the identity
//! that `join`ed is the only one that may `leave`, and the only one to get
//! that player id back by `join`ing again, from any connection, after a
//! dropped one. The seat holds the player's UDP public key: the gateway
//! checks the proof a UDP address gives against it (`halo_wire::auth`). A seat
//! whose connection dropped is held for a grace period (30 seconds unless the
//! owner says otherwise), then the player is removed. The player's state stays in `player`, which changes every tick,
//! so the slow-changing seat is a separate table: the gateway and the
//! scoreboard read both.
//!
//! The tests in `rust/halo-match-driver` publish and drive it against a real
//! local SpacetimeDB.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Duration;

use halo_map::MapError;
use halo_sim::combat::{CombatStore, Fighter, HitEvent, Loadout, Shooter, Trails};
use halo_sim::damage::Vitals;
use halo_sim::items::{Ammo, Item, ItemId, ItemStore, Kit};
use halo_sim::pickups::{self, ItemEvent, Request};
use halo_sim::rules::{self, Contestant, Death, EndReason, Ending, Game, GameEvent, GameStore, Life, Rules, Winner};
use halo_sim::wire::{decode_hits, decode_inputs, HIT_SIZE, INPUT_SIZE, MAX_HITS_PER_CALL};
use halo_sim::{Event, MapData, Player, PlayerId, RejectReason, Rng, Store, TICKS_PER_SECOND};
use spacetimedb::{reducer, table, ConnectionId, Identity, ReducerContext, ScheduleAt, Table};

/// Microseconds between ticks.
const TICK_INTERVAL_US: u64 = 1_000_000 / TICKS_PER_SECOND as u64;

/// Batches that may wait for a tick before `submit_inputs` refuses more.
const MAX_PENDING_BATCHES: u64 = 16;

/// Hit reports in one `report_hits` call, and calls that may wait for a tick:
/// a client sends one call a tick with the hits of that tick.
const MAX_PENDING_HIT_CALLS: u64 = 4096;

/// The one row of a single-row table.
const ONLY: u8 = 0;

/// How long a seat is held, unless the owner says otherwise, for a player
/// whose connection dropped: 30 seconds.
pub const DEFAULT_AWAY_GRACE_TICKS: u64 = 30 * TICKS_PER_SECOND as u64;

/// Players a match holds unless the owner says otherwise.
pub const DEFAULT_CAPACITY: u16 = 500;

/// What a refusal to join starts with, so that a client can tell why: the
/// text after it is for the player.
pub const BANNED_PREFIX: &str = "banned: ";
pub const FULL_PREFIX: &str = "full: ";

/// Bytes of the UDP public key (Ed25519).
const UDP_KEY_SIZE: usize = 32;

/// The tick counter and the completion marker. Written last in every tick.
#[table(accessor = match_tick, public)]
pub struct MatchTick {
    #[primary_key]
    id: u8,
    /// Ticks run so far; this row's value N means every row of tick N is
    /// already visible, and arrived in the same update.
    tick: u64,
    /// Server time at the start of the tick, microseconds since the Unix epoch.
    stamped_us: i64,
    /// Players in the match after the tick: every player with a standing, dead or waiting for a wave too.
    players: u32,
    /// Inputs this tick took from the batches (accepted, rejected, or dropped because no map is loaded).
    inputs: u32,
    /// Inputs this tick rejected, including those for players who are not in
    /// the match (which no player row can count).
    rejected: u32,
    /// Rejections since the match was reset.
    rejected_total: u64,
    /// Hit reports this tick took that passed the server's checks and hurt
    /// someone, and those it refused (`halo_sim::combat::Reject`).
    hits: u32,
    rejected_hits: u32,
    /// Refused hit reports since the match was reset.
    rejected_hits_total: u64,
    /// Hit reports that passed and hurt someone since the match was reset, and the refused ones that
    /// were refused as `Reject::TargetNotWhereSeen` (the server had the target nowhere near where the
    /// shooter says it saw them): what the client's treatment of other players between updates is measured by.
    hits_total: u64,
    rejected_not_where_seen_total: u64,
}

/// One player. Position and facing are the last move the server accepted.
#[table(accessor = player, public)]
#[derive(Clone)]
pub struct PlayerRow {
    #[primary_key]
    id: u16,
    x: f32,
    y: f32,
    z: f32,
    yaw: f32,
    pitch: f32,
    /// The tick that last changed the position.
    updated_tick: u64,
    /// Moves the server rejected, since the player joined.
    rejected_moves: u64,
    /// Why the latest one was rejected: one of the `REJECT_*` codes.
    last_reject: u8,
    /// The tick of the latest rejection.
    last_reject_tick: u64,
    /// How the others are to show the player: `halo_sim::FLAG_AIRBORNE` (the
    /// server's judgement, from the moves it accepted) and
    /// `halo_sim::FLAG_CROUCHED` (the player's own report). The gateway sends
    /// it as the flags of the player's packed state.
    flags: u8,
    /// Where the player is in the air, which the airborne rule of the
    /// validation measures the next move against (`halo_sim::Player`'s).
    air_ticks: u32,
    air_z: f32,
    free_ticks: u32,
    free_z: f32,
}

pub const REJECT_NONE: u8 = 0;
pub const REJECT_UNKNOWN_PLAYER: u8 = 1;
pub const REJECT_NOT_FINITE: u8 = 2;
pub const REJECT_TOO_FAST: u8 = 3;
pub const REJECT_THROUGH_SURFACE: u8 = 4;
pub const REJECT_OFF_GROUND: u8 = 5;
pub const REJECT_DUPLICATE_INPUT: u8 = 6;

fn reject_code(reason: RejectReason) -> u8 {
    match reason {
        RejectReason::UnknownPlayer => REJECT_UNKNOWN_PLAYER,
        RejectReason::NotFinite => REJECT_NOT_FINITE,
        RejectReason::TooFast => REJECT_TOO_FAST,
        RejectReason::ThroughSurface => REJECT_THROUGH_SURFACE,
        RejectReason::OffGround => REJECT_OFF_GROUND,
        RejectReason::DuplicateInput => REJECT_DUPLICATE_INPUT,
    }
}

/// What a client needs to know about the loaded map without the map itself:
/// the world bounds that position quantisation is relative to. Public; one
/// row, rewritten by every `load_map`.
#[table(accessor = map_info, public)]
pub struct MapInfo {
    #[primary_key]
    id: u8,
    /// Counts loads, as `match_state.map_version` does.
    version: u64,
    x0: f32,
    x1: f32,
    y0: f32,
    y1: f32,
    z0: f32,
    z1: f32,
}

/// Two teams, as the engine numbers them: red is 0 and blue is 1.
pub const TEAMS: u8 = 2;

/// Who a player is, for the other players' screens. One row per player in the
/// match; public. Slow state: written when a player joins or is added, and
/// deleted when they go.
#[table(accessor = roster, public)]
pub struct RosterRow {
    #[primary_key]
    player: u16,
    /// 0 (red) or 1 (blue).
    team: u8,
    /// What the others see over the player's head (at most 11 characters:
    /// the engine's name field).
    name: String,
}

/// What a player's life is: the values of [`StandingRow::state`].
pub const STATE_ALIVE: u8 = 0;
/// Dead, until `due_tick`.
pub const STATE_DEAD: u8 = 1;
/// Waiting for the wave at `due_tick`: no starting location was free.
pub const STATE_WAITING: u8 = 2;

/// How a player is doing: the scoreboard, and whether they are in the world
/// and when they will be. One row per player in the match; public, and slow
/// state: it changes when the player scores, dies or spawns, and not with
/// every move. Clients subscribe to all of it (they show every player on the
/// scoreboard, in range or not).
#[table(accessor = standing, public)]
pub struct StandingRow {
    #[primary_key]
    player: u16,
    /// 0 (red) or 1 (blue), as on the roster.
    team: u8,
    /// Kills, less what betrayals and suicides cost.
    score: i32,
    deaths: u32,
    /// `STATE_ALIVE`, `STATE_DEAD` or `STATE_WAITING`.
    #[index(btree)]
    state: u8,
    /// The match tick the player spawns on, when dead (the respawn timer's
    /// end) or waiting (the wave); 0 when alive. The match tick is the one
    /// the gateway's datagrams carry.
    due_tick: u64,
    /// Counts the player's spawns: a change says they are somewhere new.
    spawns: u32,
    /// The match tick the player last spawned on: a state of them sent before it is
    /// from where they were.
    spawned_tick: u64,
    /// Where the player last spawned, and which way they faced.
    x: f32,
    y: f32,
    z: f32,
    yaw: f32,
    /// Ticks added to the player's respawn timer.
    penalty: u32,
}

/// The game: its rules, its clock and the team scores, and how it ended.
/// One row; public; it changes when a team scores or the match ends.
#[table(accessor = game_state, public)]
pub struct GameStateRow {
    #[primary_key]
    id: u8,
    /// Team Slayer.
    teams: bool,
    score_limit: u32,
    /// Ticks the match lasts from `started_tick`; 0 for no limit.
    time_limit_ticks: u32,
    respawn_ticks: u32,
    respawn_growth_ticks: u32,
    suicide_penalty_ticks: u32,
    /// Ticks between the waves players spawn in when no starting location is free.
    wave_ticks: u32,
    /// The match tick the clock started on.
    started_tick: u64,
    /// The match tick as of the latest `CLOCK_EVERY_TICKS`: a client that is not
    /// being sent the match's datagrams (a player who is waiting for a wave) keeps
    /// the time from it, between updates, by its own clock.
    tick: u64,
    red_score: i32,
    blue_score: i32,
    /// `ENDING_NONE` while the match is on; `ENDING_SCORE_LIMIT` or `ENDING_TIME_LIMIT`.
    ending: u8,
    /// `WINNER_NOBODY`, `WINNER_PLAYER` or `WINNER_TEAM`, once it has ended.
    winner_kind: u8,
    /// The winning player's id, or team.
    winner: u16,
}

/// How often `game_state.tick` is written: twice a second.
pub const CLOCK_EVERY_TICKS: u64 = TICKS_PER_SECOND as u64 / 2;

pub const ENDING_NONE: u8 = 0;
pub const ENDING_SCORE_LIMIT: u8 = 1;
pub const ENDING_TIME_LIMIT: u8 = 2;
pub const WINNER_NOBODY: u8 = 0;
pub const WINNER_PLAYER: u8 = 1;
pub const WINNER_TEAM: u8 = 2;

/// A player's health and shields, and what they carry. One row per player in
/// the match; public, and slow state: it is written when the player spawns,
/// is hurt, or changes what they carry, and not when their shield recharges
/// (a client counts that: see `halo_sim::damage`). Clients subscribe to all of
/// it, to show a player's shield and health and what they are holding.
#[table(accessor = fighter, public)]
pub struct FighterRow {
    #[primary_key]
    player: u16,
    /// Fractions of a full shield and of full health (a shield beyond 1 is an
    /// overshield; a body below 0 is dead), as of the match tick `tick`.
    shield: f32,
    body: f32,
    /// Ticks the shield will not recharge for.
    shield_stun_ticks: i16,
    /// `halo_sim::damage`'s `SHIELD_DEPLETED`, `DEAD`, `SHIELD_OVER_CHARGING`
    /// and `SHIELD_CHARGING`.
    flags: u8,
    tick: u64,
    /// The weapons the player carries, by tag index (`NO_WEAPON`, 65535, for
    /// none), and the ones they put down lately, with the tick.
    weapon0: u16,
    weapon1: u16,
    dropped0: u16,
    dropped0_tick: u64,
    dropped1: u16,
    dropped1_tick: u64,
    /// The tick of the hit that last hurt the player, who it was by (65535 if
    /// none has), and how many hits have hurt them since they spawned: a
    /// client shows a player is hit when the count changes.
    hurt_tick: u64,
    hurt_by: u16,
    hurt_count: u32,
}

/// What became of a shooter's hit reports. One row per player who has had one
/// judged; private (the operator's log has the counts, and the owner's
/// reducers read them).
#[table(accessor = shooter)]
pub struct ShooterRow {
    #[primary_key]
    player: u16,
    /// Seconds of fire in the bucket their hits draw from, as of `hit_seconds_tick`.
    hit_seconds: f32,
    hit_seconds_tick: u64,
    /// Reports that passed the checks and reports that did not.
    accepted: u64,
    rejected: u64,
    /// Why the latest refusal was (`halo_sim::combat::Reject::code`) and when.
    last_reject: u8,
    last_reject_tick: u64,
}

/// A client's `report_hits` call, waiting for the next tick: the player it
/// came from (by their seat) and the hits (`halo_sim::wire::encode_hits`).
/// Private.
#[table(accessor = hit_report)]
pub struct HitReportRow {
    #[primary_key]
    #[auto_inc]
    id: u64,
    shooter: u16,
    data: Vec<u8>,
}

/// An item on the ground: a placement's weapon or powerup, a weapon someone
/// put down. Public, and slow state: a row is written when an item appears,
/// when it comes to rest (a falling one is *not* written every tick: where it
/// is in between is a function of the row, which every client works out with
/// the same `halo_sim::items::Item::advanced_to`), when it loses rounds to a
/// player who takes them, and deleted when it is taken or goes. Clients
/// subscribe to all of it.
#[table(accessor = item, public)]
pub struct ItemRow {
    #[primary_key]
    #[auto_inc]
    id: u32,
    /// The weapon's or equipment's tag index (`halo_map::items::ItemDef`).
    tag: u16,
    /// Where the item was, and how it was moving (world units a tick), at the
    /// match tick `tick`: an item at rest (`resting`) is where it is.
    x: f32,
    y: f32,
    z: f32,
    vx: f32,
    vy: f32,
    vz: f32,
    tick: u64,
    resting: bool,
    /// The placement that made it (an index of the map's netgame equipment),
    /// 65535 for a weapon that was put down.
    placement: u16,
    /// A weapon's rounds, in the magazine and in reserve.
    loaded: i16,
    reserve: i16,
    last_owned: u64,
    /// The player who put it down, who cannot take it until it rests (65535: nobody).
    ignore: u16,
}

/// The players who are camouflaged, and until which match tick. Public: every
/// client shows it on the players it draws. A row is there only while the
/// camouflage is.
#[table(accessor = powerup, public)]
pub struct PowerupRow {
    #[primary_key]
    player: u16,
    camo_until: u64,
}

/// The rounds of the weapons a player carries, slot by slot, as the server
/// tracks them (a client reports its own as it fires: `report_ammo`). Public
/// (the rounds are no secret), but a client subscribes to its own row only, so
/// the others' reports are not sent to it: `version` changes when the
/// server changed the rounds (a pickup, a spawn), which is when the client
/// takes them.
#[table(accessor = kit, public)]
pub struct KitRow {
    #[primary_key]
    player: u16,
    loaded_0: i16,
    reserve_0: i16,
    loaded_1: i16,
    reserve_1: i16,
    version: u32,
}

/// A player's press of the action button, waiting for the next tick: the weapon
/// slot in hand (what a swap puts down). One a player; private.
#[table(accessor = use_request)]
pub struct UseRequest {
    #[primary_key]
    player: u16,
    slot: u8,
}

/// What `report_death` was told to apply at the next tick. Private.
#[table(accessor = pending_death)]
pub struct PendingDeath {
    #[primary_key]
    #[auto_inc]
    id: u64,
    victim: u16,
    /// `NO_KILLER` for a death nobody caused.
    killer: u16,
}

/// A `report_death` killer that says nobody.
pub const NO_KILLER: u16 = u16::MAX;

/// Which map the cache must hold. Private.
#[table(accessor = match_state)]
pub struct MatchState {
    #[primary_key]
    id: u8,
    /// 0 until a map is loaded; then counts loads.
    map_version: u64,
}

/// Who a player is. One row per player who `join`ed; public, and changing
/// only when someone joins, leaves or reconnects.
#[table(accessor = seat, public)]
pub struct Seat {
    #[primary_key]
    player: u16,
    /// The SpacetimeDB identity this player is.
    #[unique]
    owner: Identity,
    /// The player's Ed25519 public key: what their UDP address proves against.
    udp_key: Vec<u8>,
    /// The connection that joined last, to tell its disconnection from an older one's.
    connection: Option<ConnectionId>,
    /// 0 while the player is connected; otherwise one more than the tick their
    /// connection dropped on. After the match's `away_grace_ticks` the player is removed.
    away_since: u64,
}

/// An identity that may not join, with the reason its player is told. Private;
/// the orchestration keeps it in step with the root database's bans.
#[table(accessor = ban)]
pub struct Ban {
    #[primary_key]
    identity: Identity,
    reason: String,
}

/// A name an identity has chosen (the root database's), for the roster to
/// show over its player. Private; the orchestration keeps it in step with the
/// root database's `known_identity`, as it does the bans.
#[table(accessor = member_name)]
pub struct MemberName {
    #[primary_key]
    identity: Identity,
    name: String,
}

/// Who runs the match. Private.
#[table(accessor = match_config)]
pub struct MatchConfig {
    #[primary_key]
    id: u8,
    /// The publisher: may do everything but what only the gateway does.
    owner: Identity,
    /// The only identity that may `submit_inputs`.
    gateway: Identity,
    /// The most players the match holds.
    capacity: u16,
    /// Ticks a seat is held after its connection dropped.
    away_grace_ticks: u64,
}

/// Where a joining player appears when the owner has said where, for tests
/// and tools that need players exactly there: the n-th of these for player id
/// n (modulo how many there are). Without any, the game's rules choose from the
/// map's starting locations. Private.
#[table(accessor = spawn_point)]
pub struct SpawnPoint {
    #[primary_key]
    index: u32,
    x: f32,
    y: f32,
    z: f32,
    yaw: f32,
}

/// The map: `MapData::to_bytes`, in one row. Private; the source of truth.
#[table(accessor = map_blob)]
pub struct MapBlob {
    #[primary_key]
    id: u8,
    data: Vec<u8>,
}

/// Input batches submitted since the last tick, oldest first. Private.
#[table(accessor = input_batch)]
pub struct InputBatch {
    #[primary_key]
    #[auto_inc]
    id: u64,
    data: Vec<u8>,
}

#[table(accessor = tick_timer, scheduled(tick))]
pub struct TickTimer {
    #[primary_key]
    #[auto_inc]
    scheduled_id: u64,
    scheduled_at: ScheduleAt,
}

thread_local! {
    /// The decoded map and the `map_version` it was decoded for. Module memory
    /// only; the tables above are the truth.
    static MAP_CACHE: RefCell<Option<(u64, Rc<MapData>)>> = const { RefCell::new(None) };
    /// Where the server saw the players over the last second, for the check
    /// of where a hit report says its target was. Module memory only, and only
    /// a cache of what a check looks back over: with none (a module whose
    /// memory is fresh) a report's target is checked against where it is now
    /// (see `halo_sim::combat`).
    static TRAILS: RefCell<Trails> = RefCell::new(Trails::new());
    /// Where each falling item is as of the last tick (`halo_sim::items::ItemStore::flight`): module memory
    /// only, so that a fall writes no rows. With none (a module whose memory is fresh) the tick works the
    /// item's flight out again from its row.
    static FLIGHTS: RefCell<BTreeMap<ItemId, Item>> = const { RefCell::new(BTreeMap::new()) };
}

fn match_state(ctx: &ReducerContext) -> MatchState {
    // `init` creates it
    ctx.db.match_state().id().find(ONLY).expect("the match is initialised")
}

/// The map, from the cache if it is current, otherwise from its row.
fn current_map(ctx: &ReducerContext) -> Option<Rc<MapData>> {
    let version = match_state(ctx).map_version;
    if version == 0 {
        return None;
    }
    if let Some(map) = MAP_CACHE.with(|c| c.borrow().as_ref().filter(|(v, _)| *v == version).map(|(_, m)| m.clone())) {
        return Some(map);
    }
    let blob = ctx.db.map_blob().id().find(ONLY)?;
    match MapData::from_bytes(&blob.data) {
        Ok(map) => {
            let map = Rc::new(map);
            MAP_CACHE.with(|c| *c.borrow_mut() = Some((version, map.clone())));
            Some(map)
        }
        Err(e) => {
            log::error!("the stored map does not decode: {e}");
            None
        }
    }
}

fn require_owner(ctx: &ReducerContext) -> Result<(), String> {
    match ctx.db.match_config().id().find(ONLY) {
        Some(config) if config.owner == ctx.sender() => Ok(()),
        _ => Err("only the match's owner may do that".into()),
    }
}

fn require_gateway(ctx: &ReducerContext) -> Result<(), String> {
    match ctx.db.match_config().id().find(ONLY) {
        Some(config) if config.gateway == ctx.sender() => Ok(()),
        _ => Err("only the match's gateway may do that".into()),
    }
}

fn to_player(row: &PlayerRow) -> Player {
    Player {
        flags: row.flags,
        air_ticks: row.air_ticks,
        air_z: row.air_z,
        free_ticks: row.free_ticks,
        free_z: row.free_z,
        ..Player::new(row.id, [row.x, row.y, row.z], row.yaw, row.pitch)
    }
}

/// Longest name a player has: the engine's name field.
pub const MAX_NAME: usize = 11;

/// A name as the roster shows it: letters, digits, spaces and `_ . -` only,
/// the first [`MAX_NAME`] of them, trimmed; `None` for nothing.
pub fn clean_name(name: &str) -> Option<String> {
    let kept: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '_' | '.' | '-'))
        .take(MAX_NAME)
        .collect();
    let kept = kept.trim().to_string();
    (!kept.is_empty()).then_some(kept)
}

/// Put `id` on the roster, on the team with fewer players, with the name its
/// identity has chosen, or a placeholder until the player has one of their own.
fn add_to_roster(ctx: &ReducerContext, id: u16, identity: Option<Identity>) {
    if ctx.db.roster().player().find(id).is_some() {
        return;
    }
    let mut counts = [0u32; TEAMS as usize];
    for row in ctx.db.roster().iter() {
        counts[(row.team % TEAMS) as usize] += 1;
    }
    let team = if counts[1] < counts[0] { 1 } else { 0 };
    let name = identity.and_then(|i| ctx.db.member_name().identity().find(i)).map(|m| m.name);
    ctx.db.roster().insert(RosterRow { player: id, team, name: name.unwrap_or_else(|| format!("Player {id}")) });
}

/// `halo_sim::Store` over the `player` table. Position writes keep the
/// rejection counters.
///
/// Every call to the host costs, and a tick looks at every player (a move reads the player's row and
/// writes it, the rules and the items read them all): [`TableStore::load`] reads the whole table once,
/// and from then on the store answers from what it read and writes through to the table, so that a
/// tick reads each row once. It is a copy of the table for as long as the store lives, and nothing else
/// writes the position columns in that time (the rejection counters the tick writes after the
/// simulation are not read through it).
struct TableStore<'a> {
    ctx: &'a ReducerContext,
    tick: u64,
    /// Every row as of now, once loaded.
    rows: RefCell<Option<BTreeMap<PlayerId, PlayerRow>>>,
}

impl<'a> TableStore<'a> {
    fn new(ctx: &'a ReducerContext, tick: u64) -> TableStore<'a> {
        TableStore { ctx, tick, rows: RefCell::new(None) }
    }

    /// Read every player's row, for the rest of the store's life.
    fn load(&self) {
        let rows = self.ctx.db.player().iter().map(|row| (row.id, row)).collect();
        *self.rows.borrow_mut() = Some(rows);
    }
}

impl Store for TableStore<'_> {
    fn player(&self, id: PlayerId) -> Option<Player> {
        if let Some(rows) = self.rows.borrow().as_ref() {
            return rows.get(&id).map(to_player);
        }
        self.ctx.db.player().id().find(id).as_ref().map(to_player)
    }

    fn set_player(&mut self, p: Player) {
        let table = self.ctx.db.player();
        let (x, y, z) = (p.position[0], p.position[1], p.position[2]);
        let moved = |row: PlayerRow| PlayerRow {
            x,
            y,
            z,
            yaw: p.yaw,
            pitch: p.pitch,
            updated_tick: self.tick,
            flags: p.flags,
            air_ticks: p.air_ticks,
            air_z: p.air_z,
            free_ticks: p.free_ticks,
            free_z: p.free_z,
            ..row
        };
        let known = match self.rows.borrow().as_ref() {
            Some(rows) => rows.get(&p.id).cloned(),
            None => table.id().find(p.id),
        };
        let written = match known {
            Some(row) => table.id().update(moved(row)),
            None => table.insert(PlayerRow {
                id: p.id,
                x,
                y,
                z,
                yaw: p.yaw,
                pitch: p.pitch,
                updated_tick: self.tick,
                rejected_moves: 0,
                last_reject: REJECT_NONE,
                last_reject_tick: 0,
                flags: p.flags,
                air_ticks: p.air_ticks,
                air_z: p.air_z,
                free_ticks: p.free_ticks,
                free_z: p.free_z,
            }),
        };
        if let Some(rows) = self.rows.borrow_mut().as_mut() {
            rows.insert(p.id, written);
        }
    }

    fn remove_player(&mut self, id: PlayerId) -> bool {
        if let Some(rows) = self.rows.borrow_mut().as_mut() {
            rows.remove(&id);
        }
        self.ctx.db.player().id().delete(id)
    }

    fn player_ids(&self) -> Vec<PlayerId> {
        if let Some(rows) = self.rows.borrow().as_ref() {
            return rows.keys().copied().collect();
        }
        let mut ids: Vec<PlayerId> = self.ctx.db.player().iter().map(|p| p.id).collect();
        ids.sort_unstable();
        ids
    }

    fn players(&self) -> Vec<Player> {
        if let Some(rows) = self.rows.borrow().as_ref() {
            return rows.values().map(to_player).collect();
        }
        let mut all: Vec<Player> = self.ctx.db.player().iter().map(|r| to_player(&r)).collect();
        all.sort_unstable_by_key(|p| p.id);
        all
    }

    fn ticks_since_move(&self, id: PlayerId) -> u32 {
        let updated = match self.rows.borrow().as_ref() {
            Some(rows) => rows.get(&id).map(|row| row.updated_tick),
            None => self.ctx.db.player().id().find(id).map(|row| row.updated_tick),
        };
        let since = updated.map_or(1, |updated| self.tick.saturating_sub(updated));
        since.clamp(1, u32::MAX as u64) as u32
    }
}

fn game_of(row: &GameStateRow) -> Game {
    let ending = match row.ending {
        ENDING_SCORE_LIMIT | ENDING_TIME_LIMIT => Some(Ending {
            reason: if row.ending == ENDING_SCORE_LIMIT { EndReason::ScoreLimit } else { EndReason::TimeLimit },
            winner: match row.winner_kind {
                WINNER_PLAYER => Winner::Player(row.winner),
                WINNER_TEAM => Winner::Team(row.winner as u8),
                _ => Winner::Nobody,
            },
        }),
        _ => None,
    };
    Game {
        rules: Rules {
            teams: row.teams,
            score_limit: row.score_limit,
            time_limit_ticks: row.time_limit_ticks,
            respawn_ticks: row.respawn_ticks,
            respawn_growth_ticks: row.respawn_growth_ticks,
            suicide_penalty_ticks: row.suicide_penalty_ticks,
            wave_ticks: row.wave_ticks,
        },
        started_tick: row.started_tick,
        team_scores: [row.red_score, row.blue_score],
        ending,
    }
}

fn game_row(game: &Game) -> GameStateRow {
    let (ending, winner_kind, winner) = match game.ending {
        None => (ENDING_NONE, WINNER_NOBODY, 0),
        Some(e) => {
            let ending = if e.reason == EndReason::ScoreLimit { ENDING_SCORE_LIMIT } else { ENDING_TIME_LIMIT };
            match e.winner {
                Winner::Nobody => (ending, WINNER_NOBODY, 0),
                Winner::Player(p) => (ending, WINNER_PLAYER, p),
                Winner::Team(t) => (ending, WINNER_TEAM, t as u16),
            }
        }
    };
    let r = &game.rules;
    GameStateRow {
        id: ONLY,
        teams: r.teams,
        score_limit: r.score_limit,
        time_limit_ticks: r.time_limit_ticks,
        respawn_ticks: r.respawn_ticks,
        respawn_growth_ticks: r.respawn_growth_ticks,
        suicide_penalty_ticks: r.suicide_penalty_ticks,
        wave_ticks: r.wave_ticks,
        started_tick: game.started_tick,
        tick: 0,
        red_score: game.team_scores[0],
        blue_score: game.team_scores[1],
        ending,
        winner_kind,
        winner,
    }
}

fn contestant_of(row: &StandingRow) -> Contestant {
    Contestant {
        id: row.player,
        team: row.team,
        score: row.score,
        deaths: row.deaths,
        life: match row.state {
            STATE_ALIVE => Life::Alive,
            STATE_DEAD => Life::Dead { due: row.due_tick },
            _ => Life::Waiting { wave: row.due_tick },
        },
        spawns: row.spawns,
        spawn: [row.x, row.y, row.z],
        spawn_yaw: row.yaw,
        spawned_tick: row.spawned_tick,
        penalty: row.penalty,
    }
}

fn standing_row(c: &Contestant) -> StandingRow {
    let (state, due_tick) = match c.life {
        Life::Alive => (STATE_ALIVE, 0),
        Life::Dead { due } => (STATE_DEAD, due),
        Life::Waiting { wave } => (STATE_WAITING, wave),
    };
    StandingRow {
        player: c.id,
        team: c.team,
        score: c.score,
        deaths: c.deaths,
        state,
        due_tick,
        spawns: c.spawns,
        spawned_tick: c.spawned_tick,
        x: c.spawn[0],
        y: c.spawn[1],
        z: c.spawn[2],
        yaw: c.spawn_yaw,
        penalty: c.penalty,
    }
}

/// `halo_sim::rules::GameStore` over the `game_state` and `standing` tables.
/// Rows are written only when they change: they are public, and what
/// subscribers are sent.
struct TableGame<'a> {
    ctx: &'a ReducerContext,
}

impl GameStore for TableGame<'_> {
    fn game(&self) -> Game {
        match self.ctx.db.game_state().id().find(ONLY) {
            Some(row) => game_of(&row),
            None => Game::new(Rules::slayer(), 0),
        }
    }

    fn set_game(&mut self, game: Game) {
        let mut row = game_row(&game);
        let table = self.ctx.db.game_state();
        match table.id().find(ONLY) {
            Some(old) if game_of(&old) == game => {}
            Some(old) => {
                row.tick = old.tick;
                table.id().update(row);
            }
            None => {
                table.insert(row);
            }
        }
    }

    fn contestant(&self, id: PlayerId) -> Option<Contestant> {
        self.ctx.db.standing().player().find(id).as_ref().map(contestant_of)
    }

    fn set_contestant(&mut self, c: Contestant) {
        let table = self.ctx.db.standing();
        match table.player().find(c.id) {
            Some(old) if contestant_of(&old) == c => {}
            Some(_) => {
                table.player().update(standing_row(&c));
            }
            None => {
                table.insert(standing_row(&c));
            }
        }
    }

    fn remove_contestant(&mut self, id: PlayerId) -> bool {
        self.ctx.db.standing().player().delete(id)
    }

    fn contestants(&self) -> Vec<Contestant> {
        let mut all: Vec<Contestant> = self.ctx.db.standing().iter().map(|r| contestant_of(&r)).collect();
        all.sort_unstable_by_key(|c| c.id);
        all
    }

    fn contestants_of(&self, ids: &[PlayerId]) -> Vec<Option<Contestant>> {
        // (a lookup is a call to the host: with this many to find, one pass over the table is cheaper)
        if ids.len() < 16 {
            return ids.iter().map(|&id| self.contestant(id)).collect();
        }
        let all = self.contestants();
        ids.iter().map(|&id| all.binary_search_by_key(&id, |c| c.id).ok().map(|i| all[i])).collect()
    }

    fn unspawned(&self) -> Vec<Contestant> {
        let table = self.ctx.db.standing();
        let mut out: Vec<Contestant> = table
            .state()
            .filter(STATE_DEAD)
            .chain(table.state().filter(STATE_WAITING))
            .map(|r| contestant_of(&r))
            .collect();
        out.sort_unstable_by_key(|c| c.id);
        out
    }
}

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
            weapons: [row.weapon0, row.weapon1],
            dropped: [(row.dropped0, row.dropped0_tick), (row.dropped1, row.dropped1_tick)],
        },
        hurt_tick: row.hurt_tick,
        hurt_by: row.hurt_by,
        hurt_count: row.hurt_count,
    }
}

fn fighter_row(f: &Fighter) -> FighterRow {
    FighterRow {
        player: f.id,
        shield: f.vitals.shield,
        body: f.vitals.body,
        shield_stun_ticks: f.vitals.shield_stun_ticks,
        flags: f.vitals.flags,
        tick: f.tick,
        weapon0: f.loadout.weapons[0],
        weapon1: f.loadout.weapons[1],
        dropped0: f.loadout.dropped[0].0,
        dropped0_tick: f.loadout.dropped[0].1,
        dropped1: f.loadout.dropped[1].0,
        dropped1_tick: f.loadout.dropped[1].1,
        hurt_tick: f.hurt_tick,
        hurt_by: f.hurt_by,
        hurt_count: f.hurt_count,
    }
}

fn shooter_of(row: &ShooterRow) -> Shooter {
    Shooter {
        id: row.player,
        hit_seconds: row.hit_seconds,
        hit_seconds_tick: row.hit_seconds_tick,
        accepted: row.accepted,
        rejected: row.rejected,
        last_reject: row.last_reject,
        last_reject_tick: row.last_reject_tick,
    }
}

fn shooter_row(s: &Shooter) -> ShooterRow {
    ShooterRow {
        player: s.id,
        hit_seconds: s.hit_seconds,
        hit_seconds_tick: s.hit_seconds_tick,
        accepted: s.accepted,
        rejected: s.rejected,
        last_reject: s.last_reject,
        last_reject_tick: s.last_reject_tick,
    }
}

/// `halo_sim::combat::CombatStore` over the `fighter` and `shooter` tables.
/// Rows are written only when they change: `fighter` is public, and what
/// subscribers are sent.
struct TableCombat<'a> {
    ctx: &'a ReducerContext,
}

impl CombatStore for TableCombat<'_> {
    fn fighter(&self, id: PlayerId) -> Option<Fighter> {
        self.ctx.db.fighter().player().find(id).as_ref().map(fighter_of)
    }

    fn set_fighter(&mut self, fighter: Fighter) {
        let table = self.ctx.db.fighter();
        match table.player().find(fighter.id) {
            Some(old) if fighter_of(&old) == fighter => {}
            Some(_) => {
                table.player().update(fighter_row(&fighter));
            }
            None => {
                table.insert(fighter_row(&fighter));
            }
        }
    }

    fn remove_fighter(&mut self, id: PlayerId) -> bool {
        self.ctx.db.fighter().player().delete(id)
    }

    fn shooter(&self, id: PlayerId) -> Shooter {
        self.ctx.db.shooter().player().find(id).as_ref().map_or_else(|| Shooter::new(id), shooter_of)
    }

    fn set_shooter(&mut self, shooter: Shooter) {
        let table = self.ctx.db.shooter();
        match table.player().find(shooter.id) {
            Some(old) if shooter_of(&old) == shooter => {}
            Some(_) => {
                table.player().update(shooter_row(&shooter));
            }
            None => {
                table.insert(shooter_row(&shooter));
            }
        }
    }

    fn remove_shooter(&mut self, id: PlayerId) -> bool {
        self.ctx.db.shooter().player().delete(id)
    }
}

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

fn item_row(item: &Item) -> ItemRow {
    ItemRow {
        id: item.id,
        tag: item.tag,
        x: item.position[0],
        y: item.position[1],
        z: item.position[2],
        vx: item.velocity[0],
        vy: item.velocity[1],
        vz: item.velocity[2],
        tick: item.tick,
        resting: item.resting,
        placement: item.placement,
        loaded: item.loaded,
        reserve: item.reserve,
        last_owned: item.last_owned,
        ignore: item.ignore,
    }
}

fn kit_of(row: Option<KitRow>, camo: Option<PowerupRow>, player: PlayerId) -> Kit {
    let mut kit = Kit::new(player);
    if let Some(row) = row {
        kit.ammo = [
            Ammo { loaded: row.loaded_0, reserve: row.reserve_0 },
            Ammo { loaded: row.loaded_1, reserve: row.reserve_1 },
        ];
        kit.version = row.version;
    }
    kit.camo_until = camo.map_or(0, |p| p.camo_until);
    kit
}

fn kit_row(kit: &Kit) -> KitRow {
    KitRow {
        player: kit.player,
        loaded_0: kit.ammo[0].loaded,
        reserve_0: kit.ammo[0].reserve,
        loaded_1: kit.ammo[1].loaded,
        reserve_1: kit.ammo[1].reserve,
        version: kit.version,
    }
}

/// `halo_sim::items::ItemStore` over the `item`, `kit` and `powerup` tables.
/// Rows are written only when they change: they are public, and what
/// subscribers are sent. Where a falling item is now is module memory only
/// (`FLIGHTS`), as `Trails` is: the row says how it began.
struct TableItems<'a> {
    ctx: &'a ReducerContext,
}

impl ItemStore for TableItems<'_> {
    fn items(&self) -> Vec<Item> {
        let mut all: Vec<Item> = self.ctx.db.item().iter().map(|r| item_of(&r)).collect();
        all.sort_unstable_by_key(|i| i.id);
        all
    }

    fn item(&self, id: ItemId) -> Option<Item> {
        self.ctx.db.item().id().find(id).as_ref().map(item_of)
    }

    fn insert_item(&mut self, item: Item) -> ItemId {
        self.ctx.db.item().insert(item_row(&Item { id: 0, ..item })).id
    }

    fn update_item(&mut self, item: Item) {
        let table = self.ctx.db.item();
        match table.id().find(item.id) {
            Some(old) if item_of(&old) == item => {}
            Some(_) => {
                table.id().update(item_row(&item));
            }
            None => {}
        }
    }

    fn remove_item(&mut self, id: ItemId) -> bool {
        FLIGHTS.with(|f| f.borrow_mut().remove(&id));
        self.ctx.db.item().id().delete(id)
    }

    fn flight(&self, id: ItemId) -> Option<Item> {
        FLIGHTS.with(|f| f.borrow().get(&id).copied())
    }

    fn set_flight(&mut self, item: Item) {
        FLIGHTS.with(|f| f.borrow_mut().insert(item.id, item));
    }

    fn clear_flight(&mut self, id: ItemId) {
        FLIGHTS.with(|f| f.borrow_mut().remove(&id));
    }

    fn kit(&self, player: PlayerId) -> Kit {
        kit_of(self.ctx.db.kit().player().find(player), self.ctx.db.powerup().player().find(player), player)
    }

    fn set_kit(&mut self, kit: Kit) {
        let table = self.ctx.db.kit();
        match table.player().find(kit.player) {
            Some(old) => {
                let held = kit_of(Some(old), None, kit.player);
                if held.ammo != kit.ammo || held.version != kit.version {
                    table.player().update(kit_row(&kit));
                }
            }
            None => {
                table.insert(kit_row(&kit));
            }
        }
        let camo = self.ctx.db.powerup();
        match (camo.player().find(kit.player), kit.camo_until) {
            (Some(_), 0) => {
                camo.player().delete(kit.player);
            }
            (Some(old), until) if old.camo_until != until => {
                camo.player().update(PowerupRow { player: kit.player, camo_until: until });
            }
            (None, until) if until != 0 => {
                camo.insert(PowerupRow { player: kit.player, camo_until: until });
            }
            _ => {}
        }
    }

    fn remove_kit(&mut self, player: PlayerId) -> bool {
        self.ctx.db.powerup().player().delete(player);
        self.ctx.db.kit().player().delete(player)
    }

    fn camouflaged(&self) -> Vec<PlayerId> {
        let mut ids: Vec<PlayerId> = self.ctx.db.powerup().iter().map(|p| p.player).collect();
        ids.sort_unstable();
        ids
    }
}

/// A player has spawned: full health and shields, the starting weapon and the
/// rounds it comes with.
fn spawn_combat(ctx: &ReducerContext, map: &MapData, id: PlayerId, tick: u64) {
    TRAILS.with(|t| halo_sim::combat::spawn(&mut TableCombat { ctx }, &mut t.borrow_mut(), map, id, tick));
    let fighter = TableCombat { ctx }.fighter(id);
    if let Some(fighter) = fighter {
        pickups::on_spawn(&mut TableItems { ctx }, map, id, &fighter.loadout);
    }
}

/// Take every item off the ground (a new game, a new map), and what the
/// players have of them.
fn clear_items(ctx: &ReducerContext) {
    let ids: Vec<u32> = ctx.db.item().iter().map(|i| i.id).collect();
    for id in ids {
        ctx.db.item().id().delete(id);
    }
    let camouflaged: Vec<u16> = ctx.db.powerup().iter().map(|p| p.player).collect();
    for id in camouflaged {
        ctx.db.powerup().player().delete(id);
    }
    FLIGHTS.with(|f| f.borrow_mut().clear());
}

/// A fighter for every player who is in the world and has none: one who
/// spawned before the map was loaded, or whose fighter was lost.
fn ensure_fighters(ctx: &ReducerContext, map: &MapData, tick: u64) {
    let alive: Vec<u16> = ctx.db.standing().state().filter(STATE_ALIVE).map(|s| s.player).collect();
    for id in alive {
        if ctx.db.fighter().player().find(id).is_none() {
            spawn_combat(ctx, map, id, tick);
        }
    }
}

/// What the rules did, for the log: deaths, the end, players told to wait. (Spawns
/// are too many to log: a player's row says.)
fn log_events(events: &[GameEvent]) {
    for event in events {
        match event {
            GameEvent::Died { victim, killer, kind, respawn_at } => {
                log::info!("player {victim} died ({kind:?}, killer {killer:?}): respawns at tick {respawn_at}")
            }
            GameEvent::DeathRefused { victim, reason } => {
                log::warn!("a death of player {victim} was refused: {reason:?}")
            }
            GameEvent::Over(ending) => log::info!("the match is over: {ending:?}"),
            _ => {}
        }
    }
}

/// What the items did, for the log: what the players took and put down.
fn log_item_events(events: &[ItemEvent]) {
    for event in events {
        match event {
            ItemEvent::PickedUp { player, item, tag, what } => {
                log::info!("player {player} took item {item} (tag {tag}): {what:?}")
            }
            ItemEvent::Dropped { player, item, tag } => log::info!("player {player} put down item {item} (tag {tag})"),
            _ => {}
        }
    }
}

/// The tick the match is on.
fn match_tick_of(ctx: &ReducerContext) -> u64 {
    ctx.db.match_tick().id().find(ONLY).map_or(0, |t| t.tick)
}

#[reducer(init)]
pub fn init(ctx: &ReducerContext) {
    // whoever publishes owns the match, and is its gateway until they say otherwise
    ctx.db.match_config().insert(MatchConfig {
        id: ONLY,
        owner: ctx.sender(),
        gateway: ctx.sender(),
        capacity: DEFAULT_CAPACITY,
        away_grace_ticks: DEFAULT_AWAY_GRACE_TICKS,
    });
    ctx.db.match_state().insert(MatchState { id: ONLY, map_version: 0 });
    // Slayer as the engine has it until the owner says otherwise (`set_game`)
    ctx.db.game_state().insert(game_row(&Game::new(Rules::slayer(), 0)));
    ctx.db.match_tick().insert(MatchTick {
        id: ONLY,
        tick: 0,
        stamped_us: 0,
        players: 0,
        inputs: 0,
        rejected: 0,
        rejected_total: 0,
        hits: 0,
        rejected_hits: 0,
        rejected_hits_total: 0,
        hits_total: 0,
        rejected_not_where_seen_total: 0,
    });
}

/// Store the map (`halo_sim::MapData::to_bytes`) and use it from the next
/// tick on. The bytes are checked first; a map that does not decode changes
/// nothing.
#[reducer]
pub fn load_map(ctx: &ReducerContext, data: Vec<u8>) -> Result<(), String> {
    require_owner(ctx)?;
    let map = MapData::from_bytes(&data).map_err(|e: MapError| e.to_string())?;
    let mut state = match_state(ctx);
    state.map_version += 1;
    let [x0, x1, y0, y1, z0, z1] = map.world_bounds;
    let info = MapInfo { id: ONLY, version: state.map_version, x0, x1, y0, y1, z0, z1 };
    if ctx.db.map_info().id().find(ONLY).is_some() {
        ctx.db.map_info().id().update(info);
    } else {
        ctx.db.map_info().insert(info);
    }
    let blob = MapBlob { id: ONLY, data };
    if ctx.db.map_blob().id().find(ONLY).is_some() {
        ctx.db.map_blob().id().update(blob);
    } else {
        ctx.db.map_blob().insert(blob);
    }
    // the cache is refilled from the row by the next tick, so it can never disagree with the table
    MAP_CACHE.with(|c| *c.borrow_mut() = None);
    // (what is on the ground is the old map's)
    clear_items(ctx);
    ctx.db.match_state().id().update(state);
    ensure_fighters(ctx, &map, match_tick_of(ctx));
    Ok(())
}

/// Put players in the match where the caller says, alive: the batch has the
/// layout of an input batch (`halo_sim::wire`), each record a player's id and
/// where they stand. For tests and tools; the players who join spawn by the
/// game's rules. Fails, changing nothing, if the batch is malformed or a
/// player is already in the match.
#[reducer]
pub fn add_players(ctx: &ReducerContext, batch: Vec<u8>) -> Result<(), String> {
    require_owner(ctx)?;
    let players = decode_inputs(&batch).map_err(|e| format!("batch of {} bytes is not whole records", e.0))?;
    let tick = ctx.db.match_tick().id().find(ONLY).map_or(0, |t| t.tick);
    for p in &players {
        if ctx.db.player().id().find(p.player).is_some() {
            return Err(format!("player {} is already in the match", p.player));
        }
    }
    for p in players {
        add_to_roster(ctx, p.player, None);
        rules::enter_placed(&mut TableGame { ctx }, p.player, roster_team(ctx, p.player), p.position, p.yaw);
        if let Some(map) = current_map(ctx) {
            spawn_combat(ctx, &map, p.player, tick);
        }
        ctx.db.player().insert(PlayerRow {
            id: p.player,
            x: p.position[0],
            y: p.position[1],
            z: p.position[2],
            yaw: p.yaw,
            pitch: p.pitch,
            updated_tick: tick,
            rejected_moves: 0,
            last_reject: REJECT_NONE,
            last_reject_tick: 0,
            flags: 0,
            air_ticks: 0,
            air_z: 0.0,
            free_ticks: 0,
            free_z: 0.0,
        });
    }
    Ok(())
}

#[reducer]
pub fn remove_players(ctx: &ReducerContext, ids: Vec<u16>) -> Result<(), String> {
    require_owner(ctx)?;
    for id in ids {
        remove_player(ctx, id);
    }
    Ok(())
}

/// Take a player out of the match, with their seat.
fn remove_player(ctx: &ReducerContext, id: u16) {
    ctx.db.player().id().delete(id);
    ctx.db.seat().player().delete(id);
    ctx.db.roster().player().delete(id);
    TableItems { ctx }.remove_kit(id);
    ctx.db.use_request().player().delete(id);
    rules::leave(&mut TableGame { ctx }, id);
    TRAILS.with(|t| halo_sim::combat::leave(&mut TableCombat { ctx }, &mut t.borrow_mut(), id));
    let reports: Vec<u64> = ctx.db.hit_report().iter().filter(|r| r.shooter == id).map(|r| r.id).collect();
    for report in reports {
        ctx.db.hit_report().id().delete(report);
    }
}

/// The team the roster has a player on.
fn roster_team(ctx: &ReducerContext, id: u16) -> u8 {
    ctx.db.roster().player().find(id).map_or(0, |r| r.team)
}

/// Take a seat in the match: a player, tied to the caller's identity.
/// `udp_key` is the public key of an Ed25519 key pair the caller keeps; the
/// gateway believes a UDP address is this player only if it can sign with the
/// private one. Calling again (from this connection or a new one, after a
/// drop) keeps the player and replaces the key. The player spawns where the
/// game's rules say (at a free starting location of the map now, or in the next
/// wave), or where the owner's `set_spawn_points` put them. Fails if the match
/// is full, or has no map, or the map no starting location for the game.
#[reducer]
pub fn join(ctx: &ReducerContext, udp_key: Vec<u8>) -> Result<(), String> {
    if udp_key.len() != UDP_KEY_SIZE {
        return Err(format!("the UDP key is {UDP_KEY_SIZE} bytes, not {}", udp_key.len()));
    }
    if let Some(ban) = ctx.db.ban().identity().find(ctx.sender()) {
        return Err(format!("{BANNED_PREFIX}{}", ban.reason));
    }
    if let Some(mut seat) = ctx.db.seat().owner().find(ctx.sender()) {
        seat.udp_key = udp_key;
        seat.connection = ctx.connection_id();
        seat.away_since = 0;
        ctx.db.seat().player().update(seat);
        return Ok(());
    }
    let capacity = ctx.db.match_config().id().find(ONLY).map_or(DEFAULT_CAPACITY, |c| c.capacity);
    // (a player who waits for a wave has a standing and a seat, and no row in `player` yet)
    let taken = |id: &u16| {
        ctx.db.player().id().find(*id).is_some()
            || ctx.db.standing().player().find(*id).is_some()
            || ctx.db.seat().player().find(*id).is_some()
    };
    let Some(id) = (0..capacity).find(|id| !taken(id)) else {
        return Err(format!("{FULL_PREFIX}the match has its {capacity} players"));
    };
    let tick = match_tick_of(ctx);
    let explicit = ctx.db.spawn_point().count() as u32;
    if explicit > 0 {
        // the owner has said where players go (`set_spawn_points`): the n-th for player n,
        // standing on the ground (a spawn point is a little above it, and a player placed
        // there would fall for a few ticks that the server would refuse to see)
        let spawn = ctx.db.spawn_point().index().find(id as u32 % explicit).ok_or("a spawn point is missing")?;
        let [x, y, z] = match current_map(ctx) {
            Some(map) => halo_sim::walk::settled(&map, [spawn.x, spawn.y, spawn.z]),
            None => [spawn.x, spawn.y, spawn.z],
        };
        ctx.db.player().insert(PlayerRow {
            id,
            x,
            y,
            z,
            yaw: spawn.yaw,
            pitch: 0.0,
            updated_tick: tick,
            rejected_moves: 0,
            last_reject: REJECT_NONE,
            last_reject_tick: 0,
            flags: 0,
            air_ticks: 0,
            air_z: 0.0,
            free_ticks: 0,
            free_z: 0.0,
        });
        add_to_roster(ctx, id, Some(ctx.sender()));
        rules::enter_placed(&mut TableGame { ctx }, id, roster_team(ctx, id), [x, y, z], spawn.yaw);
        if let Some(map) = current_map(ctx) {
            spawn_combat(ctx, &map, id, tick);
        }
    } else {
        // the game's rules say where: at a free starting location now, or in the next wave
        let map = current_map(ctx).ok_or("the match has no map yet")?;
        let game_type = TableGame { ctx }.game().rules.game_type();
        if !map.starts.iter().any(|s| game_type.uses_start(s)) {
            return Err("the map has no starting locations for the game".into());
        }
        add_to_roster(ctx, id, Some(ctx.sender()));
        let mut game = TableGame { ctx };
        rules::enter(&mut game, id, roster_team(ctx, id), tick);
        let mut store = TableStore::new(ctx, tick);
        let mut rng = Rng::seeded(tick ^ ((id as u64) << 32) ^ 0x5EED);
        let events = rules::spawn_due(&mut store, &mut game, &map, &mut rng, tick);
        log_events(&events);
        for event in &events {
            if let GameEvent::Spawned { player, .. } = event {
                spawn_combat(ctx, &map, *player, tick);
            }
        }
    }
    ctx.db.seat().insert(Seat {
        player: id,
        owner: ctx.sender(),
        udp_key,
        connection: ctx.connection_id(),
        away_since: 0,
    });
    Ok(())
}

/// Leave the match: the caller's player is removed, and with it their seat.
#[reducer]
pub fn leave(ctx: &ReducerContext) {
    if let Some(seat) = ctx.db.seat().owner().find(ctx.sender()) {
        remove_player(ctx, seat.player);
    }
}

/// A connection dropped: if it was the one a seat joined from, the seat is
/// held for the grace period for the player to come back to.
#[reducer(client_disconnected)]
pub fn disconnected(ctx: &ReducerContext) {
    let Some(mut seat) = ctx.db.seat().owner().find(ctx.sender()) else { return };
    if seat.connection != ctx.connection_id() {
        return;
    }
    let tick = ctx.db.match_tick().id().find(ONLY).map_or(0, |t| t.tick);
    seat.away_since = tick + 1;
    ctx.db.seat().player().update(seat);
}

/// Ban an identity from this match, with the reason its player is told. If it
/// has a seat the player is removed (the client sees its seat go, and its next
/// `join` is refused). Banning again replaces the reason.
#[reducer]
pub fn set_ban(ctx: &ReducerContext, identity: Identity, reason: String) -> Result<(), String> {
    require_owner(ctx)?;
    let row = Ban { identity, reason };
    if ctx.db.ban().identity().find(identity).is_some() {
        ctx.db.ban().identity().update(row);
    } else {
        ctx.db.ban().insert(row);
    }
    if let Some(seat) = ctx.db.seat().owner().find(identity) {
        remove_player(ctx, seat.player);
    }
    Ok(())
}

/// The name an identity plays under (`clean_name` of it; none, or nothing left
/// of it, forgets the name): shown on the roster for its player now, if it has
/// a seat, and whenever it takes one.
#[reducer]
pub fn set_name(ctx: &ReducerContext, identity: Identity, name: String) -> Result<(), String> {
    require_owner(ctx)?;
    let cleaned = clean_name(&name);
    match &cleaned {
        Some(name) => {
            let row = MemberName { identity, name: name.clone() };
            if ctx.db.member_name().identity().find(identity).is_some() {
                ctx.db.member_name().identity().update(row);
            } else {
                ctx.db.member_name().insert(row);
            }
        }
        None => {
            ctx.db.member_name().identity().delete(identity);
        }
    }
    if let Some(seat) = ctx.db.seat().owner().find(identity) {
        if let Some(mut row) = ctx.db.roster().player().find(seat.player) {
            row.name = cleaned.unwrap_or_else(|| format!("Player {}", seat.player));
            ctx.db.roster().player().update(row);
        }
    }
    Ok(())
}

/// Lift a ban.
#[reducer]
pub fn clear_ban(ctx: &ReducerContext, identity: Identity) -> Result<(), String> {
    require_owner(ctx)?;
    ctx.db.ban().identity().delete(identity);
    Ok(())
}

/// Where joining players appear instead of where the game's rules put them,
/// in the batch layout of `add_players` (only positions and yaw are used): for
/// tests and tools that need each player exactly where they say. Replaces the
/// earlier ones; none (the default) leaves the choice to the rules.
#[reducer]
pub fn set_spawn_points(ctx: &ReducerContext, batch: Vec<u8>) -> Result<(), String> {
    require_owner(ctx)?;
    let points = decode_inputs(&batch).map_err(|e| format!("batch of {} bytes is not whole records", e.0))?;
    let old: Vec<u32> = ctx.db.spawn_point().iter().map(|p| p.index).collect();
    for index in old {
        ctx.db.spawn_point().index().delete(index);
    }
    for (index, p) in points.iter().enumerate() {
        ctx.db.spawn_point().insert(SpawnPoint {
            index: index as u32,
            x: p.position[0],
            y: p.position[1],
            z: p.position[2],
            yaw: p.yaw,
        });
    }
    Ok(())
}

/// The most players the match holds (500 to start with).
#[reducer]
pub fn set_capacity(ctx: &ReducerContext, capacity: u16) -> Result<(), String> {
    require_owner(ctx)?;
    let mut config = ctx.db.match_config().id().find(ONLY).ok_or("the match is not initialised")?;
    config.capacity = capacity;
    ctx.db.match_config().id().update(config);
    Ok(())
}

/// How many ticks a seat is held after its connection dropped (900, 30
/// seconds, to start with); the seats are checked once a second.
#[reducer]
pub fn set_away_grace(ctx: &ReducerContext, ticks: u64) -> Result<(), String> {
    require_owner(ctx)?;
    let mut config = ctx.db.match_config().id().find(ONLY).ok_or("the match is not initialised")?;
    config.away_grace_ticks = ticks;
    ctx.db.match_config().id().update(config);
    Ok(())
}

/// Name the identity that runs the gateway: the only one that may `submit_inputs` from now on.
#[reducer]
pub fn set_gateway(ctx: &ReducerContext, gateway: Identity) -> Result<(), String> {
    require_owner(ctx)?;
    let mut config = ctx.db.match_config().id().find(ONLY).ok_or("the match is not initialised")?;
    config.gateway = gateway;
    ctx.db.match_config().id().update(config);
    Ok(())
}

/// One tick's inputs for every player, as a batch of `halo_sim::wire`
/// records. May be called more than once between ticks (the batches are
/// applied in order, and a player's second input of a tick is rejected as a
/// duplicate); the next [`tick`] consumes all of them.
#[reducer]
pub fn submit_inputs(ctx: &ReducerContext, batch: Vec<u8>) -> Result<(), String> {
    require_gateway(ctx)?;
    if !batch.len().is_multiple_of(INPUT_SIZE) {
        return Err(format!("batch of {} bytes is not whole {INPUT_SIZE}-byte records", batch.len()));
    }
    // while the tick is stopped nothing drains the queue; stale inputs are of no use
    if ctx.db.input_batch().count() >= MAX_PENDING_BATCHES {
        return Err(format!("{MAX_PENDING_BATCHES} batches are already waiting for a tick"));
    }
    ctx.db.input_batch().insert(InputBatch { id: 0, data: batch });
    Ok(())
}

/// Start ticking at 30 Hz. Does nothing if it already is.
#[reducer]
pub fn start(ctx: &ReducerContext) -> Result<(), String> {
    require_owner(ctx)?;
    if ctx.db.tick_timer().iter().next().is_none() {
        ctx.db.tick_timer().insert(TickTimer {
            scheduled_id: 0,
            scheduled_at: ScheduleAt::Interval(Duration::from_micros(TICK_INTERVAL_US).into()),
        });
    }
    Ok(())
}

#[reducer]
pub fn stop(ctx: &ReducerContext) -> Result<(), String> {
    require_owner(ctx)?;
    let timers: Vec<u64> = ctx.db.tick_timer().iter().map(|t| t.scheduled_id).collect();
    for id in timers {
        ctx.db.tick_timer().scheduled_id().delete(id);
    }
    Ok(())
}

/// Empty the match: no players or seats, no pending inputs, counters back to
/// zero. The map, the running state and the match's configuration are kept.
#[reducer]
pub fn reset(ctx: &ReducerContext) -> Result<(), String> {
    require_owner(ctx)?;
    let ids: Vec<u16> = ctx.db.player().iter().map(|p| p.id).collect();
    for id in ids {
        remove_player(ctx, id);
    }
    let batches: Vec<u64> = ctx.db.input_batch().iter().map(|b| b.id).collect();
    for id in batches {
        ctx.db.input_batch().id().delete(id);
    }
    let deaths: Vec<u64> = ctx.db.pending_death().iter().map(|d| d.id).collect();
    for id in deaths {
        ctx.db.pending_death().id().delete(id);
    }
    let reports: Vec<u64> = ctx.db.hit_report().iter().map(|r| r.id).collect();
    for id in reports {
        ctx.db.hit_report().id().delete(id);
    }
    let shooters: Vec<u16> = ctx.db.shooter().iter().map(|s| s.player).collect();
    for id in shooters {
        ctx.db.shooter().player().delete(id);
    }
    let fighters: Vec<u16> = ctx.db.fighter().iter().map(|f| f.player).collect();
    for id in fighters {
        ctx.db.fighter().player().delete(id);
    }
    TRAILS.with(|t| *t.borrow_mut() = Trails::new());
    clear_items(ctx);
    let requests: Vec<u16> = ctx.db.use_request().iter().map(|r| r.player).collect();
    for id in requests {
        ctx.db.use_request().player().delete(id);
    }
    let kits: Vec<u16> = ctx.db.kit().iter().map(|k| k.player).collect();
    for id in kits {
        ctx.db.kit().player().delete(id);
    }
    let blank = MatchTick {
        id: ONLY,
        tick: 0,
        stamped_us: 0,
        players: 0,
        inputs: 0,
        rejected: 0,
        rejected_total: 0,
        hits: 0,
        rejected_hits: 0,
        rejected_hits_total: 0,
        hits_total: 0,
        rejected_not_where_seen_total: 0,
    };
    ctx.db.match_tick().id().update(blank);
    rules::begin(&mut TableGame { ctx }, 0);
    Ok(())
}

/// Set the game: Slayer, or Team Slayer (`teams`), and its limits, and begin
/// it (see `begin_game`). `score_limit` is the score that ends the match (a
/// team's, in Team Slayer; 0 for none); `time_limit_ticks` how long it lasts
/// (0 for no limit); the respawn times and the wave are in ticks (30 a second).
#[reducer]
#[allow(clippy::too_many_arguments)]
pub fn set_game(
    ctx: &ReducerContext,
    teams: bool,
    score_limit: u32,
    time_limit_ticks: u32,
    respawn_ticks: u32,
    respawn_growth_ticks: u32,
    suicide_penalty_ticks: u32,
    wave_ticks: u32,
) -> Result<(), String> {
    require_owner(ctx)?;
    if wave_ticks == 0 {
        return Err("a wave every 0 ticks".into());
    }
    let rules = Rules {
        teams,
        score_limit,
        time_limit_ticks,
        respawn_ticks,
        respawn_growth_ticks,
        suicide_penalty_ticks,
        wave_ticks,
    };
    let tick = match_tick_of(ctx);
    let mut game = TableGame { ctx };
    game.set_game(Game::new(rules, tick));
    rules::begin(&mut game, tick);
    // (the items of the match before are not the new one's: its placements make theirs from the next tick)
    clear_items(ctx);
    Ok(())
}

/// Start the match's clock now: the time limit and the waves count from this
/// tick, and scores, deaths and respawn penalties go back to nothing. The
/// orchestration calls it when it tells players where the match is.
#[reducer]
pub fn begin_game(ctx: &ReducerContext) -> Result<(), String> {
    require_owner(ctx)?;
    rules::begin(&mut TableGame { ctx }, match_tick_of(ctx));
    clear_items(ctx);
    Ok(())
}

/// The hits the caller's player reports, as a batch of `halo_sim::wire`'s hit
/// records (a client calls it once a tick with the tick's hits, over its own
/// connection, which is reliable: a hit report lost to UDP loss is a kill that
/// did not count, and the connection says who the shooter is, by their seat).
/// The next tick judges each against the checks of `halo_sim::combat` (the
/// shooter owns the weapon, the target was where the shooter says, the rate of
/// fire is possible, the report is recent, ...), and deals the damage of those
/// that pass; a report that fails is counted (`shooter`, `match_tick`) and
/// does nothing. Fails, queuing nothing, for a caller with no seat or a batch
/// of the wrong size, or too many calls waiting for a tick.
#[reducer]
pub fn report_hits(ctx: &ReducerContext, batch: Vec<u8>) -> Result<(), String> {
    let seat = ctx.db.seat().owner().find(ctx.sender()).ok_or("you have no seat in this match")?;
    if !batch.len().is_multiple_of(HIT_SIZE) {
        return Err(format!("batch of {} bytes is not whole {HIT_SIZE}-byte records", batch.len()));
    }
    if batch.len() > MAX_HITS_PER_CALL * HIT_SIZE {
        return Err(format!("{} hits in one call, at most {MAX_HITS_PER_CALL}", batch.len() / HIT_SIZE));
    }
    if ctx.db.hit_report().count() >= MAX_PENDING_HIT_CALLS {
        return Err(format!("{MAX_PENDING_HIT_CALLS} calls are already waiting for a tick"));
    }
    ctx.db.hit_report().insert(HitReportRow { id: 0, shooter: seat.player, data: batch });
    Ok(())
}

/// What a player carries, for tests and tools: the weapons by tag index
/// (65535 for none), with the rounds the tags start them with. The player's
/// health and shields are as they are.
#[reducer]
pub fn set_loadout(ctx: &ReducerContext, player: u16, weapon0: u16, weapon1: u16) -> Result<(), String> {
    require_owner(ctx)?;
    let mut fighter = TableCombat { ctx }.fighter(player).ok_or_else(|| format!("player {player} has no fighter"))?;
    fighter.loadout = Loadout { weapons: [weapon0, weapon1], ..fighter.loadout };
    TableCombat { ctx }.set_fighter(fighter);
    if let Some(map) = current_map(ctx) {
        pickups::on_spawn(&mut TableItems { ctx }, &map, player, &fighter.loadout);
    }
    Ok(())
}

/// The caller's player pressed the action button, with the weapon slot the
/// player has in hand (the first or the second): the next tick gives the player what they reach
/// (a weapon to take as a second, or to swap for the one in hand) if the
/// rules of `halo_sim::pickups` say so, and nothing otherwise. A reliable call
/// on the player's own connection, like `report_hits`: a press lost is a
/// pickup the player has to press again for. One press a player waits for a
/// tick (a second replaces the first). Powerups, ammunition and a weapon with
/// nothing in hand are taken without it. Fails for a caller with no seat.
#[reducer]
pub fn use_item(ctx: &ReducerContext, slot: u8) -> Result<(), String> {
    let seat = ctx.db.seat().owner().find(ctx.sender()).ok_or("you have no seat in this match")?;
    let request = UseRequest { player: seat.player, slot: slot & 1 };
    if ctx.db.use_request().player().find(seat.player).is_some() {
        ctx.db.use_request().player().update(request);
    } else {
        ctx.db.use_request().insert(request);
    }
    Ok(())
}

/// The rounds of the weapons the caller's player carries, which the player's
/// client counts as it fires: `rounds` is eight bytes, the rounds in the
/// magazine and in reserve of slot 0 and then slot 1, as little-endian `i16`s.
/// The server keeps them within what the weapons hold, and uses them for what
/// a swap puts down and for how many rounds an ammunition pickup can give
/// (`halo_sim::pickups::report_ammo`). Fails for a caller with no seat, a map
/// that is not loaded or a batch of the wrong size.
#[reducer]
pub fn report_ammo(ctx: &ReducerContext, rounds: Vec<u8>) -> Result<(), String> {
    let seat = ctx.db.seat().owner().find(ctx.sender()).ok_or("you have no seat in this match")?;
    let map = current_map(ctx).ok_or("the match has no map yet")?;
    let [a, b, c, d, e, f, g, h]: [u8; 8] =
        rounds.try_into().map_err(|r: Vec<u8>| format!("{} bytes of rounds, not 8", r.len()))?;
    let ammo = [
        Ammo { loaded: i16::from_le_bytes([a, b]), reserve: i16::from_le_bytes([c, d]) },
        Ammo { loaded: i16::from_le_bytes([e, f]), reserve: i16::from_le_bytes([g, h]) },
    ];
    pickups::report_ammo(&mut TableItems { ctx }, &TableCombat { ctx }, &map, seat.player, ammo);
    Ok(())
}

/// Kill a player: the server's way for a death to happen and a kill to be
/// credited. `killer` is the player who gets the credit, or `NO_KILLER`
/// (65535) for a death nobody caused (which is the victim's own, as the
/// engine counts a fall). The death is applied by the next tick's rules (see
/// `halo_sim::rules`): a victim who is not alive then, or a match that is
/// over, refuses it, and nothing counts. The weapons' hit validation will
/// produce the same `Death` from validated hit reports.
#[reducer]
pub fn report_death(ctx: &ReducerContext, victim: u16, killer: u16) -> Result<(), String> {
    require_owner(ctx)?;
    if ctx.db.standing().player().find(victim).is_none() {
        return Err(format!("player {victim} is not in the match"));
    }
    ctx.db.pending_death().insert(PendingDeath { id: 0, victim, killer });
    Ok(())
}

/// A timed stage of the tick, for the profile of where its time goes (`--features stage-timing`; the
/// host logs "Timing span" lines with each stage's duration, see `rust/halo-server/check/stages.py`).
/// Without the feature it is nothing and costs nothing.
#[cfg(feature = "stage-timing")]
struct Stage(#[allow(dead_code)] spacetimedb::log_stopwatch::LogStopwatch);
#[cfg(not(feature = "stage-timing"))]
struct Stage;

impl Stage {
    #[cfg(feature = "stage-timing")]
    fn begin(name: &str) -> Stage {
        Stage(spacetimedb::log_stopwatch::LogStopwatch::new(name))
    }
    #[cfg(not(feature = "stage-timing"))]
    fn begin(_name: &str) -> Stage {
        Stage
    }
    /// Stop the stage here (it stops by itself at the end of its scope).
    fn end(self) {}
}

/// One simulation tick (scheduled; the server calls it, nobody else may).
#[reducer]
pub fn tick(ctx: &ReducerContext, _timer: TickTimer) -> Result<(), String> {
    if ctx.sender() != ctx.database_identity() {
        return Err("tick is scheduled by the server".into());
    }
    let mut marker = ctx.db.match_tick().id().find(ONLY).ok_or("the match is not initialised")?;
    marker.tick += 1;
    marker.stamped_us = ctx.timestamp.to_micros_since_unix_epoch();

    let stage = Stage::begin("tick.1 read inputs");
    let mut batches: Vec<InputBatch> = ctx.db.input_batch().iter().collect();
    batches.sort_unstable_by_key(|b| b.id);
    let mut decoded = Vec::with_capacity(batches.len());
    for batch in batches {
        ctx.db.input_batch().id().delete(batch.id);
        match decode_inputs(&batch.data) {
            Ok(inputs) => decoded.push(inputs),
            Err(e) => log::warn!("dropped a batch of {} bytes", e.0),
        }
    }
    let inputs = halo_sim::wire::collapse_batches(decoded);
    stage.end();

    let stage = Stage::begin("tick.2 read deaths and hit reports");
    let mut deaths: Vec<PendingDeath> = ctx.db.pending_death().iter().collect();
    deaths.sort_unstable_by_key(|d| d.id);
    for death in &deaths {
        ctx.db.pending_death().id().delete(death.id);
    }
    let mut deaths: Vec<Death> = deaths
        .iter()
        .map(|d| Death { victim: d.victim, killer: (d.killer != NO_KILLER).then_some(d.killer) })
        .collect();

    // the hit reports clients made since the last tick: each in the order it came
    let mut reports: Vec<HitReportRow> = ctx.db.hit_report().iter().collect();
    reports.sort_unstable_by_key(|r| r.id);
    let mut judged = Vec::new();
    for report in &reports {
        ctx.db.hit_report().id().delete(report.id);
        match decode_hits(&report.data) {
            Ok(hits) => judged.extend(hits.into_iter().map(|h| (report.shooter, h))),
            Err(e) => log::warn!("dropped a hit report of {} bytes", e.0),
        }
    }

    stage.end();

    let mut rejected = 0u32;
    let (mut hits, mut rejected_hits, mut not_where_seen) = (0u32, 0u32, 0u64);
    if let Some(map) = current_map(ctx) {
        let mut store = TableStore::new(ctx, marker.tick);
        store.load();
        let mut game = TableGame { ctx };
        let mut rng = Rng::seeded(marker.tick);
        if marker.tick.is_multiple_of(TICKS_PER_SECOND as u64) {
            ensure_fighters(ctx, &map, marker.tick);
        }
        // the hits first: the damage they do, and the deaths, are this tick's
        let stage = Stage::begin("tick.3 combat resolve");
        let dealt = TRAILS.with(|t| {
            halo_sim::combat::resolve(
                &store,
                &game,
                &mut TableCombat { ctx },
                &t.borrow(),
                &map,
                &mut rng,
                marker.tick,
                &judged,
            )
        });
        for event in &dealt.events {
            match event {
                HitEvent::Hit { .. } => hits += 1,
                HitEvent::Rejected { shooter, reason } => {
                    rejected_hits += 1;
                    not_where_seen += u64::from(*reason == halo_sim::combat::Reject::TargetNotWhereSeen);
                    log::warn!("rejected a hit report of player {shooter} ({reason:?}) at tick {}", marker.tick);
                }
            }
        }
        deaths.extend(dealt.deaths);
        stage.end();
        let stage = Stage::begin("tick.4 rules play");
        let outcome = rules::play(&mut store, &mut game, &map, &mut rng, marker.tick, &deaths, &inputs);
        stage.end();
        log_events(&outcome.events);
        let stage = Stage::begin("tick.5 deaths and spawns");
        // a player who died puts their weapons down; a player who spawned has the weapon
        // and the rounds the match starts them with
        let mut item_events: Vec<ItemEvent> = Vec::new();
        for event in &outcome.events {
            if let GameEvent::Died { victim, .. } = event {
                pickups::on_death(
                    &mut TableItems { ctx },
                    &mut TableCombat { ctx },
                    &store,
                    &map,
                    &mut rng,
                    marker.tick,
                    *victim,
                    &mut item_events,
                );
            }
        }
        for event in &outcome.events {
            if let GameEvent::Spawned { player, .. } = event {
                spawn_combat(ctx, &map, *player, marker.tick);
            }
        }
        stage.end();
        // the items: what the placements make, what falls, what the players take
        let stage = Stage::begin("tick.6 items");
        let mut requests: Vec<Request> =
            ctx.db.use_request().iter().map(|r| Request { player: r.player, slot: r.slot }).collect();
        requests.sort_unstable_by_key(|r| r.player);
        for request in &requests {
            ctx.db.use_request().player().delete(request.player);
        }
        item_events.extend(halo_sim::items::tick(
            &mut TableItems { ctx },
            &mut TableCombat { ctx },
            &store,
            &game,
            &map,
            &mut rng,
            marker.tick,
            &requests,
        ));
        log_item_events(&item_events);
        stage.end();
        let stage = Stage::begin("tick.7 rejected moves");
        for event in outcome.moves {
            let Event::MoveRejected { player, reason } = event else { continue };
            rejected += 1;
            if let Some(mut row) = ctx.db.player().id().find(player) {
                // (for the operator's log: where the player is, where they said they were, and when they last moved)
                if let Some(input) = inputs.iter().find(|i| i.player == player) {
                    log::warn!(
                        "rejected a move of player {player} ({reason:?}) at tick {}: from ({:.3} {:.3} {:.3}) to \
                         ({:.3} {:.3} {:.3}), last moved at tick {}, {} ticks in the air",
                        marker.tick,
                        row.x,
                        row.y,
                        row.z,
                        input.position[0],
                        input.position[1],
                        input.position[2],
                        row.updated_tick,
                        row.air_ticks
                    );
                }
                row.rejected_moves += 1;
                row.last_reject = reject_code(reason);
                row.last_reject_tick = marker.tick;
                ctx.db.player().id().update(row);
            }
        }
        stage.end();
        // where the server saw everyone, for the next ticks' hit reports
        let stage = Stage::begin("tick.8 trails");
        // (the store has them: the rejection counters written above are the only change to the rows since it read them)
        TRAILS.with(|t| {
            t.borrow_mut().record(marker.tick, store.players().iter().map(|p| (p.id, p.position)));
        });
        stage.end();
    } else if !inputs.is_empty() {
        log::warn!("no map is loaded: dropped {} inputs", inputs.len());
    }

    let stage = Stage::begin("tick.9 upkeep and marker");
    // once a second: seats whose player has been away for the grace period go
    if marker.tick.is_multiple_of(TICKS_PER_SECOND as u64) {
        let grace = ctx.db.match_config().id().find(ONLY).map_or(DEFAULT_AWAY_GRACE_TICKS, |c| c.away_grace_ticks);
        let gone: Vec<u16> = ctx
            .db
            .seat()
            .iter()
            .filter(|s| s.away_since != 0 && marker.tick >= s.away_since.saturating_add(grace))
            .map(|s| s.player)
            .collect();
        for id in gone {
            remove_player(ctx, id);
        }
    }

    if marker.tick.is_multiple_of(CLOCK_EVERY_TICKS) {
        if let Some(mut row) = ctx.db.game_state().id().find(ONLY) {
            row.tick = marker.tick;
            ctx.db.game_state().id().update(row);
        }
    }
    marker.players = ctx.db.standing().count() as u32;
    marker.inputs = inputs.len() as u32;
    marker.rejected = rejected;
    marker.rejected_total += rejected as u64;
    marker.hits = hits;
    marker.rejected_hits = rejected_hits;
    marker.rejected_hits_total += rejected_hits as u64;
    marker.hits_total += hits as u64;
    marker.rejected_not_where_seen_total += not_where_seen;
    ctx.db.match_tick().id().update(marker);
    stage.end();
    Ok(())
}
