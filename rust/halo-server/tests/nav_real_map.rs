//! The navigation grid against the real Blood Gulch: that it joins the two
//! bases, and that a walker who follows its headings, on the movement the
//! server judges, gets from one base to the other.
//!
//! Needs the game's own maps (`HALO_MAP_DIR`, a folder with `bloodgulch.map`),
//! and skips itself without them.

use std::path::PathBuf;
use std::sync::Arc;

use halo_match_driver::walkers::Walkers;
use halo_server::loadgen::{Contact, Gunner};
use halo_server::nav::{Nav, Scratch, DEFAULT_CELL};
use halo_sim::{MapData, Store, TICKS_PER_SECOND};

fn blood_gulch() -> Option<MapData> {
    let Some(dir) = std::env::var_os("HALO_MAP_DIR").map(PathBuf::from) else {
        eprintln!("HALO_MAP_DIR is not set: skipping, this test needs the game's own maps");
        return None;
    };
    let map = halo_map::HaloMap::from_path(dir.join("bloodgulch.map")).expect("bloodgulch.map loads");
    Some(MapData::from(map))
}

/// The Slayer starting locations of each base: the two ends of the map's longest axis.
fn bases(map: &MapData) -> ([f32; 3], [f32; 3]) {
    let starts: Vec<[f32; 3]> = map.starts.iter().map(|s| s.position).collect();
    let by_y = |a: &&[f32; 3], b: &&[f32; 3]| a[1].total_cmp(&b[1]);
    // (the red base is at the far end of -y, the blue at the near end)
    (*starts.iter().min_by(by_y).unwrap(), *starts.iter().max_by(by_y).unwrap())
}

#[test]
fn the_grid_of_blood_gulch_joins_the_two_bases() {
    let Some(map) = blood_gulch() else { return };
    let nav = Nav::from_map(&map, DEFAULT_CELL);
    assert!(nav.len() > 1000, "a grid of the map: {} nodes", nav.len());
    let (red, blue) = bases(&map);
    let mut scratch = Scratch::default();
    assert!(nav.connected(red, blue), "red and blue are on one stretch of ground");
    let route = nav.route(&mut scratch, red, blue).expect("a path from the red base to the blue");
    assert!(route.len() > 100, "a long way: {} nodes", route.len());
    // every spawn point of either team is on the grid and joined to the others
    for start in map.starts.iter().filter(|s| s.team == 0 || s.team == 1) {
        assert!(nav.connected(red, start.position), "{:?} is cut off", start.position);
    }
}

#[test]
fn a_hunter_walking_the_server_s_movement_gets_from_one_base_to_the_other() {
    let Some(map) = blood_gulch() else { return };
    let nav = Arc::new(Nav::from_map(&map, DEFAULT_CELL));
    let (red, blue) = bases(&map);
    let (mut walkers, _) = Walkers::new(map, &[red], 1, 3);
    // 0 is the walker; 1 is an enemy who stays in their base, far outside the range of a shot
    let mut gunner = Gunner::new(2, 25.0, 0.5, true, 5).with_hunt_range(1000.0).with_nav(nav);
    let mut arrived = None;
    let mut position = walkers.mirror.player(0).unwrap().position;
    for tick in 0..(900 * TICKS_PER_SECOND as u64) {
        let contacts =
            [Some(Contact { position, team: 0, alive: true }), Some(Contact { position: blue, team: 1, alive: true })];
        for (player, heading) in gunner.hunt(tick, &contacts) {
            walkers.set_course(player, 1.0, heading);
        }
        let inputs = walkers.next_inputs();
        walkers.apply(&inputs, tick + 1);
        position = walkers.mirror.player(0).unwrap().position;
        if (position[0] - blue[0]).hypot(position[1] - blue[1]) < 3.0 {
            arrived = Some(tick / TICKS_PER_SECOND as u64);
            break;
        }
    }
    let seconds = arrived.unwrap_or_else(|| panic!("never arrived: ended at {position:?}, the target is {blue:?}"));
    eprintln!("walked from {red:?} to {blue:?} in {seconds} s");
}

/// A crowd of 500 walkers headed for the other base, told where they are by a server that shows
/// each a tick behind the input just sent (as the load program has it from the server's tables):
/// found standing still, in a real run, 360 of 500 for minutes, as each was put back at rest at the
/// server's position every tick and so never got up to the speed of a walk.
#[test]
fn a_crowd_told_where_it_is_a_tick_late_gets_to_the_other_base() {
    let Some(map) = blood_gulch() else { return };
    let nav = Nav::from_map(&map, DEFAULT_CELL);
    let starts: Vec<[f32; 3]> = map.starts.iter().map(|s| s.position).collect();
    let (red, blue) = bases(&map);
    let players = 500u16;
    let (mut walkers, _) = Walkers::new(map.clone(), &starts, players, 7);
    let goal = |id: u16| if id.is_multiple_of(2) { blue } else { red };
    let position = |w: &Walkers, id: u16| w.mirror.player(id).expect("a player").position;
    let mut sent: Vec<Vec<[f32; 3]>> = (0..players).map(|id| vec![position(&walkers, id); 2]).collect();
    let mut scratch = Scratch::default();
    let ticks = 90 * TICKS_PER_SECOND as u64;
    let mut before_last_10_s = vec![[0.0f32; 3]; players as usize];
    for tick in 1..=ticks {
        let inputs = walkers.next_inputs();
        let rows: Vec<halo_match_driver::PlayerRow> = inputs
            .iter()
            .map(|input| {
                let history = &mut sent[input.player as usize];
                history.push(input.position);
                let shown = history[history.len() - 2];
                halo_match_driver::PlayerRow {
                    id: input.player,
                    x: shown[0],
                    y: shown[1],
                    z: shown[2],
                    yaw: 0.0,
                    pitch: 0.0,
                    updated_tick: tick,
                    rejected_moves: 0,
                    last_reject: 0,
                    last_reject_tick: 0,
                    flags: 0,
                    air_ticks: 0,
                    air_z: 0.0,
                    free_ticks: 0,
                    free_z: 0.0,
                }
            })
            .collect();
        walkers.sync_with_server(rows.iter());
        if tick.is_multiple_of(TICKS_PER_SECOND as u64) {
            for input in &inputs {
                if let Some(heading) = nav.heading(&mut scratch, input.position, goal(input.player)) {
                    walkers.set_course(input.player, 1.0, heading);
                }
            }
        }
        if tick == ticks - 10 * TICKS_PER_SECOND as u64 {
            for (id, history) in sent.iter().enumerate() {
                before_last_10_s[id] = *history.last().unwrap();
            }
        }
    }
    let (mut arrived, mut still) = (0, 0);
    for id in 0..players {
        let at = *sent[id as usize].last().unwrap();
        let (to, was) = (goal(id), before_last_10_s[id as usize]);
        if (at[0] - to[0]).hypot(at[1] - to[1]) < 12.0 {
            arrived += 1;
        } else if (at[0] - was[0]).hypot(at[1] - was[1]) < 2.0 {
            still += 1;
        }
    }
    eprintln!("{arrived} of {players} arrived, {still} still on the way");
    assert!(arrived > 300, "only {arrived} got there");
    assert!(still <= 25, "{still} of {players} stand still on the way");
}
