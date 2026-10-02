//! The root database: the one permanent database of an operator's
//! large-scale servers. It holds
//!
//! - the **server list** (`server`, public): each server the operator runs,
//!   with the map and game type of its current match, the database that match
//!   is, where players send their UDP, and how full it is. The orchestration
//!   (`halo-server`) writes it; players read it;
//! - **identity** (`known_identity`, public): every identity that has said it
//!   is here. A player's identity is the SpacetimeDB identity of the token the
//!   client keeps, which is the same on every database of the instance, so the
//!   one that registered here is the one that sits in a match's seat and the
//!   one a ban names;
//! - the **bans** (`banned`, public: who is banned and why is no secret, and
//!   the orchestration reads it with an ordinary subscription).
//!
//! A module cannot query another database from inside a reducer, so a ban
//! made here reaches a match by way of the orchestration: it subscribes to
//! `banned` and calls the match's own `set_ban` on every running match (and on
//! each new match as it starts). `register` refuses a banned identity with the
//! same message a match's `join` does, so that a player learns they are banned
//! before they choose a server.
//!
//! # Who may call what
//!
//! - The **owner** (the identity that published the database, which `init`
//!   records) calls `set_server`, `set_server_players`, `remove_server`,
//!   `clear_servers`, `ban_identity` and `unban_identity`.
//! - **Anyone** calls `register`.

use spacetimedb::{reducer, table, Identity, ReducerContext, Table};

/// The prefix of the message that refuses a banned identity, here and in a
/// match's `join`: the text after it is the reason.
pub const BANNED_PREFIX: &str = "banned: ";

/// The one row of a single-row table.
const ONLY: u8 = 0;

/// Longest text a server's fields may hold, so that a list stays a list.
const MAX_TEXT: usize = 96;

#[table(accessor = root_config)]
pub struct RootConfig {
    #[primary_key]
    id: u8,
    owner: Identity,
}

/// One server of the list. Public.
#[table(accessor = server, public)]
pub struct Server {
    /// The operator's name for the server: short, unique, stable across matches.
    #[primary_key]
    pub id: String,
    /// What the list shows.
    pub title: String,
    /// The current match's map (`bloodgulch`), game type (`slayer`) and variant (free text).
    pub map: String,
    pub game_type: String,
    pub variant: String,
    /// The current match's database; empty between matches.
    pub database: String,
    /// Where the players send UDP, `host:port`.
    pub gateway: String,
    pub players: u32,
    pub capacity: u32,
    /// Matches this server has run since it started.
    pub match_number: u64,
    /// Seconds the match is to last (0: no limit), and when it started,
    /// microseconds since the Unix epoch.
    pub match_seconds: u32,
    pub match_started_us: i64,
    /// When the orchestration last wrote the row.
    pub updated_us: i64,
}

/// Someone who has said they are here. Public.
#[table(accessor = known_identity, public)]
pub struct KnownIdentity {
    #[primary_key]
    pub identity: Identity,
    /// The name they play under (see [`clean_name`]); empty for none.
    pub name: String,
    pub first_seen_us: i64,
    pub last_seen_us: i64,
}

/// A banned identity. Public.
#[table(accessor = banned, public)]
pub struct Banned {
    #[primary_key]
    pub identity: Identity,
    pub reason: String,
    pub banned_us: i64,
}

fn require_owner(ctx: &ReducerContext) -> Result<(), String> {
    match ctx.db.root_config().id().find(ONLY) {
        Some(config) if config.owner == ctx.sender() => Ok(()),
        _ => Err("only the root database's owner may do that".into()),
    }
}

fn now_us(ctx: &ReducerContext) -> i64 {
    ctx.timestamp.to_micros_since_unix_epoch()
}

fn check_text(what: &str, text: &str) -> Result<(), String> {
    if text.len() > MAX_TEXT {
        return Err(format!("{what} is {} bytes: at most {MAX_TEXT}", text.len()));
    }
    Ok(())
}

#[reducer(init)]
pub fn init(ctx: &ReducerContext) {
    // whoever publishes owns the database
    ctx.db.root_config().insert(RootConfig { id: ONLY, owner: ctx.sender() });
}

/// Longest name a player has: the engine's name field.
pub const MAX_NAME: usize = 11;

/// A name as the game shows it: letters, digits, spaces and `_ . -` only, the
/// first [`MAX_NAME`] of them, trimmed; empty for nothing. (The match module
/// cleans a name the same way.)
pub fn clean_name(name: &str) -> String {
    let kept: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '_' | '.' | '-'))
        .take(MAX_NAME)
        .collect();
    kept.trim().to_string()
}

/// Say that the caller is here, under this name (which may be empty): their
/// identity is known from now on, which is how a player's identity and name
/// show in the list of who has played, and the orchestration carries the name
/// to the matches' rosters. Refused, with the reason, if the identity is banned.
#[reducer]
pub fn register(ctx: &ReducerContext, name: String) -> Result<(), String> {
    let me = ctx.sender();
    if let Some(ban) = ctx.db.banned().identity().find(me) {
        return Err(format!("{BANNED_PREFIX}{}", ban.reason));
    }
    let name = clean_name(&name);
    let now = now_us(ctx);
    match ctx.db.known_identity().identity().find(me) {
        Some(mut known) => {
            known.last_seen_us = now;
            known.name = name;
            ctx.db.known_identity().identity().update(known);
        }
        None => {
            ctx.db.known_identity().insert(KnownIdentity { identity: me, name, first_seen_us: now, last_seen_us: now });
        }
    }
    Ok(())
}

/// Add a server to the list, or replace its row.
#[reducer]
pub fn set_server(ctx: &ReducerContext, row: Server) -> Result<(), String> {
    require_owner(ctx)?;
    if row.id.is_empty() {
        return Err("a server needs an id".into());
    }
    for (what, text) in [
        ("the id", &row.id),
        ("the title", &row.title),
        ("the map", &row.map),
        ("the game type", &row.game_type),
        ("the variant", &row.variant),
        ("the database", &row.database),
        ("the gateway", &row.gateway),
    ] {
        check_text(what, text)?;
    }
    let row = Server { updated_us: now_us(ctx), ..row };
    if ctx.db.server().id().find(&row.id).is_some() {
        ctx.db.server().id().update(row);
    } else {
        ctx.db.server().insert(row);
    }
    Ok(())
}

/// How many players a server has now (the list's count).
#[reducer]
pub fn set_server_players(ctx: &ReducerContext, id: String, players: u32) -> Result<(), String> {
    require_owner(ctx)?;
    let mut row = ctx.db.server().id().find(&id).ok_or_else(|| format!("no server {id:?}"))?;
    if row.players != players {
        row.players = players;
        row.updated_us = now_us(ctx);
        ctx.db.server().id().update(row);
    }
    Ok(())
}

#[reducer]
pub fn remove_server(ctx: &ReducerContext, id: String) -> Result<(), String> {
    require_owner(ctx)?;
    ctx.db.server().id().delete(&id);
    Ok(())
}

/// Empty the list (a restarted orchestration starts from nothing).
#[reducer]
pub fn clear_servers(ctx: &ReducerContext) -> Result<(), String> {
    require_owner(ctx)?;
    let ids: Vec<String> = ctx.db.server().iter().map(|s| s.id).collect();
    for id in ids {
        ctx.db.server().id().delete(&id);
    }
    Ok(())
}

/// Ban an identity, with the reason the player is told. Banning again
/// replaces the reason.
#[reducer]
pub fn ban_identity(ctx: &ReducerContext, identity: Identity, reason: String) -> Result<(), String> {
    require_owner(ctx)?;
    check_text("the reason", &reason)?;
    let row = Banned { identity, reason, banned_us: now_us(ctx) };
    if ctx.db.banned().identity().find(identity).is_some() {
        ctx.db.banned().identity().update(row);
    } else {
        ctx.db.banned().insert(row);
    }
    Ok(())
}

#[reducer]
pub fn unban_identity(ctx: &ReducerContext, identity: Identity) -> Result<(), String> {
    require_owner(ctx)?;
    ctx.db.banned().identity().delete(identity);
    Ok(())
}
