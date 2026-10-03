//! One server of the list, run: its rotation of matches, each in a database of
//! its own, with the next match made ready before the last one's time is up
//! and the players handed over by the server list.
//!
//! # A match's life
//!
//! 1. A fresh database is published from the match module, loaded with the
//!    map (its player starting locations are where the game's rules spawn
//!    players), the game's rules and limits, the capacity and the bans in
//!    force, started, and given a
//!    gateway on the server's next UDP port.
//! 2. The root database's row for the server is rewritten to name it
//!    (`set_server`): from then on the server list sends players there, and
//!    clients that are in the last match follow.
//! 3. The game's clock starts when the match is announced (`begin_game`). The
//!    match ends when the game does: a player (a team, in team Slayer) reaches
//!    the score limit, or the time limit runs out, which the match's own rules
//!    count (`game_state`, which the orchestration reads). It then stays up
//!    for `end_secs` with its final scoreboard, and [`match_is_over`] says it
//!    is over.
//! 4. When the end is near (a little before the time limit, or as the game
//!    ends) the next match of the rotation is made, so that the handover is a
//!    change of row and not a wait. The last match is kept for `handover_secs`
//!    more (so that nobody is cut off before their client has moved) and then
//!    its gateway is stopped and its database deleted.
//!
//! # Bans and names
//!
//! The root database's `banned` table is the truth, and so are the names in
//! `known_identity`. Every change to them comes here as a [`RootChange`], and
//! is applied to every match that exists (the match's own `set_ban` takes a
//! seated player out, and refuses the identity's next `join`; `set_name` puts a
//! player's chosen name on the roster); each new match starts with all of them.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use halo_match_driver::root_bindings::Server as ServerRow;
use spacetimedb_sdk::Identity;

use crate::config::{self, Rotation};
use crate::maps::{LoadedMap, MapSource};
use crate::matches::{Env, MatchSpec, Report, RunningMatch};
use crate::root::{Root, RootChange};

/// How long before a match's end the next one is made.
const PREPARE_SECS: u64 = 15;
/// How long a match that failed to start waits before another try.
const RETRY_SECS: u64 = 5;
/// How often the housekeeping runs.
const POLL: Duration = Duration::from_millis(200);
/// How often a changed player count is written to the list.
const COUNT_EVERY: Duration = Duration::from_secs(2);

/// The log: lines on stderr, stamped with seconds since the program started,
/// and (for tests) kept.
#[derive(Clone)]
pub struct Log {
    start: Instant,
    kept: Option<Arc<Mutex<Vec<String>>>>,
}

impl Log {
    pub fn new() -> Log {
        Log { start: Instant::now(), kept: None }
    }

    /// A log that also keeps every line, for the test to read.
    pub fn keeping() -> (Log, Arc<Mutex<Vec<String>>>) {
        let kept = Arc::new(Mutex::new(Vec::new()));
        (Log { start: Instant::now(), kept: Some(kept.clone()) }, kept)
    }

    pub fn line(&self, who: &str, message: impl AsRef<str>) {
        let line = format!("[{:>6}s] {who}: {}", self.start.elapsed().as_secs(), message.as_ref());
        eprintln!("{line}");
        if let Some(kept) = &self.kept {
            kept.lock().unwrap_or_else(|p| p.into_inner()).push(line);
        }
    }
}

impl Default for Log {
    fn default() -> Log {
        Log::new()
    }
}

/// How long past its time limit and its final scoreboard a match whose game
/// has not ended is left running before the orchestration ends it anyway.
const END_GRACE_SECS: u64 = 10;

/// Why a match is over, if it is. The game ends it (a player or a team
/// reaches the score limit, or the time limit runs out, which the match's
/// rules count): `ended` is that reason and how long ago it was, and the
/// match is over once its final scoreboard has been up for `scoreboard`.
/// Should the game not end by the time limit (a match module that has stopped
/// ticking), the time limit and then some is the end all the same.
pub fn match_is_over(
    step: &Rotation,
    elapsed: Duration,
    ended: Option<(&'static str, Duration)>,
    scoreboard: Duration,
) -> Option<&'static str> {
    match ended {
        Some((why, since)) => (since >= scoreboard).then_some(why),
        None => (step.seconds > 0
            && elapsed >= Duration::from_secs(step.seconds as u64) + scoreboard + Duration::from_secs(END_GRACE_SECS))
        .then_some("its time is up"),
    }
}

/// What a server run needs from the program around it.
pub struct Shared {
    pub env: Env,
    pub maps: Arc<dyn MapSource>,
    pub root: Root,
    pub log: Log,
    pub stop: AtomicBool,
}

struct Live {
    matched: RunningMatch,
    step: Rotation,
    number: u64,
    capacity: u32,
    /// When the game ended, and why: from then the final scoreboard is up.
    ended: Option<(Instant, &'static str)>,
}

pub struct ServerRun {
    shared: Arc<Shared>,
    server: config::Server,
    bans: BTreeMap<Identity, String>,
    names: BTreeMap<Identity, String>,
    changes: Receiver<RootChange>,
    /// Matches made so far (numbers the next).
    made: u64,
    unix_secs: u64,
}

impl ServerRun {
    pub fn new(shared: Arc<Shared>, server: config::Server, changes: Receiver<RootChange>) -> ServerRun {
        let bans = shared.root.bans().into_iter().map(|b| (b.identity, b.reason)).collect();
        let names = shared.root.names().into_iter().collect();
        let unix_secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        ServerRun { shared, server, bans, names, changes, made: 0, unix_secs }
    }

    fn say(&self, message: impl AsRef<str>) {
        self.shared.log.line(&self.server.id, message);
    }

    /// Run the rotation until told to stop, then take the server off the list
    /// and every match down.
    pub fn run(mut self) -> Result<(), String> {
        let mut current: Option<Live> = None;
        let mut next: Option<Live> = None;
        let mut draining: Vec<(Instant, RunningMatch)> = Vec::new();
        let mut last_log = Instant::now();
        let mut last_count = (Instant::now(), u32::MAX);
        let mut retry_at = Instant::now();
        let mut result = Ok(());

        'main: while !self.shared.stop.load(Relaxed) {
            self.apply_ban_changes(&current, &next, &draining);

            // a first match, or one to replace a match that was lost
            if current.is_none() && Instant::now() >= retry_at {
                match self.make_next(&mut draining) {
                    Ok(mut live) => {
                        let listed = self.announce(&mut live);
                        current = Some(live);
                        last_log = Instant::now();
                        if let Err(e) = listed {
                            result = Err(e);
                            break 'main;
                        }
                    }
                    Err(e) => {
                        self.say(format!("could not start a match: {e}; trying again in {RETRY_SECS} s"));
                        retry_at = Instant::now() + Duration::from_secs(RETRY_SECS);
                    }
                }
            }

            // the game ending (a score reached, or its time): the final scoreboard is up from then
            if let Some(live) = current.as_mut() {
                if live.ended.is_none() {
                    if let Some(ended) = live.matched.ended() {
                        live.ended = Some((Instant::now(), ended.reason));
                        let (number, map) = (live.number, live.step.map.clone());
                        self.say(format!(
                            "match {number} ({map}) has ended: {}, {}; the final scoreboard is up for {} s",
                            ended.reason, ended.winner, self.server.end_secs
                        ));
                    }
                }
            }
            if let Some(live) = &current {
                let elapsed = live.matched.started.elapsed();
                let scoreboard = Duration::from_secs(self.server.end_secs);
                let over =
                    match_is_over(&live.step, elapsed, live.ended.map(|(at, why)| (why, at.elapsed())), scoreboard)
                        .or_else(|| (!live.matched.is_connected()).then_some("its connection to SpacetimeDB was lost"));
                // the next match, made ahead of time: before the time limit, or as the game ends
                let limit = live.step.seconds as u64;
                let prepare_from = limit.saturating_sub(PREPARE_SECS.min(limit / 2));
                if next.is_none()
                    && Instant::now() >= retry_at
                    && (live.ended.is_some() || (limit > 0 && elapsed >= Duration::from_secs(prepare_from)))
                {
                    match self.make_next(&mut draining) {
                        Ok(made) => next = Some(made),
                        Err(e) => {
                            self.say(format!("could not prepare the next match: {e}"));
                            retry_at = Instant::now() + Duration::from_secs(RETRY_SECS);
                        }
                    }
                }
                if let Some(why) = over {
                    self.say(format!("match {} ({}) is over: {why}", live.number, live.step.map));
                    let finished = current.take().expect("there is a current match");
                    let upcoming = match next.take() {
                        Some(upcoming) => Some(upcoming),
                        None if self.shared.stop.load(Relaxed) => None,
                        None => match self.make_next(&mut draining) {
                            Ok(made) => Some(made),
                            Err(e) => {
                                self.say(format!("could not start the next match: {e}"));
                                retry_at = Instant::now() + Duration::from_secs(RETRY_SECS);
                                None
                            }
                        },
                    };
                    match upcoming {
                        Some(mut upcoming) => {
                            let listed = self.announce(&mut upcoming);
                            current = Some(upcoming);
                            last_log = Instant::now();
                            last_count = (Instant::now(), u32::MAX);
                            let until = Instant::now() + Duration::from_secs(self.server.handover_secs);
                            draining.push((until, finished.matched));
                            if let Err(e) = listed {
                                result = Err(e);
                                break 'main;
                            }
                        }
                        None => {
                            // nothing to move the players to: the list says the server is between matches
                            self.announce_between();
                            draining.push((Instant::now(), finished.matched));
                        }
                    }
                }
            }

            // the players, for the list
            if let Some(live) = &current {
                if last_count.0.elapsed() >= COUNT_EVERY {
                    let players = live.matched.players();
                    if players != last_count.1 {
                        if let Err(e) = self.shared.root.set_players(&self.server.id, players) {
                            self.say(format!("the player count could not be listed: {e}"));
                        }
                        last_count = (Instant::now(), players);
                    } else {
                        last_count.0 = Instant::now();
                    }
                }
            }

            // matches whose players have moved on
            let mut kept = Vec::new();
            for (until, old) in draining.drain(..) {
                if Instant::now() >= until {
                    self.take_down(old);
                } else {
                    kept.push((until, old));
                }
            }
            draining = kept;

            if self.server.log_secs > 0 && last_log.elapsed() >= Duration::from_secs(self.server.log_secs) {
                last_log = Instant::now();
                if let Some(live) = current.as_mut() {
                    self.log_report(live);
                }
            }
            std::thread::sleep(POLL);
        }

        // shutting down: off the list first, so that nobody picks a server that is going
        if let Err(e) = self.shared.root.remove_server(&self.server.id) {
            self.say(format!("could not take the server off the list: {e}"));
            result = Err(e);
        }
        for live in current.into_iter().chain(next) {
            self.take_down(live.matched);
        }
        for (_, old) in draining {
            self.take_down(old);
        }
        result
    }

    /// Make the next match of the rotation, ready to be announced.
    fn make_next(&mut self, draining: &mut Vec<(Instant, RunningMatch)>) -> Result<Live, String> {
        let number = self.made + 1;
        let step = self.server.rotation[(self.made % self.server.rotation.len() as u64) as usize].clone();
        let bind = self.server.gateway_bind(number);
        // the port it would take may still be the match before last's: let go of it first
        let mut kept = Vec::new();
        for (until, old) in draining.drain(..) {
            if old.bind == bind {
                self.take_down(old);
            } else {
                kept.push((until, old));
            }
        }
        *draining = kept;

        self.made = number;
        let map: LoadedMap = self.shared.maps.load(&step.map)?;
        let database = format!("hm-{}-{}-{number}", self.server.id, self.unix_secs);
        let budget = step.budget.unwrap_or(self.server.budget);
        let capacity = step.capacity.unwrap_or(map.default_capacity);
        let rules = step.rules()?;
        let spec = MatchSpec {
            database: database.clone(),
            map: &map,
            capacity,
            rules,
            budget,
            send_threads: self.server.send_threads,
            bind,
            bans: self.bans.iter().map(|(i, r)| (*i, r.clone())).collect(),
            names: self.names.iter().map(|(i, n)| (*i, n.clone())).collect(),
        };
        let started = Instant::now();
        let matched = RunningMatch::start(&self.shared.env, spec)?;
        self.say(format!(
            "match {number} ready in {:.1} s: {} on {} ({}), database {database}, up to {capacity} players{} at {budget} B/s each, \
             {} to {}, UDP {bind}",
            started.elapsed().as_secs_f64(),
            step.game_type,
            step.map,
            if step.variant.is_empty() { "no variant" } else { &step.variant },
            if step.capacity.is_none() { " (the map's own cap)" } else { "" },
            if rules.score_limit > 0 { format!("score limit {}", rules.score_limit) } else { "no score limit".to_string() },
            if step.seconds > 0 { format!("time limit {} s", step.seconds) } else { "no time limit".to_string() },
        ));
        Ok(Live { matched, capacity: capacity as u32, step, number, ended: None })
    }

    /// Write the server's row so that it names this match: the players go here.
    fn announce(&self, live: &mut Live) -> Result<(), String> {
        // its time begins now, not when it was made ready (up to PREPARE_SECS earlier)
        live.matched.started = Instant::now();
        live.matched.begin_game().map_err(|e| format!("beginning the game: {e}"))?;
        let started_us = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_micros() as i64);
        let row = ServerRow {
            id: self.server.id.clone(),
            title: self.server.title().to_string(),
            map: live.step.map.clone(),
            game_type: live.step.game_type.clone(),
            variant: live.step.variant.clone(),
            database: live.matched.database.clone(),
            gateway: self.server.gateway_advertised(live.number),
            players: 0,
            capacity: live.capacity,
            match_number: live.number,
            match_seconds: live.step.seconds,
            match_started_us: started_us,
            updated_us: 0,
        };
        self.shared.root.set_server(row).map_err(|e| format!("listing the server: {e}"))?;
        self.say(format!("listed: match {} on {}, database {}", live.number, live.step.map, live.matched.database));
        Ok(())
    }

    /// The list, between matches: no database to join.
    fn announce_between(&self) {
        let row = self.shared.root.servers().into_iter().find(|s| s.id == self.server.id);
        if let Some(row) = row {
            let row = ServerRow { database: String::new(), players: 0, ..row };
            if let Err(e) = self.shared.root.set_server(row) {
                self.say(format!("could not list the server as between matches: {e}"));
            }
        }
    }

    fn take_down(&self, old: RunningMatch) {
        let database = old.database.clone();
        match old.stop(&self.shared.env) {
            Ok(()) => self.say(format!("database {database} deleted")),
            Err(e) => self.say(format!("database {database} could not be deleted: {e}")),
        }
    }

    fn apply_ban_changes(&mut self, current: &Option<Live>, next: &Option<Live>, draining: &[(Instant, RunningMatch)]) {
        loop {
            let change = match self.changes.try_recv() {
                Ok(change) => change,
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return,
            };
            match &change {
                RootChange::Banned { identity, reason } => {
                    self.bans.insert(*identity, reason.clone());
                }
                RootChange::Lifted { identity } => {
                    self.bans.remove(identity);
                }
                RootChange::Named { identity, name } if name.is_empty() => {
                    self.names.remove(identity);
                }
                RootChange::Named { identity, name } => {
                    self.names.insert(*identity, name.clone());
                }
            }
            let matches = current
                .iter()
                .map(|l| &l.matched)
                .chain(next.iter().map(|l| &l.matched))
                .chain(draining.iter().map(|(_, m)| m));
            for m in matches {
                let result = match &change {
                    RootChange::Banned { identity, reason } => m.set_ban(*identity, reason),
                    RootChange::Lifted { identity } => m.clear_ban(*identity),
                    RootChange::Named { identity, name } => m.set_name(*identity, name),
                };
                match (&change, result) {
                    (RootChange::Banned { identity, reason }, Ok(())) => {
                        self.say(format!("{} banned from {} ({reason})", identity.to_hex(), m.database))
                    }
                    (RootChange::Lifted { identity }, Ok(())) => {
                        self.say(format!("{} unbanned in {}", identity.to_hex(), m.database))
                    }
                    // (a name is not worth a line each: there is one for every player who joins)
                    (RootChange::Named { .. }, Ok(())) => {}
                    (_, Err(e)) => self.say(format!("a change did not reach {}: {e}", m.database)),
                }
            }
        }
    }

    fn log_report(&self, live: &mut Live) {
        let report = live.matched.report(&self.shared.env, live.capacity);
        self.say(format!(
            "match {} {} {}",
            live.number,
            live.step.map,
            format_report(&report, live.matched.sessions(), live.matched.started.elapsed(), &live.step)
        ));
    }
}

/// One line of the log: tick time, player count, bandwidth, rejected moves and rejected hit reports.
pub fn format_report(report: &Report, sessions: usize, elapsed: Duration, step: &Rotation) -> String {
    let tick = match report.tick_ms {
        Some((whole, module)) => format!("tick {whole:.2} ms (module {module:.2}) x{:.0}", report.ticks),
        None => "tick -".to_string(),
    };
    let per_player =
        if report.players > 0 { report.out_bytes_per_second / report.players as f64 / 1000.0 } else { 0.0 };
    let left = if step.seconds > 0 {
        format!("{} s left", (step.seconds as u64).saturating_sub(elapsed.as_secs()))
    } else {
        "no time limit".to_string()
    };
    format!(
        "{tick} | players {}/{} ({} on UDP) | out {:.2} MB/s ({per_player:.1} KB/s a player) | rejected moves {} (+{} in {:.0} s) | \
         rejected hits {} (+{}) | inputs late {} unbound {} | gateway ticks missed {}, send p50 {:.2} max {:.2} ms | {left}",
        report.players,
        report.capacity,
        sessions,
        report.out_bytes_per_second / 1e6,
        report.rejected_total,
        report.rejected,
        report.seconds,
        report.rejected_hits_total,
        report.rejected_hits,
        report.inputs_late,
        report.inputs_unbound,
        report.ticks_missed,
        report.send_ms_p50,
        report.send_ms_max,
    )
}
