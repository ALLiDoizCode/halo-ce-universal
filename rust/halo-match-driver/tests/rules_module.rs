//! The game's rules in the match module, against a real local SpacetimeDB: how
//! players spawn when they join, what a death is worth, the waves (the fallback), the end,
//! and what the public tables say of them (`standing`, `game_state`). The rules
//! themselves are tested at the step in `rust/halo-sim/tests/rules.rs`; these
//! check that the module keeps them in its tables and ticks them.
//!
//! They need `HALO_STDB_BIN` (a SpacetimeDB 2.10.x release directory) and
//! skip themselves without it; no game data: the map is a flat floor.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use halo_match_driver::server::{build_module, stdb_bin_dir, Server};
use halo_match_driver::{MatchClient, PlayerClient, StandingRow};
use halo_sim::fixtures::{flat_floor_map, start_at, with_starts};
use halo_sim::rules::Rules;
use halo_sim::TICKS_PER_SECOND;

const WAIT: Duration = Duration::from_secs(20);
const ALIVE: u8 = 0;
const DEAD: u8 = 1;
const WAITING: u8 = 2;

fn wasm() -> &'static PathBuf {
    static WASM: OnceLock<PathBuf> = OnceLock::new();
    WASM.get_or_init(build_module)
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + WAIT;
    while !done() {
        assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A match on a flat floor with `starts` starting locations 10 world units
/// apart, running, with the owner's connection to it.
fn started_match(name: &str, starts: usize, rules: Rules) -> Option<(Server, MatchClient)> {
    let Some(bin) = stdb_bin_dir() else {
        eprintln!("HALO_STDB_BIN is not set: skipping, this test needs a SpacetimeDB 2.10.x release");
        return None;
    };
    let server = Server::start(&bin);
    server.publish(wasm(), name);
    let owner = server.connect(name);
    let starts: Vec<_> = (0..starts).map(|i| start_at(i as f32 * 10.0 - 20.0, 0.0, -1)).collect();
    owner.load_map(with_starts(flat_floor_map(), &starts).to_bytes()).unwrap();
    owner.set_game(&rules).unwrap();
    owner.start();
    Some((server, owner))
}

/// `count` players, each with an identity of their own, seated one after another.
fn seat_players(server: &Server, name: &str, owner: &MatchClient, count: u16) -> Vec<PlayerClient> {
    let clients: Vec<PlayerClient> = (0..count)
        .map(|id| {
            let client = PlayerClient::connect_unsubscribed(&server.uri(), name, &server.new_account().token);
            client.join([id as u8 + 1; 32]).unwrap();
            client
        })
        .collect();
    wait_until("the seats", || owner.seats().len() == count as usize);
    clients
}

fn standing(owner: &MatchClient, id: u16) -> StandingRow {
    owner.standings().remove(&id).unwrap_or_else(|| panic!("player {id} has no standing"))
}

fn quick() -> Rules {
    Rules { suicide_penalty_ticks: 0, wave_ticks: 2 * TICKS_PER_SECOND, ..Rules::slayer() }
}

#[test]
fn a_player_who_joins_is_spawned_at_a_starting_location_by_the_rules() {
    let Some((server, owner)) = started_match("spawn", 4, quick()) else { return };
    let _players = seat_players(&server, "spawn", &owner, 3);
    wait_until("the players' standings", || owner.standings().len() == 3);
    let rows = owner.players();
    assert_eq!(rows.len(), 3, "a player row for each who spawned");
    for id in 0..3u16 {
        let s = standing(&owner, id);
        assert_eq!((s.state, s.score, s.deaths, s.spawns), (ALIVE, 0, 0, 1), "player {id}");
        let row = &rows[&id];
        // at one of the starts: x = -20, -10, 0 or 10, on the floor
        assert!((row.x + 20.0).rem_euclid(10.0) < 0.01, "player {id} at x {}", row.x);
        assert!(row.y.abs() < 0.01 && (row.z - 0.01).abs() < 0.001);
        assert_eq!([s.x, s.y, s.z], [row.x, row.y, row.z], "the standing says where the player spawned");
    }
    // three different starts: they are 10 apart
    let mut xs: Vec<i32> = rows.values().map(|r| r.x.round() as i32).collect();
    xs.sort_unstable();
    xs.dedup();
    assert_eq!(xs.len(), 3);
    wait_until("the count", || owner.marker().unwrap().players == 3);
}

#[test]
fn a_kill_is_credited_and_the_dead_player_respawns_when_their_timer_is_out() {
    let Some((server, owner)) = started_match("kill", 4, quick()) else { return };
    let _players = seat_players(&server, "kill", &owner, 2);
    wait_until("both spawned", || owner.standings().values().filter(|s| s.state == ALIVE).count() == 2);

    owner.report_death(1, Some(0)).unwrap();
    wait_until("the death", || standing(&owner, 1).state == DEAD);
    let (killer, victim) = (standing(&owner, 0), standing(&owner, 1));
    assert_eq!((killer.score, killer.deaths), (1, 0));
    assert_eq!((victim.score, victim.deaths), (0, 1));
    // the timer is three seconds at least; the standing says when
    let now = owner.marker().unwrap().tick;
    assert!(victim.due_tick > now + 60 && victim.due_tick <= now + 100, "due {} at {now}", victim.due_tick);

    let due = victim.due_tick;
    wait_until("the respawn", || standing(&owner, 1).state == ALIVE);
    let victim = standing(&owner, 1);
    assert_eq!(victim.spawns, 2, "a new spawn is counted");
    assert!(owner.marker().unwrap().tick >= due, "not before the timer is out");
    assert_eq!(standing(&owner, 0).score, 1);
}

#[test]
fn a_death_nobody_caused_costs_the_player_a_point() {
    let Some((server, owner)) = started_match("fall", 2, quick()) else { return };
    let _players = seat_players(&server, "fall", &owner, 1);
    wait_until("spawned", || owner.standings().values().all(|s| s.state == ALIVE) && !owner.standings().is_empty());
    owner.report_death(0, None).unwrap();
    wait_until("the death", || standing(&owner, 0).state == DEAD);
    assert_eq!(standing(&owner, 0).score, -1);
}

#[test]
fn only_the_owner_may_set_the_game_or_report_a_death() {
    let Some((server, owner)) = started_match("guards", 2, quick()) else { return };
    let _players = seat_players(&server, "guards", &owner, 1);
    let stranger = MatchClient::connect(&server.uri(), "guards");
    for refused in [stranger.report_death(0, None), stranger.set_game(&Rules::team_slayer()), stranger.begin_game()] {
        assert!(refused.unwrap_err().contains("owner"));
    }
    // a death of a player who is not there
    assert!(owner.report_death(9, None).unwrap_err().contains("not in the match"));
    assert!(owner.set_game(&Rules { wave_ticks: 0, ..Rules::slayer() }).is_err());
}

/// No two of the players in `rows` are within a pill's width of one another.
fn assert_apart(rows: &[[f32; 3]]) {
    for (i, a) in rows.iter().enumerate() {
        for b in &rows[i + 1..] {
            let d = ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
            assert!(d >= 0.4, "two players {d} apart");
        }
    }
}

#[test]
fn when_no_starting_location_is_free_a_player_is_put_beside_one_at_once() {
    let Some((server, owner)) = started_match("beside", 1, quick()) else { return };
    let _players = seat_players(&server, "beside", &owner, 4);
    // the one start holds one player; the others are beside it, not waiting for a wave
    wait_until("all four to be in the world", || {
        owner.standings().len() == 4 && owner.standings().values().all(|s| s.state == ALIVE)
    });
    let rows = owner.players();
    assert_eq!(rows.len(), 4);
    let points: Vec<[f32; 3]> = rows.values().map(|r| [r.x, r.y, r.z]).collect();
    assert_apart(&points);
}

#[test]
fn when_not_even_a_spot_beside_a_start_is_free_a_player_waits_and_is_told_which_wave() {
    // (a wave every six seconds: long enough to see them wait; one start, and more players than
    // the places around it)
    let rules = Rules { wave_ticks: 6 * TICKS_PER_SECOND, ..quick() };
    let Some((server, owner)) = started_match("wave", 1, rules) else { return };
    let _players = seat_players(&server, "wave", &owner, 60);
    wait_until("the standings", || owner.standings().len() == 60);
    let waiting: Vec<StandingRow> = owner.standings().into_values().filter(|s| s.state == WAITING).collect();
    assert!(!waiting.is_empty(), "somebody had no place: {:?}", owner.standings());
    let wave = 6 * TICKS_PER_SECOND as u64;
    for s in &waiting {
        assert_eq!(s.due_tick % wave, 0, "the wave is a multiple of {wave} ticks: {}", s.due_tick);
    }
    // and the rest are in the world, apart from one another
    let alive = owner.standings().values().filter(|s| s.state == ALIVE).count();
    assert_eq!(alive + waiting.len(), 60);
    wait_until("the players' rows", || owner.players().len() == alive);
    let points: Vec<[f32; 3]> = owner.players().values().map(|r| [r.x, r.y, r.z]).collect();
    assert_apart(&points);
}

#[test]
fn the_match_ends_at_the_score_limit_and_the_standings_stay_as_they_were() {
    let rules = Rules { score_limit: 2, ..quick() };
    let Some((server, owner)) = started_match("limit", 4, rules) else { return };
    let _players = seat_players(&server, "limit", &owner, 3);
    wait_until("all spawned", || owner.standings().values().filter(|s| s.state == ALIVE).count() == 3);
    assert_eq!(owner.game().unwrap().ending, 0);

    owner.report_death(1, Some(0)).unwrap();
    wait_until("the first kill", || standing(&owner, 0).score == 1);
    assert_eq!(owner.game().unwrap().ending, 0, "one kill of two");
    owner.report_death(2, Some(0)).unwrap();
    wait_until("the end", || owner.game().unwrap().ending != 0);
    let game = owner.game().unwrap();
    assert_eq!((game.ending, game.winner_kind, game.winner), (1, 1, 0), "a score limit, won by player 0");
    assert_eq!(standing(&owner, 0).score, 2);

    // after the end nothing counts, and the dead stay dead
    owner.report_death(0, Some(1)).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(standing(&owner, 0).score, 2);
    assert_eq!(standing(&owner, 0).state, ALIVE);
    std::thread::sleep(Duration::from_millis(3500));
    assert_eq!(standing(&owner, 2).state, DEAD, "no spawning once the match is over");
}

#[test]
fn the_match_ends_at_the_time_limit() {
    let rules = Rules { score_limit: 0, time_limit_ticks: 3 * TICKS_PER_SECOND, ..quick() };
    let Some((server, owner)) = started_match("time", 2, rules) else { return };
    let _players = seat_players(&server, "time", &owner, 2);
    owner.begin_game().unwrap();
    wait_until("the first kill", || {
        let _ = owner.report_death(1, Some(0));
        standing(&owner, 0).score >= 1
    });
    assert_eq!(owner.game().unwrap().ending, 0);
    wait_until("the end", || owner.game().unwrap().ending != 0);
    let game = owner.game().unwrap();
    assert_eq!(
        (game.ending, game.winner_kind, game.winner),
        (2, 1, 0),
        "a time limit, won by the player with the kill"
    );
}

#[test]
fn team_slayer_scores_for_the_team_and_a_betrayal_costs_it() {
    let rules = Rules { score_limit: 3, ..Rules::team_slayer() };
    let Some((server, owner)) = started_match("teams", 6, Rules { respawn_ticks: 0, ..rules }) else { return };
    let _players = seat_players(&server, "teams", &owner, 4);
    wait_until("all spawned", || owner.standings().values().filter(|s| s.state == ALIVE).count() == 4);
    // the roster alternates: 0 and 2 are red, 1 and 3 blue
    assert_eq!(standing(&owner, 0).team, 0);
    assert_eq!(standing(&owner, 1).team, 1);

    owner.report_death(1, Some(0)).unwrap();
    wait_until("the kill", || owner.game().unwrap().red_score == 1);
    owner.report_death(2, Some(0)).unwrap();
    wait_until("the betrayal", || owner.game().unwrap().red_score == 0);
    assert_eq!(standing(&owner, 0).score, 0);
    assert_eq!(owner.game().unwrap().blue_score, 0);
    assert!(owner.game().unwrap().teams);
}

#[test]
fn beginning_the_game_starts_the_clock_and_clears_the_scores() {
    let Some((server, owner)) = started_match("begin", 4, quick()) else { return };
    let _players = seat_players(&server, "begin", &owner, 2);
    wait_until("both spawned", || owner.standings().values().filter(|s| s.state == ALIVE).count() == 2);
    owner.report_death(1, Some(0)).unwrap();
    wait_until("the kill", || standing(&owner, 0).score == 1);
    let before = owner.game().unwrap().started_tick;
    owner.begin_game().unwrap();
    wait_until("the clock", || owner.game().unwrap().started_tick > before);
    assert_eq!((standing(&owner, 0).score, standing(&owner, 1).deaths), (0, 0));
    assert_eq!(owner.game().unwrap().ending, 0);
}

#[test]
fn a_player_who_leaves_goes_from_the_standings_and_a_dead_one_does_not_hold_a_start() {
    let Some((server, owner)) = started_match("leave", 2, quick()) else { return };
    let players = seat_players(&server, "leave", &owner, 2);
    wait_until("both spawned", || owner.standings().values().filter(|s| s.state == ALIVE).count() == 2);
    players[1].leave().unwrap();
    wait_until("the standing to go", || owner.standings().len() == 1);
    assert!(!owner.players().contains_key(&1));
    wait_until("the count", || owner.marker().unwrap().players == 1);
}
