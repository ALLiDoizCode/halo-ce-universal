//! One server of the list, run: its rotation of matches, each in a database of
//! its own, with the next match made ready before the last one's time is up
//! and the players handed over by the server list.
//!
//! # A match's life
//!
//! 1. A fresh database is published from the match module, loaded with the
//!    map, spawn points, capacity and the bans in force, started, and given a
//!    gateway on the server's next UDP port.
//! 2. The root database's row for the server is rewritten to name it
//!    (`set_server`): from then on the server list sends players there, and
//!    clients that are in the last match follow.
//! 3. It runs until [`match_is_over`] says it has ended.
//! 4. A little before that the next match of the rotation is made, so that
//!    the handover is a change of row and not a wait. The last match is kept
//!    for `handover_secs` more (so that nobody is cut off before their client
//!    has moved) and then its gateway is stopped and its database deleted.
//!
//! # Bans
//!
//! The root database's `banned` table is the truth. Every change to it comes
//! here as a [`BanChange`], and is applied to every match that exists (the
//! match's own `set_ban` takes a seated player out, and refuses the identity's
//! next `join`); each new match starts with all of them.

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
use crate::root::{BanChange, Root};

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

/// Why a match is over, if it is. Today only the rotation's time limit ends
/// one: the game types' own endings (a score reached, the clock of the
/// game's rules) are the rules ticket's to add here, by looking at the
/// match's state. Nothing else in the orchestration needs to change for them.
pub fn match_is_over(step: &Rotation, elapsed: Duration) -> Option<&'static str> {
    (step.seconds > 0 && elapsed >= Duration::from_secs(step.seconds as u64)).then_some("its time is up")
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
}

pub struct ServerRun {
    shared: Arc<Shared>,
    server: config::Server,
    bans: BTreeMap<Identity, String>,
    changes: Receiver<BanChange>,
    /// Matches made so far (numbers the next).
    made: u64,
    unix_secs: u64,
}

impl ServerRun {
    pub fn new(shared: Arc<Shared>, server: config::Server, changes: Receiver<BanChange>) -> ServerRun {
        let bans = shared.root.bans().into_iter().map(|b| (b.identity, b.reason)).collect();
        let unix_secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        ServerRun { shared, server, bans, changes, made: 0, unix_secs }
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
                    Ok(live) => {
                        let listed = self.announce(&live);
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

            if let Some(live) = &current {
                let elapsed = live.matched.started.elapsed();
                let over = match_is_over(&live.step, elapsed)
                    .or_else(|| (!live.matched.is_connected()).then_some("its connection to SpacetimeDB was lost"));
                // the next match, made ahead of time
                let limit = live.step.seconds as u64;
                let prepare_from = limit.saturating_sub(PREPARE_SECS.min(limit / 2));
                if next.is_none()
                    && Instant::now() >= retry_at
                    && limit > 0
                    && elapsed >= Duration::from_secs(prepare_from)
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
                        Some(upcoming) => {
                            let listed = self.announce(&upcoming);
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
        let spec = MatchSpec {
            database: database.clone(),
            map: &map,
            capacity: step.capacity,
            budget,
            send_threads: self.server.send_threads,
            bind,
            bans: self.bans.iter().map(|(i, r)| (*i, r.clone())).collect(),
        };
        let started = Instant::now();
        let matched = RunningMatch::start(&self.shared.env, spec)?;
        self.say(format!(
            "match {number} ready in {:.1} s: {} on {} ({}), database {database}, up to {} players at {} B/s each, UDP {bind}",
            started.elapsed().as_secs_f64(),
            step.game_type,
            step.map,
            if step.variant.is_empty() { "no variant" } else { &step.variant },
            step.capacity,
            budget,
        ));
        Ok(Live { matched, capacity: step.capacity as u32, step, number })
    }

    /// Write the server's row so that it names this match: the players go here.
    fn announce(&self, live: &Live) -> Result<(), String> {
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
                BanChange::Banned { identity, reason } => {
                    self.bans.insert(*identity, reason.clone());
                }
                BanChange::Lifted { identity } => {
                    self.bans.remove(identity);
                }
            }
            let matches = current
                .iter()
                .map(|l| &l.matched)
                .chain(next.iter().map(|l| &l.matched))
                .chain(draining.iter().map(|(_, m)| m));
            for m in matches {
                let result = match &change {
                    BanChange::Banned { identity, reason } => m.set_ban(*identity, reason),
                    BanChange::Lifted { identity } => m.clear_ban(*identity),
                };
                match (&change, result) {
                    (BanChange::Banned { identity, reason }, Ok(())) => {
                        self.say(format!("{} banned from {} ({reason})", identity.to_hex(), m.database))
                    }
                    (BanChange::Lifted { identity }, Ok(())) => {
                        self.say(format!("{} unbanned in {}", identity.to_hex(), m.database))
                    }
                    (_, Err(e)) => self.say(format!("a ban change did not reach {}: {e}", m.database)),
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

/// One line of the log: tick time, player count, bandwidth, rejected moves.
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
         inputs late {} unbound {} | gateway ticks missed {}, send p50 {:.2} max {:.2} ms | {left}",
        report.players,
        report.capacity,
        sessions,
        report.out_bytes_per_second / 1e6,
        report.rejected_total,
        report.rejected,
        report.seconds,
        report.inputs_late,
        report.inputs_unbound,
        report.ticks_missed,
        report.send_ms_p50,
        report.send_ms_max,
    )
}
