//! Benchmark module: a fixed-rate tick that rewrites one row per player, the
//! way a shooter replicates every unit every tick. Not game code.

use spacetimedb::{reducer, table, ReducerContext, ScheduleAt, Table};
use std::time::Duration;

/// One player's replicated state. 56 bytes of payload, a little larger than
/// the 35-byte on-foot unit state the distributed netcode sends.
#[table(accessor = unit_state, public)]
pub struct UnitState {
    #[primary_key]
    player_id: u32,
    /// Interest cell. Never changes; clients may subscribe to one cell.
    #[index(btree)]
    cell: u32,
    tick: u32,
    /// Server time at the start of the tick that wrote this row, in
    /// microseconds since the Unix epoch. The client subtracts it from its
    /// own clock to get the age of the state on arrival.
    stamped_us: i64,
    x: f32,
    y: f32,
    z: f32,
    vx: f32,
    vy: f32,
    vz: f32,
    yaw: f32,
    pitch: f32,
    flags: u32,
}

/// The latest input each client sent. Private: nobody subscribes to it.
#[table(accessor = player_input)]
pub struct PlayerInput {
    #[primary_key]
    player_id: u32,
    buttons: u32,
    yaw: f32,
    pitch: f32,
    client_tick: u32,
}

#[table(accessor = world)]
pub struct World {
    #[primary_key]
    id: u32,
    tick: u32,
}

#[table(accessor = tick_timer, scheduled(tick))]
pub struct TickTimer {
    #[primary_key]
    #[auto_inc]
    scheduled_id: u64,
    scheduled_at: ScheduleAt,
}

fn clear(ctx: &ReducerContext) {
    let timers: Vec<u64> = ctx.db.tick_timer().iter().map(|t| t.scheduled_id).collect();
    for id in timers {
        ctx.db.tick_timer().scheduled_id().delete(id);
    }
    let players: Vec<u32> = ctx.db.unit_state().iter().map(|u| u.player_id).collect();
    for id in players {
        ctx.db.unit_state().player_id().delete(id);
        ctx.db.player_input().player_id().delete(id);
    }
    ctx.db.world().id().delete(0);
}

/// Create `players` rows spread over `cells` interest cells. Does not start
/// the tick.
#[reducer]
pub fn setup(ctx: &ReducerContext, players: u32, cells: u32) {
    clear(ctx);
    let cells = cells.max(1);
    ctx.db.world().insert(World { id: 0, tick: 0 });
    for player_id in 0..players {
        ctx.db.unit_state().insert(UnitState {
            player_id,
            cell: player_id % cells,
            tick: 0,
            stamped_us: 0,
            x: player_id as f32,
            y: 0.0,
            z: 0.0,
            vx: 0.0,
            vy: 0.0,
            vz: 0.0,
            yaw: 0.0,
            pitch: 0.0,
            flags: 0,
        });
        ctx.db.player_input().insert(PlayerInput {
            player_id,
            buttons: 0,
            yaw: 0.0,
            pitch: 0.0,
            client_tick: 0,
        });
    }
}

#[reducer]
pub fn start(ctx: &ReducerContext, interval_us: u64) {
    ctx.db.tick_timer().insert(TickTimer {
        scheduled_id: 0,
        scheduled_at: ScheduleAt::Interval(Duration::from_micros(interval_us).into()),
    });
}

#[reducer]
pub fn stop(ctx: &ReducerContext) {
    let timers: Vec<u64> = ctx.db.tick_timer().iter().map(|t| t.scheduled_id).collect();
    for id in timers {
        ctx.db.tick_timer().scheduled_id().delete(id);
    }
}

#[reducer]
pub fn send_input(ctx: &ReducerContext, player_id: u32, buttons: u32, yaw: f32, pitch: f32, client_tick: u32) {
    ctx.db.player_input().player_id().update(PlayerInput {
        player_id,
        buttons,
        yaw,
        pitch,
        client_tick,
    });
}

/// One simulation tick: read every player's input and rewrite every unit
/// state. The arithmetic is a stand-in; the real engine does far more work
/// per player (collision, physics, weapons), which this does not measure.
#[reducer]
pub fn tick(ctx: &ReducerContext, _timer: TickTimer) {
    let Some(world) = ctx.db.world().id().find(0) else {
        return;
    };
    let tick = world.tick.wrapping_add(1);
    ctx.db.world().id().update(World { id: 0, tick });

    let stamped_us = ctx.timestamp.to_micros_since_unix_epoch();
    const DT: f32 = 1.0 / 30.0;

    let units: Vec<UnitState> = ctx.db.unit_state().iter().collect();
    for mut unit in units {
        if let Some(input) = ctx.db.player_input().player_id().find(unit.player_id) {
            unit.yaw = input.yaw;
            unit.pitch = input.pitch;
            unit.flags = input.buttons;
        }
        let (sin, cos) = unit.yaw.sin_cos();
        unit.vx = cos * 2.25;
        unit.vy = sin * 2.25;
        unit.x += unit.vx * DT;
        unit.y += unit.vy * DT;
        unit.tick = tick;
        unit.stamped_us = stamped_us;
        ctx.db.unit_state().player_id().update(unit);
    }
}
