//! The rules of the game, for Slayer and Team Slayer: who is alive, where
//! players spawn, what a death is worth, and when the match is over. The
//! engine's own (`source/game/game_engine.c`, `game_engine_slayer.c`) are the
//! reference; [`crate::spawn`] has the choice of a place to spawn.
//!
//! The server owns all of it (a client decides only its own movement): the
//! match module calls [`play`] once a tick, in place of [`crate::step`], with
//! the tick's input and the deaths the server has decided on, and keeps the
//! [`Game`] and each player's [`Contestant`] in its public tables.
//!
//! # Deaths
//!
//! Nothing here deals damage. A death comes in as a [`Death`]: the victim and
//! who gets the credit. The weapons ticket's hit validation produces them (a
//! validated hit that brings a player's health to nothing is a `Death`); the
//! rules apply the score and set the respawn timer. A death of a player who
//! is not alive, or after the match is over, is refused (a
//! [`GameEvent::DeathRefused`]), so it cannot count twice.
//!
//! - A kill of an enemy is worth 1 to the killer (and the killer's team, in a
//!   team game). A kill of a teammate, and a death with no killer or by the
//!   player's own hand, is worth -1 (a death with no killer is the player's
//!   own, as the engine counts it); a death by a player who has left the match
//!   is worth nothing to anyone.
//! - The respawn timer is the engine's: the player's penalty plus the
//!   variant's respawn time, plus the suicide penalty when the death was not a
//!   kill of an enemy, and at least three seconds. A variant with a respawn
//!   time growth adds it to the dead player's penalty (up to five times it)
//!   and takes it off the killer's.
//!
//! # Spawning and waves
//!
//! A player who joins, and a dead player whose timer has run out, spawn at
//! once if a starting location is free by the engine's rules. If none is, they
//! wait for the next **wave**: a tick that is a multiple of
//! [`Rules::wave_ticks`] counted from the start of the match. In a wave the
//! players who are waiting, longest first, are put at a free location or,
//! when all are taken, beside one (see [`crate::spawn`]). A player the wave
//! could not place waits for the next. [`Contestant::life`] says when the
//! wave is, so that the player can be told.
//!
//! # The end
//!
//! The match is over when a player (a team, in a team game) has the score
//! limit, or the time limit has run out: [`Game::ending`] says why and who
//! won. After that nobody spawns or scores; the scoreboard stays as it was.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::map::MapData;
use crate::rng::Rng;
use crate::spawn::{self, Occupant};
use crate::state::{Player, PlayerId, Store};
use crate::step::{step, Event, PlayerInput};
use crate::TICKS_PER_SECOND;

/// The shortest a respawn timer is, whatever the variant says (the engine's
/// 90 ticks).
pub const MIN_RESPAWN_TICKS: u32 = 90;

/// Teams, as the engine numbers them: red is 0 and blue is 1.
pub const TEAMS: u8 = 2;

/// What a variant of Slayer sets (the engine's `universal_variant`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rules {
    /// Team Slayer.
    pub teams: bool,
    /// The score that ends the match: a player's, or a team's; 0 for none.
    pub score_limit: u32,
    /// Ticks the match lasts, from [`Game::started_tick`]; 0 for no limit.
    pub time_limit_ticks: u32,
    pub respawn_ticks: u32,
    pub respawn_growth_ticks: u32,
    pub suicide_penalty_ticks: u32,
    /// Ticks between waves (at least 1).
    pub wave_ticks: u32,
}

impl Rules {
    /// The engine's default Slayer variant: 15 kills, no respawn time but the
    /// minimum, 10 seconds more for a suicide.
    pub fn slayer() -> Rules {
        Rules {
            teams: false,
            score_limit: 15,
            time_limit_ticks: 0,
            respawn_ticks: 0,
            respawn_growth_ticks: 0,
            suicide_penalty_ticks: 10 * TICKS_PER_SECOND,
            wave_ticks: 5 * TICKS_PER_SECOND,
        }
    }

    /// The engine's default Team Slayer variant: a team's 50 kills, a 10
    /// second respawn.
    pub fn team_slayer() -> Rules {
        Rules { teams: true, score_limit: 50, respawn_ticks: 10 * TICKS_PER_SECOND, ..Rules::slayer() }
    }

    fn wave_ticks(&self) -> u64 {
        self.wave_ticks.max(1) as u64
    }
}

/// Whether a player is in the world.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Life {
    Alive,
    /// Dead; the respawn timer runs out at this tick.
    Dead {
        due: u64,
    },
    /// The timer has run out (or the player has just joined) and no starting
    /// location was free: the player spawns in the wave at this tick.
    Waiting {
        wave: u64,
    },
}

/// One player of the match, as the rules hold them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Contestant {
    pub id: PlayerId,
    /// 0 red, 1 blue.
    pub team: u8,
    /// The player's score: kills, less what betrayals and suicides cost.
    pub score: i32,
    pub deaths: u32,
    pub life: Life,
    /// How many times the player has spawned: a change says the player has
    /// been put somewhere new.
    pub spawns: u32,
    /// Where, and facing which way, the player last spawned.
    pub spawn: [f32; 3],
    pub spawn_yaw: f32,
    /// The tick the player last spawned on (0 for one placed by the caller): a
    /// state of the player from before it is from where they were.
    pub spawned_tick: u64,
    /// Ticks added to the respawn timer (a variant's growth).
    pub penalty: u32,
}

impl Contestant {
    fn new(id: PlayerId, team: u8, life: Life) -> Contestant {
        Contestant {
            id,
            team,
            score: 0,
            deaths: 0,
            life,
            spawns: 0,
            spawn: [0.0; 3],
            spawn_yaw: 0.0,
            spawned_tick: 0,
            penalty: 0,
        }
    }

    pub fn is_alive(&self) -> bool {
        self.life == Life::Alive
    }
}

/// Why the match ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    ScoreLimit,
    TimeLimit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Winner {
    /// The top scores are level.
    Nobody,
    Player(PlayerId),
    Team(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ending {
    pub reason: EndReason,
    pub winner: Winner,
}

/// The match: its rules, its clock and the team scores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Game {
    pub rules: Rules,
    /// The tick the clock started on: the time limit and the waves count from it.
    pub started_tick: u64,
    /// Team scores in a team game (red, blue).
    pub team_scores: [i32; 2],
    pub ending: Option<Ending>,
}

impl Game {
    pub fn new(rules: Rules, tick: u64) -> Game {
        Game { rules, started_tick: tick, team_scores: [0; 2], ending: None }
    }

    /// Whether a wave happens on this tick: every [`Rules::wave_ticks`] after the
    /// clock started (not the tick it starts on).
    pub fn is_wave(&self, tick: u64) -> bool {
        tick > self.started_tick && (tick - self.started_tick).is_multiple_of(self.rules.wave_ticks())
    }

    /// The tick of the first wave after `tick`.
    pub fn next_wave(&self, tick: u64) -> u64 {
        let wave = self.rules.wave_ticks();
        let since = tick.saturating_sub(self.started_tick);
        self.started_tick + (since / wave + 1) * wave
    }
}

/// Where the rules keep their state: the server's tables, or memory.
pub trait GameStore {
    fn game(&self) -> Game;
    fn set_game(&mut self, game: Game);
    fn contestant(&self, id: PlayerId) -> Option<Contestant>;
    /// Insert the contestant, or replace the one with the same id.
    fn set_contestant(&mut self, contestant: Contestant);
    fn remove_contestant(&mut self, id: PlayerId) -> bool;
    /// Every contestant, in id order.
    fn contestants(&self) -> Vec<Contestant>;
    /// The contestants who are not alive (a store with an index on life may
    /// answer from it), in id order.
    fn unspawned(&self) -> Vec<Contestant> {
        self.contestants().into_iter().filter(|c| !c.is_alive()).collect()
    }
}

/// A [`GameStore`] in memory.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryGame {
    game: Game,
    contestants: BTreeMap<PlayerId, Contestant>,
}

impl MemoryGame {
    pub fn new(rules: Rules) -> MemoryGame {
        MemoryGame { game: Game::new(rules, 0), contestants: BTreeMap::new() }
    }
}

impl GameStore for MemoryGame {
    fn game(&self) -> Game {
        self.game
    }

    fn set_game(&mut self, game: Game) {
        self.game = game;
    }

    fn contestant(&self, id: PlayerId) -> Option<Contestant> {
        self.contestants.get(&id).copied()
    }

    fn set_contestant(&mut self, contestant: Contestant) {
        self.contestants.insert(contestant.id, contestant);
    }

    fn remove_contestant(&mut self, id: PlayerId) -> bool {
        self.contestants.remove(&id).is_some()
    }

    fn contestants(&self) -> Vec<Contestant> {
        self.contestants.values().copied().collect()
    }
}

/// The whole of a [`GameStore`] as bytes, as [`crate::snapshot`] is for a
/// [`Store`]: two stores are equal exactly when their snapshots are.
pub fn snapshot_game(store: &impl GameStore) -> Vec<u8> {
    let game = store.game();
    let mut out = Vec::new();
    let r = &game.rules;
    out.push(r.teams as u8);
    for v in [
        r.score_limit,
        r.time_limit_ticks,
        r.respawn_ticks,
        r.respawn_growth_ticks,
        r.suicide_penalty_ticks,
        r.wave_ticks,
    ] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&game.started_tick.to_le_bytes());
    for s in game.team_scores {
        out.extend_from_slice(&s.to_le_bytes());
    }
    match game.ending {
        None => out.extend_from_slice(&[0, 0, 0, 0]),
        Some(e) => {
            out.push(1 + e.reason as u8);
            let (kind, who) = match e.winner {
                Winner::Nobody => (0u8, 0u16),
                Winner::Player(p) => (1, p),
                Winner::Team(t) => (2, t as u16),
            };
            out.push(kind);
            out.extend_from_slice(&who.to_le_bytes());
        }
    }
    for c in store.contestants() {
        out.extend_from_slice(&c.id.to_le_bytes());
        out.push(c.team);
        out.extend_from_slice(&c.score.to_le_bytes());
        out.extend_from_slice(&c.deaths.to_le_bytes());
        let (kind, at) = match c.life {
            Life::Alive => (0u8, 0u64),
            Life::Dead { due } => (1, due),
            Life::Waiting { wave } => (2, wave),
        };
        out.push(kind);
        out.extend_from_slice(&at.to_le_bytes());
        out.extend_from_slice(&c.spawns.to_le_bytes());
        for v in c.spawn.iter().chain([&c.spawn_yaw]) {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&c.spawned_tick.to_le_bytes());
        out.extend_from_slice(&c.penalty.to_le_bytes());
    }
    out
}

/// A player's death, as the server has decided it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Death {
    pub victim: PlayerId,
    /// Who gets the credit; `None` for a death nobody caused (a fall, the
    /// world), which is the victim's own.
    pub killer: Option<PlayerId>,
}

/// What a death counted as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeathKind {
    /// An enemy's kill.
    Kill,
    /// A teammate's.
    Betrayal,
    /// The player's own, or nobody's.
    Suicide,
    /// A player who is no longer in the match killed them.
    Unclaimed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeathRefusal {
    /// The victim is not in the match.
    UnknownPlayer,
    /// The victim is not alive: they died already.
    NotAlive,
    /// The match is over.
    GameOver,
}

/// What the rules did in a tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GameEvent {
    Died {
        victim: PlayerId,
        killer: Option<PlayerId>,
        kind: DeathKind,
        respawn_at: u64,
    },
    /// A player's score changed (`score` is what it is now).
    Scored {
        player: PlayerId,
        delta: i32,
        score: i32,
    },
    DeathRefused {
        victim: PlayerId,
        reason: DeathRefusal,
    },
    Spawned {
        player: PlayerId,
        position: [f32; 3],
        yaw: f32,
        wave: bool,
    },
    /// No starting location was free: the player spawns in the wave at this tick.
    Waiting {
        player: PlayerId,
        wave_at: u64,
    },
    Over(Ending),
}

/// What [`play`] did: the moves the step judged, and what the rules did.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub moves: Vec<Event>,
    pub events: Vec<GameEvent>,
}

/// Put a player in the match, to spawn on the next [`play`] (or [`spawn_due`]).
/// `false`, and nothing changed, if they are in it already.
pub fn enter(game: &mut impl GameStore, id: PlayerId, team: u8, tick: u64) -> bool {
    if game.contestant(id).is_some() {
        return false;
    }
    game.set_contestant(Contestant::new(id, team % TEAMS, Life::Dead { due: tick }));
    true
}

/// Put a player in the match, alive where the caller has placed them (the
/// player's row in the [`Store`] is the caller's).
pub fn enter_placed(game: &mut impl GameStore, id: PlayerId, team: u8, position: [f32; 3], yaw: f32) -> bool {
    if game.contestant(id).is_some() {
        return false;
    }
    let mut c = Contestant::new(id, team % TEAMS, Life::Alive);
    c.spawns = 1;
    c.spawn = position;
    c.spawn_yaw = yaw;
    game.set_contestant(c);
    true
}

/// A player leaves: their score goes with them (the match's team scores keep
/// what they made).
pub fn leave(game: &mut impl GameStore, id: PlayerId) -> bool {
    game.remove_contestant(id)
}

/// Start the match's clock at `tick`: scores, deaths and penalties go back to
/// nothing, and the match is not over.
pub fn begin(game: &mut impl GameStore, tick: u64) {
    let mut g = game.game();
    g.started_tick = tick;
    g.team_scores = [0; 2];
    g.ending = None;
    game.set_game(g);
    for mut c in game.contestants() {
        c.score = 0;
        c.deaths = 0;
        c.penalty = 0;
        // (the waves are counted from the new clock: a player waiting for one waits for the next of those)
        if let Life::Waiting { .. } = c.life {
            c.life = Life::Waiting { wave: g.next_wave(tick) };
        }
        game.set_contestant(c);
    }
}

/// How many ticks after a spawn the moves that reach the server are the ones the
/// player's client sent before it heard of the spawn (the tick's completion reaches
/// the gateway's connection, then the client; the client's input comes back through
/// the gateway and the next tick's batch), and so are dropped without being judged.
pub const SPAWN_INPUT_DELAY_TICKS: u64 = 3;

/// Advance the match by one tick (1/30 s), at match tick `tick`: apply the
/// deaths, end the match if its time is up, judge the moves of the players who
/// are alive (the input of a player who is dead, or has just spawned, is dropped), and spawn the
/// players who are due. The one place where the rules and the step meet.
pub fn play(
    store: &mut impl Store,
    game: &mut impl GameStore,
    map: &MapData,
    rng: &mut Rng,
    tick: u64,
    deaths: &[Death],
    inputs: &[PlayerInput],
) -> Outcome {
    let before = game.game();
    let mut state = before;
    let mut events = Vec::new();
    for death in deaths {
        if state.ending.is_some() {
            events.push(GameEvent::DeathRefused { victim: death.victim, reason: DeathRefusal::GameOver });
        } else {
            apply_death(game, &mut state, tick, *death, &mut events);
        }
    }
    if state.ending.is_none() {
        let limit = state.rules.time_limit_ticks as u64;
        if limit > 0 && tick >= state.started_tick + limit {
            finish(game, &mut state, EndReason::TimeLimit, &mut events);
        }
    }
    if state != before {
        game.set_game(state);
    }

    // only a player who is alive moves (an input for a player the rules do not
    // know goes to the step, which refuses it), and not at once after a spawn: what
    // their client sent before it heard of the spawn is still on its way, from where
    // their body was, and would be refused as a move across the map
    let living: Vec<PlayerInput> = inputs
        .iter()
        .filter(|i| {
            game.contestant(i.player).is_none_or(|c| {
                c.is_alive() && (c.spawned_tick == 0 || tick > c.spawned_tick + SPAWN_INPUT_DELAY_TICKS)
            })
        })
        .copied()
        .collect();
    let moves = step(store, &living, map, rng);

    events.extend(spawn_due(store, game, map, rng, tick));
    Outcome { moves, events }
}

/// Spawn the players who are due at `tick`: those whose respawn timer has
/// run out, and those whose wave it is (see the module's section on waves).
pub fn spawn_due(
    store: &mut impl Store,
    game: &mut impl GameStore,
    map: &MapData,
    rng: &mut Rng,
    tick: u64,
) -> Vec<GameEvent> {
    let mut events = Vec::new();
    let state = game.game();
    if state.ending.is_some() {
        return events;
    }
    let mut due: Vec<Contestant> = game
        .unspawned()
        .into_iter()
        .filter(|c| match c.life {
            Life::Alive => false,
            Life::Dead { due } => due <= tick,
            Life::Waiting { wave } => wave <= tick,
        })
        .collect();
    if due.is_empty() {
        return events;
    }
    // those who have waited a wave first, then by when they were due, then by id
    due.sort_by_key(|c| match c.life {
        Life::Waiting { wave } => (0u8, wave, c.id),
        Life::Dead { due } => (1, due, c.id),
        Life::Alive => (2, 0, c.id),
    });

    let teams = state.rules.teams;
    let mut occupants: Vec<Occupant> = game
        .contestants()
        .iter()
        .filter(|c| c.is_alive())
        .filter_map(|c| store.player(c.id).map(|p| Occupant { position: p.position, team: c.team }))
        .collect();
    let wave = state.is_wave(tick);
    // a team (or, without teams, everyone) with no spot left for one has none for the next
    let (mut none_free, mut none_beside) = ([false; TEAMS as usize], [false; TEAMS as usize]);
    for mut c in due {
        let key = if teams { (c.team % TEAMS) as usize } else { 0 };
        let mut spot = None;
        let mut in_wave = false;
        if matches!(c.life, Life::Dead { .. }) && !none_free[key] {
            spot = spawn::pick(map, teams, c.team, &occupants, rng, false);
            none_free[key] = spot.is_none();
        }
        // (a player who waits is due on the wave's tick: a wave that was late is still one)
        if spot.is_none() && (wave || matches!(c.life, Life::Waiting { .. })) && !none_beside[key] {
            spot = spawn::pick(map, teams, c.team, &occupants, rng, true);
            none_beside[key] = spot.is_none();
            in_wave = spot.is_some();
        }
        match spot {
            Some(spot) => {
                store.set_player(Player::new(c.id, spot.position, spot.yaw, 0.0));
                c.life = Life::Alive;
                c.spawns += 1;
                c.spawn = spot.position;
                c.spawn_yaw = spot.yaw;
                c.spawned_tick = tick;
                game.set_contestant(c);
                occupants.push(Occupant { position: spot.position, team: c.team });
                events.push(GameEvent::Spawned { player: c.id, position: spot.position, yaw: spot.yaw, wave: in_wave });
            }
            None => {
                let wave_at = state.next_wave(tick);
                c.life = Life::Waiting { wave: wave_at };
                game.set_contestant(c);
                events.push(GameEvent::Waiting { player: c.id, wave_at });
            }
        }
    }
    events
}

/// The score of a player, or of their team in a team game.
fn standing(state: &Game, c: &Contestant) -> i32 {
    if state.rules.teams {
        state.team_scores[(c.team % TEAMS) as usize]
    } else {
        c.score
    }
}

fn apply_death(game: &mut impl GameStore, state: &mut Game, tick: u64, death: Death, events: &mut Vec<GameEvent>) {
    let Some(mut victim) = game.contestant(death.victim) else {
        events.push(GameEvent::DeathRefused { victim: death.victim, reason: DeathRefusal::UnknownPlayer });
        return;
    };
    if !victim.is_alive() {
        events.push(GameEvent::DeathRefused { victim: death.victim, reason: DeathRefusal::NotAlive });
        return;
    }
    let rules = state.rules;
    let killer = death.killer.filter(|k| *k != victim.id);
    let mut credit = killer.and_then(|k| game.contestant(k));
    let kind = match (death.killer, &credit) {
        (None, _) => DeathKind::Suicide,
        (Some(k), _) if k == victim.id => DeathKind::Suicide,
        (Some(_), None) => DeathKind::Unclaimed,
        (Some(_), Some(k)) if rules.teams && k.team == victim.team => DeathKind::Betrayal,
        _ => DeathKind::Kill,
    };

    // the score
    let scored = match kind {
        DeathKind::Kill => credit.as_mut().map(|k| (k, 1)),
        DeathKind::Betrayal => credit.as_mut().map(|k| (k, -1)),
        DeathKind::Suicide => Some((&mut victim, -1)),
        DeathKind::Unclaimed => None,
    };
    let mut scorer = None;
    if let Some((player, delta)) = scored {
        player.score += delta;
        if rules.teams {
            state.team_scores[(player.team % TEAMS) as usize] += delta;
        }
        events.push(GameEvent::Scored { player: player.id, delta, score: player.score });
        scorer = Some((player.id, delta));
    }

    // the respawn timer
    let mut timer = victim.penalty as u64 + rules.respawn_ticks as u64;
    if rules.respawn_growth_ticks > 0 {
        let growth = rules.respawn_growth_ticks;
        victim.penalty = (victim.penalty + growth).min(growth * 5);
        if kind == DeathKind::Kill {
            if let Some(k) = credit.as_mut() {
                k.penalty = k.penalty.saturating_sub(growth);
            }
        }
    }
    if kind != DeathKind::Kill {
        timer += rules.suicide_penalty_ticks as u64;
    }
    let timer = timer.max(MIN_RESPAWN_TICKS as u64);
    victim.deaths += 1;
    victim.life = Life::Dead { due: tick + timer };
    events.push(GameEvent::Died { victim: victim.id, killer: death.killer, kind, respawn_at: tick + timer });

    // (the suicide's score was on the victim; the others are the killer's)
    let credited = credit.filter(|k| k.id != victim.id);
    game.set_contestant(victim);
    if let Some(k) = credited {
        game.set_contestant(k);
    }
    if let Some((id, delta)) = scorer {
        if delta > 0 && rules.score_limit > 0 {
            if let Some(c) = game.contestant(id) {
                if standing(state, &c) >= rules.score_limit as i32 {
                    finish(game, state, EndReason::ScoreLimit, events);
                }
            }
        }
    }
}

/// End the match: the winner is the highest score (the highest team's in a
/// team game), nobody if the top is level.
fn finish(game: &impl GameStore, state: &mut Game, reason: EndReason, events: &mut Vec<GameEvent>) {
    let winner = if state.rules.teams {
        match state.team_scores[0].cmp(&state.team_scores[1]) {
            core::cmp::Ordering::Greater => Winner::Team(0),
            core::cmp::Ordering::Less => Winner::Team(1),
            core::cmp::Ordering::Equal => Winner::Nobody,
        }
    } else {
        let mut top: Option<(i32, PlayerId)> = None;
        let mut level = false;
        for c in game.contestants() {
            match top {
                Some((score, _)) if c.score < score => {}
                Some((score, _)) if c.score == score => level = true,
                _ => {
                    top = Some((c.score, c.id));
                    level = false;
                }
            }
        }
        match top {
            Some((_, id)) if !level => Winner::Player(id),
            _ => Winner::Nobody,
        }
    };
    let ending = Ending { reason, winner };
    state.ending = Some(ending);
    events.push(GameEvent::Over(ending));
}
