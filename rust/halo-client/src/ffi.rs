//! The C interface: what port/linux/game/large_mode.c calls.
//!
//! The rules of the boundary, which the 32-bit C client's build sets (it is
//! compiled with `-freg-struct-return` and `-malign-double`, so a structure
//! cannot cross by value):
//!
//! - only `float`, 32-bit integers (`unsigned long` in the client's C) and
//!   pointers go in or out;
//! - nothing is returned by value but a 32-bit integer; everything else comes
//!   back through an out-pointer the caller owns;
//! - no pointer is kept past the call.
//!
//! One session at a time. All calls are for the game's main thread; the
//! session's own threads never call into C.
//!
//! The functions are `unsafe` only for their pointer arguments. A panic never
//! crosses into C: a call that would panic returns 0 and records the reason
//! for [`halo_large_error`].

use std::ffi::{c_char, CStr};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::Mutex;

use halo_sim::walk::Controls;

use crate::browser::{Browser, ServerEntry};
use crate::identity::IdentityFile;
use crate::local::Local;
use crate::session::{Config, LifeState, Member, RefusalKind, RemoteUnit, Session, Standing};

#[derive(Default)]
struct Global {
    session: Option<Session>,
    /// The local player's own movement, once the map is being read.
    local: Option<Local>,
    /// What [`halo_large_frame`] froze, for [`halo_large_unit`] to read.
    frame: Vec<RemoteUnit>,
    /// What [`halo_large_scoreboard_freeze`] froze, best first, for
    /// [`halo_large_scoreboard_row`] to read.
    board: Vec<(u16, Standing, Member)>,
    error: String,
    /// Where identities are kept ([`halo_large_identity_dir`]).
    identity_dir: Option<PathBuf>,
    /// The name the player plays under ([`halo_large_set_name`]).
    name: String,
    /// The server list's connection, and the list as [`halo_large_browse_list`] froze it.
    browser: Option<Browser>,
    servers: Vec<ServerEntry>,
}

static GLOBAL: Mutex<Global> = Mutex::new(Global {
    session: None,
    local: None,
    frame: Vec::new(),
    board: Vec::new(),
    error: String::new(),
    identity_dir: None,
    name: String::new(),
    browser: None,
    servers: Vec::new(),
});

fn global() -> std::sync::MutexGuard<'static, Global> {
    GLOBAL.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Run `f`, turning a panic into `or`.
fn guard<T>(or: T, f: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(value) => value,
        Err(_) => {
            global().error = "the client library panicked".into();
            or
        }
    }
}

unsafe fn string(pointer: *const c_char) -> Option<String> {
    if pointer.is_null() {
        return None;
    }
    // SAFETY: the caller passes a NUL-terminated string
    Some(unsafe { CStr::from_ptr(pointer) }.to_string_lossy().into_owned())
}

/// Start a session: connect to SpacetimeDB directly, take a seat in the match
/// (its `join`) and join the gateway over UDP as the player the seat is. A new
/// SpacetimeDB identity is made for the session. Returns 1 when the session has
/// started, which is not yet being in the match (see [`halo_large_status`]), and
/// 0 when it could not (see [`halo_large_error`]). A session already running is
/// stopped first.
///
/// # Safety
/// The three strings are NUL-terminated.
#[no_mangle]
pub unsafe extern "C" fn halo_large_start(
    gateway: *const c_char,
    spacetimedb: *const c_char,
    database: *const c_char,
) -> u32 {
    guard(0, || {
        let (Some(gateway), Some(spacetimedb), Some(database)) =
            (unsafe { string(gateway) }, unsafe { string(spacetimedb) }, unsafe { string(database) })
        else {
            global().error = "a null string".into();
            return 0;
        };
        // (the old session's drop joins its threads: not under the lock)
        let old = {
            let mut g = global();
            g.frame.clear();
            g.error.clear();
            g.local = None;
            g.session.take()
        };
        drop(old);
        let identity = IdentityFile::new(global().identity_dir.as_deref(), &spacetimedb);
        match Session::start(Config { gateway, spacetimedb, database, token: None, identity }) {
            Ok(session) => {
                global().session = Some(session);
                1
            }
            Err(e) => {
                global().error = e;
                0
            }
        }
    })
}

/// End the session, if there is one.
#[no_mangle]
pub extern "C" fn halo_large_stop() {
    guard((), || {
        let old = {
            let mut g = global();
            g.frame.clear();
            g.local = None;
            g.session.take()
        };
        drop(old);
    })
}

/// Read the map file at `path` (the player's own Xbox map file, which the
/// game has loaded too) for the local player's movement: its collision data
/// and its tags' movement values. Returns at once, the file being read on a
/// thread of its own; [`halo_large_move`] has nothing to give until it is
/// in. Returns 1 when the reading has begun, and 0 for a null path.
///
/// # Safety
/// `path` is NUL-terminated.
#[no_mangle]
pub unsafe extern "C" fn halo_large_load_map(path: *const c_char) -> u32 {
    guard(0, || {
        let Some(path) = (unsafe { string(path) }) else {
            global().error = "a null path".into();
            return 0;
        };
        global().local = Some(Local::load(path));
        1
    })
}

/// Put the local player at `x y z` (world units), at rest: where the server
/// placed them, and every tick the library is not moving them (the map not in
/// yet) where the engine has the unit. Does nothing without
/// [`halo_large_load_map`].
#[no_mangle]
pub extern "C" fn halo_large_place(x: f32, y: f32, z: f32) {
    guard((), || {
        if let Some(local) = &mut global().local {
            local.place([x, y, z]);
        }
    })
}

/// The local player's movement for one tick: `forward` and `strafe` are the
/// throttle (ahead and to the left, -1 to 1), `yaw` and `pitch` where the
/// player faces and aims, in radians, and `jump` and `crouch` are 1 while the
/// buttons are held. Returns 0 when the library cannot move the player yet
/// (the map is not in, or the player not placed): the caller moves them as it
/// did before. Otherwise it returns 1, plus 2 while the player is in the air
/// and plus 4 while they are crouched (in the crouch, or standing up from it),
/// and `out` has seven `float`s: the position `x y z` (world units) and
/// velocity `x y z` (world units a second) of the player after the tick, and
/// how fast the tick drove the player into the ground it landed on (world
/// units a *tick*, the engine's unit; 0 when it did not land; what falling
/// damage reads). The new position, the facing and whether the player is
/// crouched are sent to the gateway as this tick's input, as
/// [`halo_large_send_input`] would, unless the tick ran ahead of the clock
/// (a game that runs ticks in a rush): then the player stays where they were
/// and nothing is sent.
///
/// # Safety
/// `out` points to seven writable `float`s.
#[no_mangle]
pub unsafe extern "C" fn halo_large_move(
    forward: f32,
    strafe: f32,
    yaw: f32,
    pitch: f32,
    jump: u32,
    crouch: u32,
    out: *mut f32,
) -> u32 {
    guard(0, || {
        let mut g = global();
        if out.is_null() {
            return 0;
        }
        let controls = Controls { forward, strafe, yaw, pitch, jump: jump != 0, crouch: crouch != 0 };
        let Some(moved) = g.local.as_mut().and_then(|l| l.step(controls)) else {
            return 0;
        };
        if moved.stepped {
            if let Some(session) = &g.session {
                let flags = if moved.crouched { halo_sim::FLAG_CROUCHED } else { 0 };
                session.send_input(moved.position, yaw, pitch, flags);
            }
        }
        let [x, y, z] = moved.position;
        let [vx, vy, vz] = moved.velocity;
        unsafe { std::slice::from_raw_parts_mut(out, 7) }.copy_from_slice(&[
            x,
            y,
            z,
            vx,
            vy,
            vz,
            moved.landing_velocity,
        ]);
        1 + 2 * moved.airborne as u32 + 4 * moved.crouched as u32
    })
}

/// Where the session is. Returns 1 once the gateway has welcomed the player
/// (the other players' states flow), 0 before that and with no session.
/// `out` has eight `unsigned long`s:
///
/// | index | |
/// |---|---|
/// | 0 | 1 when the direct SpacetimeDB connection is up and subscribed |
/// | 1 | how many maps the match has loaded (0: none yet) |
/// | 2 | Hellos and Auths sent |
/// | 3 | datagrams received |
/// | 4 | bytes received |
/// | 5 | datagrams that did not decode |
/// | 6 | the player this session is (the match's seat for it), or 0xFFFFFFFF before the match has given one |
/// | 7 | inputs sent |
///
/// # Safety
/// `out` points to eight writable `unsigned long`s.
#[no_mangle]
pub unsafe extern "C" fn halo_large_status(out: *mut u32) -> u32 {
    guard(0, || {
        let g = global();
        if out.is_null() {
            return 0;
        }
        let out = unsafe { std::slice::from_raw_parts_mut(out, 8) };
        out.fill(0);
        out[6] = u32::MAX;
        let Some(session) = &g.session else { return 0 };
        let (slow, counters) = (session.slow(), session.counters());
        out[0] = slow.connected as u32;
        out[1] = slow.map_version as u32;
        out[2] = counters.joins_sent;
        out[3] = counters.datagrams as u32;
        out[4] = counters.bytes as u32;
        out[5] = counters.ignored as u32;
        out[6] = session.player().map_or(u32::MAX, |p| p as u32);
        out[7] = counters.inputs_sent as u32;
        session.joined() as u32
    })
}

/// Freeze the other players' newest states for [`halo_large_unit`] to read,
/// and return how many there are. `tick` is set to the newest tick a datagram
/// has carried.
///
/// # Safety
/// `tick` points to a writable `unsigned long`.
#[no_mangle]
pub unsafe extern "C" fn halo_large_frame(tick: *mut u32) -> u32 {
    guard(0, || {
        let mut g = global();
        if tick.is_null() {
            return 0;
        }
        let Some(session) = &g.session else {
            unsafe { *tick = 0 };
            g.frame.clear();
            return 0;
        };
        let frame = session.frame();
        unsafe { *tick = frame.tick };
        g.frame = frame.units;
        g.frame.len() as u32
    })
}

/// One state of the frozen frame, `index` below the count [`halo_large_frame`]
/// returned. Returns 1, or 0 when there is no such state. `player` is the
/// player number, `tick` the tick of the datagram that carried the state, and
/// `out` has nine `float`s: position `x y z` (world units), velocity
/// `x y z` (world units a second), yaw and pitch (radians), and the player's
/// flags as a number (1 in the air, 2 crouched: `halo_sim::FLAG_AIRBORNE`,
/// `halo_sim::FLAG_CROUCHED`).
///
/// # Safety
/// `player` and `tick` point to a writable `unsigned long` each, `out` to nine
/// writable `float`s.
#[no_mangle]
pub unsafe extern "C" fn halo_large_unit(index: u32, player: *mut u32, tick: *mut u32, out: *mut f32) -> u32 {
    guard(0, || {
        let g = global();
        if player.is_null() || tick.is_null() || out.is_null() {
            return 0;
        }
        let Some(unit) = g.frame.get(index as usize) else { return 0 };
        let s = &unit.state;
        unsafe {
            *player = s.player as u32;
            *tick = unit.tick;
            std::slice::from_raw_parts_mut(out, 9).copy_from_slice(&[
                s.position[0],
                s.position[1],
                s.position[2],
                s.velocity[0],
                s.velocity[1],
                s.velocity[2],
                s.yaw,
                s.pitch,
                s.flags as f32,
            ]);
        }
        1
    })
}

/// Who a player is, from the match's roster: `team` is the engine's number for
/// it (0 red, 1 blue) and `name` is filled with the player's name, UTF-8,
/// NUL-terminated and cut to `size`. Returns 1, or 0 when the roster does
/// not have the player (and nothing is written).
///
/// # Safety
/// `team` points to a writable `unsigned long`, `name` to `size` writable `char`s.
#[no_mangle]
pub unsafe extern "C" fn halo_large_member(player: u32, team: *mut u32, name: *mut c_char, size: u32) -> u32 {
    guard(0, || {
        let g = global();
        if team.is_null() || name.is_null() || size == 0 {
            return 0;
        }
        let Some(member) = u16::try_from(player).ok().and_then(|p| g.session.as_ref()?.member(p)) else { return 0 };
        let bytes = member.name.as_bytes();
        let length = bytes.len().min(size as usize - 1);
        // SAFETY: `size` bytes are the caller's
        unsafe {
            *team = member.team as u32;
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), name as *mut u8, length);
            *name.add(length) = 0;
        }
        1
    })
}

/// The local player's state as the server holds it, from the direct
/// connection: `x y z yaw pitch` in `out` (five `float`s). Returns 1, or 0
/// before the server has said.
///
/// # Safety
/// `out` points to five writable `float`s.
#[no_mangle]
pub unsafe extern "C" fn halo_large_local(out: *mut f32) -> u32 {
    guard(0, || {
        let g = global();
        if out.is_null() {
            return 0;
        }
        let Some(local) = g.session.as_ref().and_then(|s| s.slow().local) else { return 0 };
        unsafe { std::slice::from_raw_parts_mut(out, 5) }.copy_from_slice(&local);
        1
    })
}

/// The map's world bounds `x0 x1 y0 y1 z0 z1` in `out` (six `float`s), from
/// the direct connection. Returns 1, or 0 before the server has said.
///
/// # Safety
/// `out` points to six writable `float`s.
#[no_mangle]
pub unsafe extern "C" fn halo_large_bounds(out: *mut f32) -> u32 {
    guard(0, || {
        let g = global();
        if out.is_null() {
            return 0;
        }
        let Some(bounds) = g.session.as_ref().and_then(|s| s.slow().bounds) else { return 0 };
        unsafe { std::slice::from_raw_parts_mut(out, 6) }.copy_from_slice(&bounds.to_world());
        1
    })
}

/// How the local player is doing, as the server says (the match's `standing`
/// table): `info` has eight `unsigned long`s,
///
/// | index | |
/// |---|---|
/// | 0 | 0 alive, 1 dead, 2 waiting for a wave (no starting location was free) |
/// | 1 | the server tick the player spawns on: when dead, the respawn timer's end; when waiting, the wave's; 0 when alive |
/// | 2 | counts the player's spawns: a change says they are somewhere new |
/// | 3 | the score (a signed number) |
/// | 4 | deaths |
/// | 5 | the team (0 red, 1 blue) |
/// | 6 | the server's tick now (the ticks above are on this clock; 30 a second): from the game's row, twice a second, and the time since |
/// | 7 | the tick the player last spawned on |
///
/// and `position` four `float`s, `x y z yaw`: where the server last spawned the
/// player. Returns 1, or 0 before the server has said.
///
/// # Safety
/// `info` points to eight writable `unsigned long`s, `position` to four writable `float`s.
#[no_mangle]
pub unsafe extern "C" fn halo_large_life(info: *mut u32, position: *mut f32) -> u32 {
    guard(0, || {
        let g = global();
        if info.is_null() || position.is_null() {
            return 0;
        }
        let Some(session) = &g.session else { return 0 };
        let Some(life) = session.life() else { return 0 };
        let state = match life.state {
            LifeState::Alive => 0,
            LifeState::Dead => 1,
            LifeState::Waiting => 2,
        };
        unsafe {
            std::slice::from_raw_parts_mut(info, 8).copy_from_slice(&[
                state,
                life.due_tick as u32,
                life.spawns,
                life.score as u32,
                life.deaths,
                life.team as u32,
                session.server_tick(),
                life.spawned_tick as u32,
            ]);
            std::slice::from_raw_parts_mut(position, 4).copy_from_slice(&life.spawn);
        }
        1
    })
}

/// The game, as the server says (the match's `game_state` table): `out` has
/// ten `unsigned long`s,
///
/// | index | |
/// |---|---|
/// | 0 | 1 in Team Slayer |
/// | 1 | the score limit (0 for none) |
/// | 2 | the time limit in ticks (0 for none), from the tick in 3 |
/// | 3 | the tick the match's clock started on |
/// | 4 | ticks between waves |
/// | 5 | the red team's score (a signed number) |
/// | 6 | the blue team's |
/// | 7 | 0 while the match is on, 1 when a score limit ended it, 2 a time limit |
/// | 8 | who won: 0 nobody, 1 a player, 2 a team |
/// | 9 | the winning player's number, or team (0 red, 1 blue) |
///
/// Returns 1, or 0 before the server has said.
///
/// # Safety
/// `out` points to ten writable `unsigned long`s.
#[no_mangle]
pub unsafe extern "C" fn halo_large_game(out: *mut u32) -> u32 {
    guard(0, || {
        let g = global();
        if out.is_null() {
            return 0;
        }
        let Some(game) = g.session.as_ref().and_then(|s| s.game()) else { return 0 };
        unsafe {
            std::slice::from_raw_parts_mut(out, 10).copy_from_slice(&[
                game.teams as u32,
                game.score_limit,
                game.time_limit_ticks,
                game.started_tick as u32,
                game.wave_ticks,
                game.red_score as u32,
                game.blue_score as u32,
                game.ending as u32,
                game.winner_kind as u32,
                game.winner as u32,
            ]);
        }
        1
    })
}

/// Freeze the scoreboard for [`halo_large_scoreboard_row`] to read: every
/// player of the match, in range or not, best first (by score, then fewer
/// deaths, then player number), and return how many there are.
#[no_mangle]
pub extern "C" fn halo_large_scoreboard_freeze() -> u32 {
    guard(0, || {
        let mut g = global();
        let mut board = g.session.as_ref().map(|s| s.scoreboard()).unwrap_or_default();
        board.sort_by_key(|(id, s, _)| (std::cmp::Reverse(s.score), s.deaths, *id));
        g.board = board;
        g.board.len() as u32
    })
}

/// One row of the frozen scoreboard, `index` below the count
/// [`halo_large_scoreboard_freeze`] returned: `out` has seven `unsigned long`s,
/// the player's number, team (0 red, 1 blue), score (a signed number), deaths,
/// life (as [`halo_large_life`]'s first), place (1 for the best, and the same
/// for a tie), and 1 if the player is this session's own; `name` is filled with
/// the player's name, UTF-8, NUL-terminated and cut to `size`. Returns 1, or 0
/// when there is no such row.
///
/// # Safety
/// `out` points to seven writable `unsigned long`s, `name` to `size` writable `char`s.
#[no_mangle]
pub unsafe extern "C" fn halo_large_scoreboard_row(index: u32, out: *mut u32, name: *mut c_char, size: u32) -> u32 {
    guard(0, || {
        let g = global();
        if out.is_null() || name.is_null() || size == 0 {
            return 0;
        }
        let Some((player, standing, member)) = g.board.get(index as usize) else { return 0 };
        // (a tie is the same place: the place of the first with the score)
        let place = g.board.iter().position(|(_, s, _)| s.score == standing.score).unwrap_or(index as usize) + 1;
        let own = g.session.as_ref().and_then(|s| s.player()) == Some(*player);
        let life = match standing.state {
            LifeState::Alive => 0,
            LifeState::Dead => 1,
            LifeState::Waiting => 2,
        };
        let bytes = member.name.as_bytes();
        let length = bytes.len().min(size as usize - 1);
        // SAFETY: the sizes are the caller's
        unsafe {
            std::slice::from_raw_parts_mut(out, 7).copy_from_slice(&[
                *player as u32,
                member.team as u32,
                standing.score as u32,
                standing.deaths,
                life,
                place as u32,
                own as u32,
            ]);
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), name as *mut u8, length);
            *name.add(length) = 0;
        }
        1
    })
}

/// A player's health and shields, as the server says (the match's `fighter`
/// table, with the shield's recharge since counted: see `halo_sim::damage`):
/// `out` has six `float`s,
///
/// | index | |
/// |---|---|
/// | 0 | the shield, as a fraction of a full one (above 1 is an overshield) |
/// | 1 | the health, as a fraction of full health (below 0 is dead) |
/// | 2 | ticks the shield will not recharge for (a hit stuns it) |
/// | 3 | flags: 1 the shield is down, 2 dead, 4 overcharging, 8 recharging |
/// | 4 | how many hits have hurt the player since they spawned: a change is a hit |
/// | 5 | the player who last hurt them, or -1 for none |
///
/// Returns 1, or 0 when the server has no fighter for the player (they are
/// not in the world, or it has not said yet).
///
/// # Safety
/// `out` points to six writable `float`s.
#[no_mangle]
pub unsafe extern "C" fn halo_large_vitals(player: u32, out: *mut f32) -> u32 {
    guard(0, || {
        let g = global();
        if out.is_null() {
            return 0;
        }
        let Some(session) = &g.session else { return 0 };
        let Some(fighter) = u16::try_from(player).ok().and_then(|p| session.fighter(p)) else { return 0 };
        let vitals = match g.local.as_ref().and_then(|l| l.map()) {
            Some(map) => fighter.vitals_at(map, u64::from(session.server_tick())),
            None => fighter.vitals,
        };
        let by = if fighter.hurt_count == 0 { -1.0 } else { f32::from(fighter.hurt_by) };
        unsafe { std::slice::from_raw_parts_mut(out, 6) }.copy_from_slice(&[
            vitals.shield,
            vitals.body,
            f32::from(vitals.shield_stun_ticks),
            f32::from(vitals.flags),
            fighter.hurt_count as f32,
            by,
        ]);
        1
    })
}

/// The weapons a player carries, as the server says, by their tag index in
/// the map (65535 for none): `out` has two `unsigned long`s. Returns 1, or 0
/// when the server has no fighter for the player.
///
/// # Safety
/// `out` points to two writable `unsigned long`s.
#[no_mangle]
pub unsafe extern "C" fn halo_large_loadout(player: u32, out: *mut u32) -> u32 {
    guard(0, || {
        let g = global();
        if out.is_null() {
            return 0;
        }
        let Some(fighter) = u16::try_from(player).ok().and_then(|p| g.session.as_ref()?.fighter(p)) else { return 0 };
        unsafe { std::slice::from_raw_parts_mut(out, 2) }
            .copy_from_slice(&[u32::from(fighter.loadout.weapons[0]), u32::from(fighter.loadout.weapons[1])]);
        1
    })
}

/// The name of a weapon of the map by its tag index, as the tags have it
/// (`weapons\pistol\pistol.weap`), NUL-terminated and cut to `size`: what the
/// engine finds the weapon's tag by. Returns its length, or 0 when the map is
/// not in yet or has no such weapon.
///
/// # Safety
/// `buffer` points to `size` writable `char`s.
#[no_mangle]
pub unsafe extern "C" fn halo_large_weapon_name(tag_index: u32, buffer: *mut c_char, size: u32) -> u32 {
    guard(0, || {
        let g = global();
        let Some(map) = g.local.as_ref().and_then(|l| l.map()) else { return 0 };
        let Some(weapon) = u16::try_from(tag_index).ok().and_then(|t| map.combat.weapon(t)) else { return 0 };
        unsafe { copy_text(&weapon.name, buffer, size) }
    })
}

/// Report a hit the engine saw the local player's weapon make: `target` is the
/// player hit, `damage` the damage effect's tag index in the map (what hurt
/// the target: the weapon's bullet, its explosion, its blow in melee),
/// `material` the part of the target that was hit (an index of the player's
/// body's materials, -1 for none), `scale` the scale the engine dealt the
/// damage at (see `halo_sim::source`), `ox oy oz` where the shot hit and
/// `tx ty tz` where the engine has the target (world units). The report goes to the server over the direct
/// connection, which does not lose it (the server's `report_hits`), made at the
/// newest server tick the client has heard of; the server checks it and deals
/// the damage (the client deals none). Returns 1 when the report is on its way,
/// 0 without a session, a seat, or word from the gateway yet.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn halo_large_report_hit(
    target: u32,
    damage: u32,
    material: i32,
    scale: f32,
    ox: f32,
    oy: f32,
    oz: f32,
    tx: f32,
    ty: f32,
    tz: f32,
) -> u32 {
    guard(0, || {
        let (Ok(target), Ok(damage), Ok(material)) =
            (u16::try_from(target), u16::try_from(damage), i16::try_from(material))
        else {
            return 0;
        };
        match &global().session {
            Some(session) => session.report_hit(target, damage, material, scale, [ox, oy, oz], [tx, ty, tz]) as u32,
            None => 0,
        }
    })
}

/// How many hits have been reported, and in how many calls to the server:
/// `out` has two `unsigned long`s. Returns 1, or 0 with no session.
///
/// # Safety
/// `out` points to two writable `unsigned long`s.
#[no_mangle]
pub unsafe extern "C" fn halo_large_hits(out: *mut u32) -> u32 {
    guard(0, || {
        let g = global();
        if out.is_null() {
            return 0;
        }
        let Some(session) = &g.session else { return 0 };
        let counters = session.counters();
        unsafe { std::slice::from_raw_parts_mut(out, 2) }
            .copy_from_slice(&[counters.hits_reported as u32, counters.hit_calls as u32]);
        1
    })
}

/// Tell the gateway where the local player is now: this tick's input. Does
/// nothing until the gateway has welcomed the player.
#[no_mangle]
pub extern "C" fn halo_large_send_input(x: f32, y: f32, z: f32, yaw: f32, pitch: f32) {
    guard((), || {
        if let Some(session) = &global().session {
            session.send_input([x, y, z], yaw, pitch, 0);
        }
    })
}

/// Copy the last error's text, NUL-terminated and cut to `size`, into
/// `buffer`; returns its length (0: no error, and `buffer` is left alone).
///
/// # Safety
/// `buffer` points to `size` writable `char`s.
#[no_mangle]
pub unsafe extern "C" fn halo_large_error(buffer: *mut c_char, size: u32) -> u32 {
    guard(0, || {
        let g = global();
        // a session's own latest trouble, or why it could not start
        let message = match &g.session {
            Some(session) => session.last_error().unwrap_or_default(),
            None => g.error.clone(),
        };
        // (or why the map could not be read)
        let message =
            if message.is_empty() { g.local.as_ref().and_then(|l| l.error()).unwrap_or_default() } else { message };
        if message.is_empty() || size == 0 || buffer.is_null() {
            return 0;
        }
        let length = message.len().min(size as usize - 1);
        // SAFETY: `size` bytes are the caller's
        unsafe {
            std::ptr::copy_nonoverlapping(message.as_ptr(), buffer as *mut u8, length);
            *buffer.add(length) = 0;
        }
        length as u32
    })
}

/// Copy `text`, NUL-terminated and cut to `size`, into `buffer`; its length
/// (0 and `buffer` left alone for no text).
///
/// # Safety
/// `buffer` points to `size` writable `char`s.
unsafe fn copy_text(text: &str, buffer: *mut c_char, size: u32) -> u32 {
    if text.is_empty() || size == 0 || buffer.is_null() {
        return 0;
    }
    let length = text.len().min(size as usize - 1);
    // SAFETY: `size` bytes are the caller's
    unsafe {
        std::ptr::copy_nonoverlapping(text.as_ptr(), buffer as *mut u8, length);
        *buffer.add(length) = 0;
    }
    length as u32
}

/// Where the player's identity is kept between sessions: a folder, in which
/// each SpacetimeDB the player has been to has a file with the token that
/// proves who they are there. Without it every session (and the server list)
/// is a new identity. Set it before [`halo_large_start`] and
/// [`halo_large_browse_start`].
///
/// # Safety
/// `folder` is NUL-terminated.
#[no_mangle]
pub unsafe extern "C" fn halo_large_identity_dir(folder: *const c_char) {
    guard((), || {
        global().identity_dir = unsafe { string(folder) }.filter(|f| !f.is_empty()).map(PathBuf::from);
    })
}

/// The name the player plays under, which the server list is told when
/// [`halo_large_browse_start`] connects and which the matches show on their
/// rosters (letters, digits, spaces and `_ . -`, the first 11 of them; empty for
/// none). Set it before [`halo_large_browse_start`].
///
/// # Safety
/// `name` is NUL-terminated.
#[no_mangle]
pub unsafe extern "C" fn halo_large_set_name(name: *const c_char) {
    guard((), || {
        global().name = unsafe { string(name) }.unwrap_or_default();
    })
}

/// Start listing the servers of the root database `database` on the
/// SpacetimeDB at `spacetimedb` (a URI), as the player's own identity. Returns
/// 1 when listing has started, which is not yet having the list (see
/// [`halo_large_browse_status`]), and 0 when it could not (see
/// [`halo_large_error`]). A list already running is stopped first.
///
/// # Safety
/// The strings are NUL-terminated.
#[no_mangle]
pub unsafe extern "C" fn halo_large_browse_start(spacetimedb: *const c_char, database: *const c_char) -> u32 {
    guard(0, || {
        let (Some(spacetimedb), Some(database)) = (unsafe { string(spacetimedb) }, unsafe { string(database) }) else {
            global().error = "a null string".into();
            return 0;
        };
        let (old, identity, name) = {
            let mut g = global();
            g.servers.clear();
            g.error.clear();
            (g.browser.take(), IdentityFile::new(g.identity_dir.as_deref(), &spacetimedb), g.name.clone())
        };
        drop(old);
        match Browser::start(&spacetimedb, &database, identity, &name) {
            Ok(browser) => {
                global().browser = Some(browser);
                1
            }
            Err(e) => {
                global().error = e;
                0
            }
        }
    })
}

/// Stop listing servers, if it is.
#[no_mangle]
pub extern "C" fn halo_large_browse_stop() {
    guard((), || {
        let old = {
            let mut g = global();
            g.servers.clear();
            g.browser.take()
        };
        drop(old);
    })
}

/// Whether the list is up to date with the server (1) or not yet (0, and with
/// no list started). `out` has two `unsigned long`s: 1 when the server list's
/// database has refused the player as banned, and the number of servers in the
/// list now.
///
/// # Safety
/// `out` points to two writable `unsigned long`s.
#[no_mangle]
pub unsafe extern "C" fn halo_large_browse_status(out: *mut u32) -> u32 {
    guard(0, || {
        let g = global();
        if out.is_null() {
            return 0;
        }
        let out = unsafe { std::slice::from_raw_parts_mut(out, 2) };
        out.fill(0);
        let Some(browser) = &g.browser else { return 0 };
        out[0] = browser.banned().is_some() as u32;
        out[1] = browser.servers().len() as u32;
        browser.connected() as u32
    })
}

/// Freeze the server list for [`halo_large_browse_entry`],
/// [`halo_large_browse_text`] and [`halo_large_browse_find`] to read, and
/// return how many servers there are.
#[no_mangle]
pub extern "C" fn halo_large_browse_list() -> u32 {
    guard(0, || {
        let mut g = global();
        g.servers = g.browser.as_ref().map(|b| b.servers()).unwrap_or_default();
        g.servers.len() as u32
    })
}

/// The numbers of one server of the frozen list, `index` below the count
/// [`halo_large_browse_list`] returned. Returns 1, or 0 when there is no such
/// server. `out` has five `unsigned long`s: the players in the match, the most
/// it holds, the match's number (matches the server has run), its time limit in
/// seconds (0: none) and 1 when there is a match to join (0: between matches).
///
/// # Safety
/// `out` points to five writable `unsigned long`s.
#[no_mangle]
pub unsafe extern "C" fn halo_large_browse_entry(index: u32, out: *mut u32) -> u32 {
    guard(0, || {
        let g = global();
        if out.is_null() {
            return 0;
        }
        let Some(s) = g.servers.get(index as usize) else { return 0 };
        unsafe { std::slice::from_raw_parts_mut(out, 5) }.copy_from_slice(&[
            s.players,
            s.capacity,
            s.match_number as u32,
            s.match_seconds,
            !s.database.is_empty() as u32,
        ]);
        1
    })
}

/// One text of one server of the frozen list: `field` 0 the server's id, 1 its
/// title, 2 the match's map, 3 its game type, 4 its variant, 5 its database
/// (empty between matches), 6 the gateway to send UDP to (`host:port`).
/// Copied, NUL-terminated and cut to `size`, into `buffer`; returns its length
/// (0: nothing to say, or no such server, and `buffer` is left alone).
///
/// # Safety
/// `buffer` points to `size` writable `char`s.
#[no_mangle]
pub unsafe extern "C" fn halo_large_browse_text(index: u32, field: u32, buffer: *mut c_char, size: u32) -> u32 {
    guard(0, || {
        let g = global();
        let Some(s) = g.servers.get(index as usize) else { return 0 };
        let text = match field {
            0 => &s.id,
            1 => &s.title,
            2 => &s.map,
            3 => &s.game_type,
            4 => &s.variant,
            5 => &s.database,
            6 => &s.gateway,
            _ => return 0,
        };
        unsafe { copy_text(text, buffer, size) }
    })
}

/// The index in the frozen list of the server with this id, plus one; 0 when
/// the list has none.
///
/// # Safety
/// `id` is NUL-terminated.
#[no_mangle]
pub unsafe extern "C" fn halo_large_browse_find(id: *const c_char) -> u32 {
    guard(0, || {
        let Some(id) = (unsafe { string(id) }) else { return 0 };
        let g = global();
        g.servers.iter().position(|s| s.id == id).map_or(0, |at| at as u32 + 1)
    })
}

/// What the server list has to say that the player should read: that its
/// database turned them away as banned (and why), or else its latest trouble.
/// Copied as [`halo_large_error`] copies; returns the length (0: nothing).
///
/// # Safety
/// `buffer` points to `size` writable `char`s.
#[no_mangle]
pub unsafe extern "C" fn halo_large_browse_message(buffer: *mut c_char, size: u32) -> u32 {
    guard(0, || {
        let g = global();
        let Some(browser) = &g.browser else { return 0 };
        let text = browser.banned().map(|b| b.message).or_else(|| browser.last_error()).unwrap_or_default();
        unsafe { copy_text(&text, buffer, size) }
    })
}

/// The player's SpacetimeDB identity as hex (64 characters), once the server
/// has said: the one of the running session, else the one of the server list's
/// connection. Copied as [`halo_large_error`] copies; returns the length (0:
/// not known yet).
///
/// # Safety
/// `buffer` points to `size` writable `char`s.
#[no_mangle]
pub unsafe extern "C" fn halo_large_identity(buffer: *mut c_char, size: u32) -> u32 {
    guard(0, || {
        let g = global();
        let identity =
            g.session.as_ref().and_then(|s| s.identity()).or_else(|| g.browser.as_ref().and_then(|b| b.identity()));
        unsafe { copy_text(&identity.unwrap_or_default(), buffer, size) }
    })
}

/// Why the match has not given the player a seat, if it has not: returns 1
/// when the player is banned, 2 when the match is full, 3 for any other
/// reason, 0 when there is no refusal. The words to show the player are copied
/// as [`halo_large_error`] copies.
///
/// # Safety
/// `buffer` points to `size` writable `char`s.
#[no_mangle]
pub unsafe extern "C" fn halo_large_refusal(buffer: *mut c_char, size: u32) -> u32 {
    guard(0, || {
        let g = global();
        let Some(refusal) = g.session.as_ref().and_then(|s| s.refusal()) else { return 0 };
        unsafe { copy_text(&refusal.message, buffer, size) };
        match refusal.kind {
            RefusalKind::Banned => 1,
            RefusalKind::Full => 2,
            RefusalKind::Other => 3,
        }
    })
}
