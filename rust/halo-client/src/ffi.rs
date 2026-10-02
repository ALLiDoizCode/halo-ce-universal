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
use std::sync::Mutex;

use crate::session::{Config, RemoteUnit, Session};

#[derive(Default)]
struct Global {
    session: Option<Session>,
    /// What [`halo_large_frame`] froze, for [`halo_large_unit`] to read.
    frame: Vec<RemoteUnit>,
    error: String,
}

static GLOBAL: Mutex<Global> = Mutex::new(Global { session: None, frame: Vec::new(), error: String::new() });

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
            g.session.take()
        };
        drop(old);
        match Session::start(Config { gateway, spacetimedb, database, token: None }) {
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
            g.session.take()
        };
        drop(old);
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
/// `out` has eight `float`s: position `x y z` (world units), velocity
/// `x y z` (world units a second), yaw and pitch (radians).
///
/// # Safety
/// `player` and `tick` point to a writable `unsigned long` each, `out` to eight
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
            std::slice::from_raw_parts_mut(out, 8).copy_from_slice(&[
                s.position[0],
                s.position[1],
                s.position[2],
                s.velocity[0],
                s.velocity[1],
                s.velocity[2],
                s.yaw,
                s.pitch,
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

/// Tell the gateway where the local player is now: this tick's input. Does
/// nothing until the gateway has welcomed the player.
#[no_mangle]
pub extern "C" fn halo_large_send_input(x: f32, y: f32, z: f32, yaw: f32, pitch: f32) {
    guard((), || {
        if let Some(session) = &global().session {
            session.send_input([x, y, z], yaw, pitch);
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
