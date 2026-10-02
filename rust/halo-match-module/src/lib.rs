//! The match module: one SpacetimeDB database holds one large-scale match.
//!
//! - A scheduled reducer, [`tick`], runs `halo_sim::step` 30 times a second.
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
//!
//! The tests in `rust/halo-match-driver` publish and drive it against a real
//! local SpacetimeDB.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use halo_map::MapError;
use halo_sim::wire::{decode_inputs, INPUT_SIZE};
use halo_sim::{step, Event, MapData, Player, PlayerId, RejectReason, Rng, Store, TICKS_PER_SECOND};
use spacetimedb::{reducer, table, ReducerContext, ScheduleAt, Table};

/// Microseconds between ticks.
const TICK_INTERVAL_US: u64 = 1_000_000 / TICKS_PER_SECOND as u64;

/// Batches that may wait for a tick before `submit_inputs` refuses more.
const MAX_PENDING_BATCHES: u64 = 16;

/// The one row of a single-row table.
const ONLY: u8 = 0;

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
    /// Players in the match after the tick.
    players: u32,
    /// Inputs this tick took from the batches (accepted, rejected, or dropped because no map is loaded).
    inputs: u32,
    /// Inputs this tick rejected, including those for players who are not in
    /// the match (which no player row can count).
    rejected: u32,
    /// Rejections since the match was reset.
    rejected_total: u64,
}

/// One player. Position and facing are the last move the server accepted.
#[table(accessor = player, public)]
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

/// Which map the cache must hold. Private.
#[table(accessor = match_state)]
pub struct MatchState {
    #[primary_key]
    id: u8,
    /// 0 until a map is loaded; then counts loads.
    map_version: u64,
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

fn to_player(row: &PlayerRow) -> Player {
    Player { id: row.id, position: [row.x, row.y, row.z], yaw: row.yaw, pitch: row.pitch }
}

/// `halo_sim::Store` over the `player` table. Position writes keep the
/// rejection counters.
struct TableStore<'a> {
    ctx: &'a ReducerContext,
    tick: u64,
}

impl Store for TableStore<'_> {
    fn player(&self, id: PlayerId) -> Option<Player> {
        self.ctx.db.player().id().find(id).as_ref().map(to_player)
    }

    fn set_player(&mut self, p: Player) {
        let table = self.ctx.db.player();
        let (x, y, z) = (p.position[0], p.position[1], p.position[2]);
        let moved = |row: PlayerRow| PlayerRow { x, y, z, yaw: p.yaw, pitch: p.pitch, updated_tick: self.tick, ..row };
        match table.id().find(p.id) {
            Some(row) => {
                table.id().update(moved(row));
            }
            None => {
                let fresh = PlayerRow {
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
                };
                table.insert(fresh);
            }
        }
    }

    fn remove_player(&mut self, id: PlayerId) -> bool {
        self.ctx.db.player().id().delete(id)
    }

    fn player_ids(&self) -> Vec<PlayerId> {
        let mut ids: Vec<PlayerId> = self.ctx.db.player().iter().map(|p| p.id).collect();
        ids.sort_unstable();
        ids
    }
}

#[reducer(init)]
pub fn init(ctx: &ReducerContext) {
    ctx.db.match_state().insert(MatchState { id: ONLY, map_version: 0 });
    ctx.db.match_tick().insert(MatchTick {
        id: ONLY,
        tick: 0,
        stamped_us: 0,
        players: 0,
        inputs: 0,
        rejected: 0,
        rejected_total: 0,
    });
}

/// Store the map (`halo_sim::MapData::to_bytes`) and use it from the next
/// tick on. The bytes are checked first; a map that does not decode changes
/// nothing.
#[reducer]
pub fn load_map(ctx: &ReducerContext, data: Vec<u8>) -> Result<(), String> {
    MapData::from_bytes(&data).map_err(|e: MapError| e.to_string())?;
    let mut state = match_state(ctx);
    state.map_version += 1;
    let blob = MapBlob { id: ONLY, data };
    if ctx.db.map_blob().id().find(ONLY).is_some() {
        ctx.db.map_blob().id().update(blob);
    } else {
        ctx.db.map_blob().insert(blob);
    }
    // the cache is refilled from the row by the next tick, so it can never disagree with the table
    MAP_CACHE.with(|c| *c.borrow_mut() = None);
    ctx.db.match_state().id().update(state);
    Ok(())
}

/// Put players in the match. The batch has the layout of an input batch
/// (`halo_sim::wire`): each record is a player's id and where they start.
/// Fails, changing nothing, if the batch is malformed or a player is already
/// in the match. Where a player should spawn is a later ticket's rule; the
/// caller chooses for now.
#[reducer]
pub fn add_players(ctx: &ReducerContext, batch: Vec<u8>) -> Result<(), String> {
    let players = decode_inputs(&batch).map_err(|e| format!("batch of {} bytes is not whole records", e.0))?;
    let tick = ctx.db.match_tick().id().find(ONLY).map_or(0, |t| t.tick);
    for p in &players {
        if ctx.db.player().id().find(p.player).is_some() {
            return Err(format!("player {} is already in the match", p.player));
        }
    }
    for p in players {
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
        });
    }
    Ok(())
}

#[reducer]
pub fn remove_players(ctx: &ReducerContext, ids: Vec<u16>) {
    for id in ids {
        ctx.db.player().id().delete(id);
    }
}

/// One tick's inputs for every player, as a batch of `halo_sim::wire`
/// records. May be called more than once between ticks (the batches are
/// applied in order, and a player's second input of a tick is rejected as a
/// duplicate); the next [`tick`] consumes all of them.
#[reducer]
pub fn submit_inputs(ctx: &ReducerContext, batch: Vec<u8>) -> Result<(), String> {
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
pub fn start(ctx: &ReducerContext) {
    if ctx.db.tick_timer().iter().next().is_none() {
        ctx.db.tick_timer().insert(TickTimer {
            scheduled_id: 0,
            scheduled_at: ScheduleAt::Interval(Duration::from_micros(TICK_INTERVAL_US).into()),
        });
    }
}

#[reducer]
pub fn stop(ctx: &ReducerContext) {
    let timers: Vec<u64> = ctx.db.tick_timer().iter().map(|t| t.scheduled_id).collect();
    for id in timers {
        ctx.db.tick_timer().scheduled_id().delete(id);
    }
}

/// Empty the match: no players, no pending inputs, counters back to zero. The
/// map and the running state are kept.
#[reducer]
pub fn reset(ctx: &ReducerContext) {
    let ids: Vec<u16> = ctx.db.player().iter().map(|p| p.id).collect();
    for id in ids {
        ctx.db.player().id().delete(id);
    }
    let batches: Vec<u64> = ctx.db.input_batch().iter().map(|b| b.id).collect();
    for id in batches {
        ctx.db.input_batch().id().delete(id);
    }
    let blank = MatchTick { id: ONLY, tick: 0, stamped_us: 0, players: 0, inputs: 0, rejected: 0, rejected_total: 0 };
    ctx.db.match_tick().id().update(blank);
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

    let mut inputs = Vec::new();
    let mut batches: Vec<InputBatch> = ctx.db.input_batch().iter().collect();
    batches.sort_unstable_by_key(|b| b.id);
    for batch in batches {
        ctx.db.input_batch().id().delete(batch.id);
        match decode_inputs(&batch.data) {
            Ok(mut decoded) => inputs.append(&mut decoded),
            Err(e) => log::warn!("dropped a batch of {} bytes", e.0),
        }
    }

    let mut rejected = 0u32;
    if let Some(map) = current_map(ctx) {
        let mut store = TableStore { ctx, tick: marker.tick };
        let events = step(&mut store, &inputs, &map, &mut Rng::seeded(marker.tick));
        for event in events {
            let Event::MoveRejected { player, reason } = event else { continue };
            rejected += 1;
            if let Some(mut row) = ctx.db.player().id().find(player) {
                row.rejected_moves += 1;
                row.last_reject = reject_code(reason);
                row.last_reject_tick = marker.tick;
                ctx.db.player().id().update(row);
            }
        }
    } else if !inputs.is_empty() {
        log::warn!("no map is loaded: dropped {} inputs", inputs.len());
    }

    marker.players = ctx.db.player().count() as u32;
    marker.inputs = inputs.len() as u32;
    marker.rejected = rejected;
    marker.rejected_total += rejected as u64;
    ctx.db.match_tick().id().update(marker);
    Ok(())
}
