//! The game's rules at the step seam: a store, a game, the tick's deaths and
//! inputs go in, and what the players, the scores and the events are comes
//! out. No network, no client.

use halo_map::game_type;
use halo_sim::fixtures::{flat_floor_map, start_at, walled_floor_map, with_starts};
use halo_sim::rules::{
    begin, enter, enter_placed, leave, play, spawn_due, Contestant, Death, DeathKind, DeathRefusal, EndReason,
    GameEvent, GameStore, Life, MemoryGame, Rules, Winner,
};
use halo_sim::spawn::{settle, Start};
use halo_sim::{Event, MapData, MemoryStore, PlayerInput, Rng, Store, TICKS_PER_SECOND};

/// A floor with starts 10 world units apart along x (clear of each other by
/// the engine's rules, which keep enemies 2 apart).
fn floor_with(n: usize) -> MapData {
    let starts: Vec<Start> = (0..n).map(|i| start_at(i as f32 * 10.0 - 40.0, 0.0, -1)).collect();
    with_starts(flat_floor_map(), &starts)
}

struct Match {
    map: MapData,
    store: MemoryStore,
    game: MemoryGame,
    rng: Rng,
    tick: u64,
}

impl Match {
    fn new(map: MapData, rules: Rules) -> Match {
        Match { map, store: MemoryStore::new(), game: MemoryGame::new(rules), rng: Rng::seeded(1), tick: 0 }
    }

    fn join(&mut self, id: u16, team: u8) {
        assert!(enter(&mut self.game, id, team, self.tick));
    }

    /// One tick with these deaths and inputs.
    fn tick(&mut self, deaths: &[Death], inputs: &[PlayerInput]) -> Vec<GameEvent> {
        self.tick += 1;
        play(&mut self.store, &mut self.game, &self.map, &mut self.rng, self.tick, deaths, inputs).events
    }

    fn run(&mut self, ticks: u64) -> Vec<GameEvent> {
        (0..ticks).flat_map(|_| self.tick(&[], &[])).collect()
    }

    fn c(&self, id: u16) -> Contestant {
        self.game.contestant(id).unwrap()
    }

    fn alive(&self, id: u16) -> bool {
        self.c(id).is_alive()
    }
}

fn kill(victim: u16, killer: u16) -> Death {
    Death { victim, killer: Some(killer) }
}

fn spawned(events: &[GameEvent]) -> Vec<u16> {
    events
        .iter()
        .filter_map(|e| match e {
            GameEvent::Spawned { player, .. } => Some(*player),
            _ => None,
        })
        .collect()
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

// ---- spawning

#[test]
fn a_player_who_joins_spawns_at_a_starting_location_of_the_game() {
    // the first start is for capture the flag only, the second for any game
    let mut ctf = start_at(-5.0, 0.0, -1);
    ctf.game_types = [game_type::CTF, 0, 0, 0];
    let any = start_at(5.0, 0.0, -1);
    let mut m = Match::new(with_starts(flat_floor_map(), &[ctf, any]), Rules::slayer());
    m.join(7, 0);
    let events = m.tick(&[], &[]);
    assert_eq!(spawned(&events), [7]);
    assert!(m.alive(7));
    assert_eq!(m.store.player(7).unwrap().position, any.position, "the one start a Slayer game uses");
    assert_eq!(m.c(7).spawn, any.position);
    assert_eq!(m.c(7).spawns, 1);
    assert_eq!(m.c(7).spawned_tick, 1, "the tick it happened on");
}

#[test]
fn the_slayer_starts_are_those_that_list_slayer_or_a_group_with_it() {
    let at = |types: [i16; 4]| Start { game_types: types, ..start_at(0.0, 0.0, -1) };
    assert!(at([game_type::SLAYER, 0, 0, 0]).is_for_slayer());
    assert!(at([0, game_type::CTF, game_type::ALL, 0]).is_for_slayer());
    assert!(at([game_type::ALL_NON_CTF, 0, 0, 0]).is_for_slayer());
    assert!(at([game_type::ALL_NORMAL, 0, 0, 0]).is_for_slayer());
    assert!(!at([game_type::CTF, 0, 0, 0]).is_for_slayer());
    assert!(!at([game_type::RACE, game_type::ODDBALL, 0, 0]).is_for_slayer());
}

#[test]
fn a_team_game_uses_the_starts_whatever_team_the_author_made_them_for() {
    // (on the maps as they ship every start that lists Slayer is team 0's: both teams spawn there)
    let starts = [start_at(-30.0, 0.0, 0), start_at(30.0, 0.0, 0)];
    let mut m = Match::new(with_starts(flat_floor_map(), &starts), Rules::team_slayer());
    m.join(0, 0);
    m.join(1, 1);
    m.run(1);
    assert!(m.alive(0) && m.alive(1));
}

#[test]
fn a_game_without_teams_uses_every_start_whatever_its_team() {
    let starts = [start_at(-30.0, 0.0, 0), start_at(30.0, 0.0, 1)];
    let mut m = Match::new(with_starts(flat_floor_map(), &starts), Rules::slayer());
    for id in 0..2 {
        m.join(id, 0);
    }
    m.run(1);
    let mut xs: Vec<f32> = (0..2).map(|id| m.store.player(id).unwrap().position[0]).collect();
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert_eq!(xs, [-30.0, 30.0]);
}

#[test]
fn players_are_never_put_on_top_of_one_another() {
    // 4 starts for 60 players: waves, and beside the starts
    let mut m = Match::new(floor_with(4), Rules::slayer());
    for id in 0..60 {
        m.join(id, (id % 2) as u8);
    }
    m.run(10 * TICKS_PER_SECOND as u64);
    let placed: Vec<[f32; 3]> =
        (0..60).filter(|id| m.alive(*id)).map(|id| m.store.player(id).unwrap().position).collect();
    assert!(placed.len() > 4, "the waves put more than the starts' worth in: {}", placed.len());
    for (i, a) in placed.iter().enumerate() {
        for b in &placed[i + 1..] {
            assert!(distance(*a, *b) >= 0.4, "two players {} apart: {a:?} {b:?}", distance(*a, *b));
        }
    }
}

#[test]
fn enemies_are_not_spawned_near_each_other_when_a_free_start_is_away() {
    // two starts, one 1 world unit from the player who is there, one far
    let near = start_at(1.0, 0.0, -1);
    let far = start_at(40.0, 0.0, -1);
    let mut m = Match::new(with_starts(flat_floor_map(), &[near, far]), Rules::slayer());
    enter_placed(&mut m.game, 0, 0, [0.0, 0.0, 0.01], 0.0);
    m.store.set_player(halo_sim::Player::new(0, [0.0, 0.0, 0.01], 0.0, 0.0));
    for seed in 0..20 {
        let mut rng = Rng::seeded(seed);
        let mut store = m.store.clone();
        let mut game = m.game.clone();
        enter(&mut game, 1, 1, 0);
        spawn_due(&mut store, &mut game, &m.map, &mut rng, 1);
        assert_eq!(store.player(1).unwrap().position, far.position, "seed {seed}");
    }
}

#[test]
fn in_a_team_game_a_player_is_spawned_where_their_team_is() {
    // two free starts: one beside a teammate, one beside an enemy but 4 away (not forbidden, but worse)
    let by_friend = start_at(-30.0, 0.0, -1);
    let by_enemy = start_at(30.0, 0.0, -1);
    let mut m = Match::new(with_starts(flat_floor_map(), &[by_friend, by_enemy]), Rules::team_slayer());
    for (id, team, x) in [(0u16, 0u8, -33.0f32), (1, 0, -27.0), (2, 1, 34.0)] {
        enter_placed(&mut m.game, id, team, [x, 0.0, 0.01], 0.0);
        m.store.set_player(halo_sim::Player::new(id, [x, 0.0, 0.01], 0.0, 0.0));
    }
    let mut at_friend = 0;
    for seed in 0..40 {
        let mut rng = Rng::seeded(seed);
        let mut store = m.store.clone();
        let mut game = m.game.clone();
        enter(&mut game, 3, 0, 0);
        spawn_due(&mut store, &mut game, &m.map, &mut rng, 1);
        if store.player(3).unwrap().position == by_friend.position {
            at_friend += 1;
        }
    }
    // the friends' bonus makes it the likelier (the choice is the engine's: rating times a random)
    assert!(at_friend > 25, "{at_friend} of 40 at the start by the friends");
}

#[test]
fn spawning_is_the_same_for_the_same_seed() {
    let run = |seed| {
        let mut m = Match::new(floor_with(6), Rules::slayer());
        m.rng = Rng::seeded(seed);
        for id in 0..6 {
            m.join(id, 0);
        }
        m.run(1);
        (0..6).map(|id| m.store.player(id).unwrap().position).collect::<Vec<_>>()
    };
    assert_eq!(run(5), run(5));
    assert_ne!(run(5), run(6), "the choice among starts is the seeded random's");
}

#[test]
fn a_start_inside_a_wall_is_moved_clear_of_it() {
    let map = walled_floor_map();
    let wall = halo_sim::fixtures::WALL_X;
    // the player's pill is 0.2 wide: a start 0.05 from the wall is inside it
    let start = Start { position: [wall - 0.05, 0.0, 0.0], ..start_at(0.0, 0.0, -1) };
    let before = halo_sim::walk::footing(&map, [wall - 0.05, 0.0, 0.01]);
    assert!(before.penetration > halo_sim::PENETRATION_TOLERANCE, "the fixture start is in the wall");
    let settled = settle(&map, start);
    let after = halo_sim::walk::footing(&map, settled.position);
    assert!(after.penetration <= halo_sim::PENETRATION_TOLERANCE, "{after:?}");
    assert!(after.supported);
    assert!(distance(settled.position, start.position) <= 0.65, "moved only a little: {settled:?}");
    // a start that is fine stays where it is (on the ground)
    let fine = settle(&map, start_at(0.0, 0.0, -1));
    assert_eq!(fine.position, [0.0, 0.0, 0.01]);
}

#[test]
fn a_spawned_player_can_be_moved_by_the_step_from_where_they_were_put() {
    let mut m = Match::new(floor_with(3), Rules::slayer());
    m.join(2, 0);
    // (the moves that reach the server just after a spawn are dropped: see `SPAWN_INPUT_DELAY_TICKS`)
    m.run(1 + halo_sim::rules::SPAWN_INPUT_DELAY_TICKS);
    let at = m.store.player(2).unwrap().position;
    let input = PlayerInput { player: 2, position: [at[0] + 0.05, at[1], at[2]], yaw: 0.5, pitch: 0.0, flags: 0 };
    let events = {
        m.tick += 1;
        play(&mut m.store, &mut m.game, &m.map, &mut m.rng, m.tick, &[], &[input]).moves
    };
    assert_eq!(events, [Event::MoveAccepted { player: 2 }]);
}

// ---- waves

#[test]
fn when_no_start_is_free_players_wait_and_are_told_for_which_wave() {
    let mut m = Match::new(floor_with(2), Rules::slayer());
    for id in 0..5 {
        m.join(id, 0);
    }
    let events = m.tick(&[], &[]);
    assert_eq!(spawned(&events).len(), 2, "the two starts are taken, by two players");
    let waiting: Vec<(u16, u64)> = events
        .iter()
        .filter_map(|e| match e {
            GameEvent::Waiting { player, wave_at } => Some((*player, *wave_at)),
            _ => None,
        })
        .collect();
    assert_eq!(waiting.len(), 3);
    let wave = 5 * TICKS_PER_SECOND as u64;
    assert!(waiting.iter().all(|(_, at)| *at == wave), "the next wave is at tick {wave}: {waiting:?}");
    for (id, _) in &waiting {
        assert_eq!(m.c(*id).life, Life::Waiting { wave });
        assert!(!m.alive(*id));
    }

    // nothing happens until the wave
    let events = m.run(wave - m.tick - 1);
    assert!(spawned(&events).is_empty() && events.is_empty());
    // and then they spawn, in the wave, beside the starts
    let events = m.tick(&[], &[]);
    assert_eq!(m.tick, wave);
    let in_wave: Vec<u16> = events
        .iter()
        .filter_map(|e| match e {
            GameEvent::Spawned { player, wave: true, .. } => Some(*player),
            _ => None,
        })
        .collect();
    let mut waited: Vec<u16> = waiting.iter().map(|(id, _)| *id).collect();
    waited.sort_unstable();
    assert_eq!(in_wave, waited);
    assert!((0..5).all(|id| m.alive(id)));
}

#[test]
fn a_wave_that_cannot_place_a_player_leaves_them_for_the_next() {
    // a map with no start for the game: nobody can be placed, ever
    let mut ctf = start_at(0.0, 0.0, -1);
    ctf.game_types = [game_type::CTF, 0, 0, 0];
    let mut m = Match::new(with_starts(flat_floor_map(), &[ctf]), Rules::slayer());
    m.join(1, 0);
    let wave = 5 * TICKS_PER_SECOND as u64;
    let events = m.run(2 * wave + 1);
    assert!(spawned(&events).is_empty());
    assert_eq!(m.c(1).life, Life::Waiting { wave: 3 * wave });
}

#[test]
fn the_players_of_a_wave_are_placed_in_the_order_they_waited() {
    let mut m = Match::new(floor_with(1), Rules::slayer());
    m.join(7, 0);
    m.tick(&[], &[]);
    // the one start is taken: these wait for the wave at tick 150, those who joined first first
    for id in [5u16, 4] {
        m.join(id, 0);
    }
    m.tick(&[], &[]);
    m.join(2, 0);
    m.tick(&[], &[]);
    let wave = 5 * TICKS_PER_SECOND as u64;
    m.run(wave - m.tick - 1);
    let events = m.tick(&[], &[]);
    let order: Vec<u16> = events
        .iter()
        .filter_map(|e| match e {
            GameEvent::Spawned { player, wave: true, .. } => Some(*player),
            _ => None,
        })
        .collect();
    assert_eq!(order, [2, 4, 5], "by the tick they were due: all the same wave, so by id");
}

// ---- deaths and score

#[test]
fn a_kill_is_worth_a_point_to_the_killer_and_a_death_to_the_victim() {
    let mut m = Match::new(floor_with(4), Rules::slayer());
    for id in 0..3 {
        m.join(id, 0);
    }
    m.run(1);
    let events = m.tick(&[kill(1, 0)], &[]);
    assert!(events.contains(&GameEvent::Scored { player: 0, delta: 1, score: 1 }));
    assert!(events.contains(&GameEvent::Died {
        victim: 1,
        killer: Some(0),
        kind: DeathKind::Kill,
        respawn_at: m.tick + 90
    }));
    assert_eq!((m.c(0).score, m.c(0).deaths), (1, 0));
    assert_eq!((m.c(1).score, m.c(1).deaths), (0, 1));
    assert!(!m.alive(1));
    assert!(m.alive(0) && m.alive(2));
}

#[test]
fn a_suicide_and_a_death_nobody_caused_cost_a_point_and_ten_seconds_more() {
    let mut m = Match::new(floor_with(4), Rules::slayer());
    for id in 0..2 {
        m.join(id, 0);
    }
    m.run(1);
    let events = m.tick(&[Death { victim: 0, killer: Some(0) }, Death { victim: 1, killer: None }], &[]);
    assert_eq!((m.c(0).score, m.c(1).score), (-1, -1));
    let respawn = m.tick + 90 + 300 - 90;
    // (the engine's variant: no respawn time, 300 ticks for a suicide, never under 90)
    assert!(events.contains(&GameEvent::Died {
        victim: 0,
        killer: Some(0),
        kind: DeathKind::Suicide,
        respawn_at: respawn
    }));
    assert!(events.contains(&GameEvent::Died {
        victim: 1,
        killer: None,
        kind: DeathKind::Suicide,
        respawn_at: respawn
    }));
}

#[test]
fn a_betrayal_costs_the_killer_a_point_for_them_and_their_team() {
    let mut m = Match::new(floor_with(4), Rules::team_slayer());
    m.join(0, 0);
    m.join(1, 0);
    m.join(2, 1);
    m.run(1);
    let events = m.tick(&[kill(1, 0)], &[]);
    assert!(events.contains(&GameEvent::Scored { player: 0, delta: -1, score: -1 }));
    assert_eq!(m.game.game().team_scores, [-1, 0]);
    assert_eq!(m.c(1).deaths, 1);
    let events = m.tick(&[kill(2, 0)], &[]);
    assert!(events.contains(&GameEvent::Scored { player: 0, delta: 1, score: 0 }));
    assert_eq!(m.game.game().team_scores, [0, 0]);
}

#[test]
fn a_team_game_scores_for_the_team_and_a_game_without_teams_does_not() {
    let mut m = Match::new(floor_with(4), Rules::team_slayer());
    for (id, team) in [(0, 0), (1, 1), (2, 1)] {
        m.join(id, team);
    }
    m.run(1);
    m.tick(&[kill(1, 0), kill(2, 0)], &[]);
    assert_eq!(m.game.game().team_scores, [2, 0]);
    assert_eq!(m.c(0).score, 2);

    let mut m = Match::new(floor_with(4), Rules::slayer());
    for id in 0..2 {
        m.join(id, 0);
    }
    m.run(1);
    m.tick(&[kill(1, 0)], &[]);
    assert_eq!(m.game.game().team_scores, [0, 0], "teams do not count");
}

#[test]
fn a_death_by_a_player_who_has_left_scores_for_nobody() {
    let mut m = Match::new(floor_with(4), Rules::slayer());
    for id in 0..3 {
        m.join(id, 0);
    }
    m.run(1);
    assert!(leave(&mut m.game, 2));
    let events = m.tick(&[kill(1, 2)], &[]);
    assert!(events.iter().any(|e| matches!(e, GameEvent::Died { kind: DeathKind::Unclaimed, .. })));
    assert!(!events.iter().any(|e| matches!(e, GameEvent::Scored { .. })));
    assert_eq!(m.c(1).deaths, 1);
}

#[test]
fn a_death_that_cannot_happen_is_refused_and_does_not_count() {
    let mut m = Match::new(floor_with(4), Rules::slayer());
    for id in 0..2 {
        m.join(id, 0);
    }
    m.run(1);
    let events = m.tick(&[kill(1, 0), kill(1, 0), kill(9, 0)], &[]);
    assert_eq!(m.c(0).score, 1, "the second kill of a dead player is not a point");
    assert_eq!(m.c(1).deaths, 1);
    assert!(events.contains(&GameEvent::DeathRefused { victim: 1, reason: DeathRefusal::NotAlive }));
    assert!(events.contains(&GameEvent::DeathRefused { victim: 9, reason: DeathRefusal::UnknownPlayer }));
}

#[test]
fn the_respawn_timer_is_the_variants_and_never_under_three_seconds() {
    let mut rules = Rules::slayer();
    rules.respawn_ticks = 0;
    let mut m = Match::new(floor_with(4), rules);
    m.join(0, 0);
    m.join(1, 0);
    m.run(1);
    m.tick(&[kill(1, 0)], &[]);
    assert_eq!(m.c(1).life, Life::Dead { due: m.tick + 90 });

    let mut m = Match::new(floor_with(4), Rules::team_slayer());
    m.join(0, 0);
    m.join(1, 1);
    m.run(1);
    m.tick(&[kill(1, 0)], &[]);
    assert_eq!(m.c(1).life, Life::Dead { due: m.tick + 300 }, "team slayer: ten seconds");
}

#[test]
fn a_respawn_time_growth_makes_a_player_who_keeps_dying_wait_longer_and_a_killer_less() {
    let mut rules = Rules::slayer();
    rules.respawn_ticks = 100;
    rules.respawn_growth_ticks = 30;
    let mut m = Match::new(floor_with(4), rules);
    m.join(0, 0);
    m.join(1, 0);
    m.run(1);
    m.tick(&[kill(1, 0)], &[]);
    // the first death takes the variant's time; the penalty it earned is for the next
    assert_eq!(m.c(1).life, Life::Dead { due: m.tick + 100 });
    assert_eq!(m.c(1).penalty, 30);
    m.run(100);
    assert!(m.alive(1));
    m.tick(&[kill(1, 0)], &[]);
    assert_eq!(m.c(1).life, Life::Dead { due: m.tick + 130 });
    assert_eq!(m.c(1).penalty, 60);
    // (a killer has none to take off)
    assert_eq!(m.c(0).penalty, 0);
}

#[test]
fn a_dead_player_spawns_when_their_timer_runs_out() {
    let mut m = Match::new(floor_with(4), Rules::slayer());
    m.join(0, 0);
    m.join(1, 0);
    m.run(1);
    let died_at = m.tick + 1;
    m.tick(&[kill(1, 0)], &[]);
    assert_eq!(m.c(1).spawns, 1);
    let events = m.run(89);
    assert!(!spawned(&events).contains(&1), "not before the timer is out");
    assert!(!m.alive(1));
    let events = m.tick(&[], &[]);
    assert_eq!(m.tick, died_at + 90);
    assert!(spawned(&events).contains(&1));
    assert!(m.alive(1));
    assert_eq!(m.c(1).spawns, 2, "a new spawn is a new count, which tells the client");
}

#[test]
fn a_dead_player_does_not_move() {
    let mut m = Match::new(floor_with(4), Rules::slayer());
    m.join(0, 0);
    m.join(1, 0);
    m.run(1);
    let at = m.store.player(1).unwrap().position;
    m.tick(&[kill(1, 0)], &[]);
    m.tick += 1;
    let input = PlayerInput { player: 1, position: [at[0] + 0.05, at[1], at[2]], yaw: 1.0, pitch: 0.0, flags: 0 };
    let outcome = play(&mut m.store, &mut m.game, &m.map, &mut m.rng, m.tick, &[], &[input]);
    assert!(outcome.moves.is_empty(), "the input of a dead player is dropped: {:?}", outcome.moves);
    assert_eq!(m.store.player(1).unwrap().position, at);
}

#[test]
fn a_move_that_was_on_its_way_when_the_player_spawned_is_dropped_not_refused() {
    // the client sends where its unit is (the body, once dead) until it hears of the spawn: the
    // moves it sent in that time reach the server after the player has been put at the start
    let mut m = Match::new(floor_with(4), Rules::slayer());
    m.join(0, 0);
    m.join(1, 0);
    m.run(1);
    let body = m.store.player(1).unwrap().position;
    m.tick(&[kill(1, 0)], &[]);
    m.run(89);
    let events = m.tick(&[], &[]);
    assert!(spawned(&events).contains(&1));
    let start = m.store.player(1).unwrap().position;
    // (the body lay somewhere else on the floor than the start the player is given)
    let body = [body[0] + 60.0, body[1], body[2]];
    assert!(distance(start, body) > 5.0, "the start is not where the body lay");
    let stale = PlayerInput { player: 1, position: body, yaw: 0.0, pitch: 0.0, flags: 0 };
    for _ in 0..halo_sim::rules::SPAWN_INPUT_DELAY_TICKS {
        m.tick += 1;
        let outcome = play(&mut m.store, &mut m.game, &m.map, &mut m.rng, m.tick, &[], &[stale]);
        assert!(outcome.moves.is_empty(), "dropped, not refused: {:?}", outcome.moves);
        assert_eq!(m.store.player(1).unwrap().position, start);
    }
    // ... and the first move from the start after that is judged as any other
    m.tick += 1;
    let from_start =
        PlayerInput { player: 1, position: [start[0] + 0.05, start[1], start[2]], yaw: 0.0, pitch: 0.0, flags: 0 };
    let outcome = play(&mut m.store, &mut m.game, &m.map, &mut m.rng, m.tick, &[], &[from_start]);
    assert!(outcome.moves.iter().all(|e| matches!(e, Event::MoveAccepted { .. })), "{:?}", outcome.moves);
    assert_eq!(m.store.player(1).unwrap().position, [start[0] + 0.05, start[1], start[2]]);
}

// ---- the end

#[test]
fn the_match_ends_at_the_score_limit_and_the_one_who_has_it_wins() {
    let mut rules = Rules::slayer();
    rules.score_limit = 3;
    rules.respawn_ticks = 0;
    let mut m = Match::new(floor_with(4), rules);
    m.join(0, 0);
    m.join(1, 0);
    m.run(1);
    for round in 0..3 {
        assert!(m.game.game().ending.is_none(), "not over after {round} kills");
        let events = m.tick(&[kill(1, 0)], &[]);
        if round < 2 {
            assert!(!events.iter().any(|e| matches!(e, GameEvent::Over(_))));
            m.run(90);
        } else {
            let over = events.iter().find_map(|e| match e {
                GameEvent::Over(ending) => Some(*ending),
                _ => None,
            });
            let over = over.expect("the third kill ends it");
            assert_eq!(over.reason, EndReason::ScoreLimit);
            assert_eq!(over.winner, Winner::Player(0));
        }
    }
    assert_eq!(m.game.game().ending.map(|e| e.winner), Some(Winner::Player(0)));
}

#[test]
fn nothing_counts_after_the_end_and_nobody_spawns() {
    let mut rules = Rules::slayer();
    rules.score_limit = 1;
    let mut m = Match::new(floor_with(4), rules);
    for id in 0..3 {
        m.join(id, 0);
    }
    m.run(1);
    m.tick(&[kill(1, 0)], &[]);
    assert!(m.game.game().ending.is_some());
    let events = m.tick(&[kill(2, 0)], &[]);
    assert_eq!(events, [GameEvent::DeathRefused { victim: 2, reason: DeathRefusal::GameOver }]);
    assert_eq!(m.c(0).score, 1);
    // the dead stay dead
    let events = m.run(200);
    assert!(spawned(&events).is_empty());
    assert!(!m.alive(1));
    // and a player who joins now does not spawn either
    m.join(9, 0);
    assert!(spawned(&m.run(10)).is_empty());
}

#[test]
fn in_a_team_game_the_team_score_ends_the_match_and_the_team_wins() {
    let mut rules = Rules::team_slayer();
    rules.score_limit = 2;
    rules.respawn_ticks = 0;
    let mut m = Match::new(floor_with(6), rules);
    for (id, team) in [(0, 0), (1, 0), (2, 1), (3, 1)] {
        m.join(id, team);
    }
    m.run(1);
    // one kill each side: nobody is over
    m.tick(&[kill(2, 0), kill(0, 3)], &[]);
    assert!(m.game.game().ending.is_none());
    assert_eq!(m.game.game().team_scores, [1, 1]);
    // the second of the blue team's: a different player of the team scores, and the team has it
    let events = m.tick(&[kill(1, 2)], &[]);
    assert!(events.iter().any(|e| matches!(e, GameEvent::Over(_))), "{events:?}");
    let ending = m.game.game().ending.unwrap();
    assert_eq!(ending.reason, EndReason::ScoreLimit);
    assert_eq!(ending.winner, Winner::Team(1));
}

#[test]
fn the_match_ends_at_the_time_limit_and_the_top_score_wins() {
    let mut rules = Rules::slayer();
    rules.score_limit = 0;
    rules.time_limit_ticks = 60 * TICKS_PER_SECOND;
    let mut m = Match::new(floor_with(4), rules);
    m.join(0, 0);
    m.join(1, 0);
    m.run(1);
    m.tick(&[kill(1, 0)], &[]);
    let events = m.run(60 * TICKS_PER_SECOND as u64 - 3);
    assert!(m.game.game().ending.is_none(), "not before the time");
    assert!(!events.iter().any(|e| matches!(e, GameEvent::Over(_))));
    let events = m.tick(&[], &[]);
    assert_eq!(m.tick, 60 * TICKS_PER_SECOND as u64);
    assert_eq!(
        events,
        [GameEvent::Over(halo_sim::rules::Ending { reason: EndReason::TimeLimit, winner: Winner::Player(0) })]
    );
}

#[test]
fn a_level_score_at_the_time_limit_has_no_winner() {
    let mut rules = Rules::team_slayer();
    rules.time_limit_ticks = 100;
    let mut m = Match::new(floor_with(4), rules);
    m.join(0, 0);
    m.join(1, 1);
    m.run(1);
    m.run(100);
    assert_eq!(m.game.game().ending.map(|e| (e.reason, e.winner)), Some((EndReason::TimeLimit, Winner::Nobody)));
}

#[test]
fn the_clock_starts_when_the_match_begins() {
    let mut rules = Rules::slayer();
    rules.time_limit_ticks = 100;
    let mut m = Match::new(floor_with(4), rules);
    m.join(0, 0);
    m.run(50);
    m.game.set_game(halo_sim::rules::Game::new(rules, 0));
    begin(&mut m.game, m.tick);
    m.run(99);
    assert!(m.game.game().ending.is_none());
    m.run(1);
    assert!(m.game.game().ending.is_some());
    // beginning again, with scores in
    begin(&mut m.game, m.tick);
    assert!(m.game.game().ending.is_none());
}

// ---- placed players

#[test]
fn a_player_placed_by_the_caller_is_alive_and_can_die_and_respawn() {
    let mut m = Match::new(floor_with(3), Rules::slayer());
    m.store.set_player(halo_sim::Player::new(4, [1.0, 2.0, 0.01], 0.0, 0.0));
    assert!(enter_placed(&mut m.game, 4, 1, [1.0, 2.0, 0.01], 0.0));
    assert!(!enter_placed(&mut m.game, 4, 1, [1.0, 2.0, 0.01], 0.0), "once");
    assert!(m.alive(4));
    m.join(5, 0);
    m.run(1);
    m.tick(&[kill(4, 5)], &[]);
    assert!(!m.alive(4));
    m.run(90);
    assert!(m.alive(4));
}

#[test]
fn players_who_join_one_at_a_time_before_the_first_tick_wait_for_the_wave() {
    let mut m = Match::new(floor_with(1), Rules::slayer());
    for id in 0..4 {
        m.join(id, (id % 2) as u8);
        let events = spawn_due(&mut m.store, &mut m.game, &m.map, &mut m.rng, 0);
        assert_eq!(spawned(&events).len(), (id == 0) as usize, "player {id}");
    }
    assert_eq!(m.c(1).life, Life::Waiting { wave: 150 });
}

#[test]
fn beginning_the_game_again_moves_the_waves_a_player_waits_for_onto_the_new_clock() {
    let mut m = Match::new(floor_with(1), Rules::slayer());
    m.join(0, 0);
    m.join(1, 0);
    m.run(3);
    assert_eq!(m.c(1).life, Life::Waiting { wave: 150 });
    begin(&mut m.game, m.tick);
    assert_eq!(m.c(1).life, Life::Waiting { wave: m.tick + 150 });
}
