//! A player's walking, asserted on what a player ends up doing: where they
//! are and how fast they move after some ticks of controls, and whether the
//! server's step accepts the positions they report. Nothing here looks inside
//! the movement code.
//!
//! The maps are the small invented ones of `halo_sim::fixtures`, so these run
//! without the game's data; the same movement against Blood Gulch's real
//! collision data is compared with the C engine by the scenario harness
//! (`tools/scenarios`), and `tests/game_data.rs` runs it on every map.

use halo_sim::fixtures::{flat_floor_map, ramp_map, walled_floor_map, RAMP_START_X, WALL_X};
use halo_sim::walk::{walk, Body, Controls, GRAVITY};
use halo_sim::{step, Event, MapData, MemoryStore, Player, PlayerInput, Rng, Store, TICKS_PER_SECOND};

const TICKS: f32 = TICKS_PER_SECOND as f32;

fn controls(forward: f32, strafe: f32, yaw: f32) -> Controls {
    Controls { forward, strafe, yaw, pitch: 0.0 }
}

/// A player put on the ground at `x, y` who has had time to settle.
fn standing(map: &MapData, x: f32, y: f32, z: f32) -> Body {
    let mut body = Body::at([x, y, z]);
    for _ in 0..20 {
        walk(map, &mut body, &Controls::standing(0.0));
    }
    assert!(!body.airborne, "the player settled on the ground at {:?}", body.position);
    body
}

fn run(map: &MapData, body: &mut Body, c: Controls, ticks: u32) {
    for _ in 0..ticks {
        walk(map, body, &c);
    }
}

/// How fast the player moves over the ground, world units a second.
fn speed(body: &Body) -> f32 {
    halo_sim::math::magnitude(&body.velocity) * TICKS
}

fn close(a: f32, b: f32, tolerance: f32) -> bool {
    (a - b).abs() <= tolerance
}

#[test]
fn a_player_standing_on_flat_ground_stays_where_they_are() {
    let map = flat_floor_map();
    let mut body = standing(&map, 3.0, 4.0, 0.0);
    let before = body.position;
    run(&map, &mut body, Controls::standing(0.7), 100);
    assert_eq!(body.position, before);
    assert_eq!(body.velocity, [0.0; 3]);
    assert!(!body.airborne);
}

#[test]
fn pushing_ahead_speeds_the_player_up_by_the_tags_acceleration_to_the_tags_run_speed() {
    let map = flat_floor_map();
    let m = map.movement;
    let mut body = standing(&map, 0.0, 0.0, 0.0);

    walk(&map, &mut body, &controls(1.0, 0.0, 0.0));
    assert!(close(speed(&body), m.run_acceleration, 1e-4), "one tick: {}", speed(&body));
    walk(&map, &mut body, &controls(1.0, 0.0, 0.0));
    assert!(close(speed(&body), 2.0 * m.run_acceleration, 1e-4), "two ticks: {}", speed(&body));

    run(&map, &mut body, controls(1.0, 0.0, 0.0), 60);
    assert!(close(speed(&body), m.run_forward_speed, 1e-4), "at full speed: {}", speed(&body));
    let before = body.position;
    run(&map, &mut body, controls(1.0, 0.0, 0.0), 30);
    // a second at the run speed, along the facing (+x), without leaving the floor
    assert!(close(body.position[0] - before[0], m.run_forward_speed, 1e-3));
    assert!(close(body.position[1], 0.0, 1e-5) && close(body.position[2], 0.0, 1e-5));
    assert!(!body.airborne);
}

#[test]
fn each_direction_has_its_own_tag_speed() {
    let map = flat_floor_map();
    let m = map.movement;
    for (forward, strafe, want) in [
        (-1.0, 0.0, m.run_backward_speed),
        (0.0, 1.0, m.run_sideways_speed),
        (0.0, -1.0, m.run_sideways_speed),
        (0.5, 0.0, 0.5 * m.run_forward_speed),
    ] {
        let mut body = standing(&map, 0.0, 0.0, 0.0);
        run(&map, &mut body, controls(forward, strafe, 0.0), 120);
        assert!(close(speed(&body), want, 1e-3), "throttle ({forward}, {strafe}): {} not {want}", speed(&body));
    }
}

#[test]
fn the_player_moves_along_where_they_face_and_to_their_left_for_a_positive_strafe() {
    let map = flat_floor_map();
    let mut body = standing(&map, 0.0, 0.0, 0.0);
    // facing +y: ahead is +y
    run(&map, &mut body, controls(1.0, 0.0, core::f32::consts::FRAC_PI_2), 60);
    assert!(body.position[1] > 1.0 && body.position[0].abs() < 1e-3, "{:?}", body.position);
    // facing +y, a strafe to the left is -x
    let mut body = standing(&map, 0.0, 0.0, 0.0);
    run(&map, &mut body, controls(0.0, 1.0, core::f32::consts::FRAC_PI_2), 60);
    assert!(body.position[0] < -1.0 && body.position[1].abs() < 1e-3, "{:?}", body.position);
}

#[test]
fn a_throttle_too_small_to_mean_it_moves_no_one() {
    let map = flat_floor_map();
    let mut body = standing(&map, 0.0, 0.0, 0.0);
    run(&map, &mut body, controls(0.05, 0.05, 0.0), 60);
    assert_eq!(body.position, [0.0, 0.0, 0.0]);
}

#[test]
fn letting_go_slows_the_player_to_a_stop() {
    let map = flat_floor_map();
    let mut body = standing(&map, 0.0, 0.0, 0.0);
    run(&map, &mut body, controls(1.0, 0.0, 0.0), 90);
    run(&map, &mut body, Controls::standing(0.0), 90);
    assert!(speed(&body) < 1e-4, "still moving at {}", speed(&body));
}

#[test]
fn a_player_running_at_a_wall_stops_a_radius_from_it_and_stays_there() {
    let map = walled_floor_map();
    let radius = map.movement.collision_radius;
    let mut body = standing(&map, WALL_X - 5.0, 0.0, 0.0);
    run(&map, &mut body, controls(1.0, 0.0, 0.0), 200);
    assert!(close(body.position[0], WALL_X - radius, 1e-3), "stopped at x = {}", body.position[0]);
    assert!(speed(&body) < 1e-4);
    assert!(!body.airborne);
    // nothing at all gets through it
    run(&map, &mut body, controls(1.0, 0.0, 0.0), 100);
    assert!(body.position[0] <= WALL_X - radius + 1e-3);
}

#[test]
fn a_player_running_at_a_wall_at_an_angle_slides_along_it() {
    let map = walled_floor_map();
    let m = map.movement;
    let mut body = standing(&map, WALL_X - 5.0, 0.0, 0.0);
    let angle = 0.6f32;
    run(&map, &mut body, controls(1.0, 0.0, angle), 150);
    // held off the wall, carried along it at the part of the run speed that is along it
    assert!(close(body.position[0], WALL_X - m.collision_radius, 1e-3), "x = {}", body.position[0]);
    assert!(body.position[1] > 3.0, "slid only to y = {}", body.position[1]);
    let along = m.run_forward_speed * angle.sin();
    assert!(close(speed(&body), along, 0.05), "slides at {} not about {along}", speed(&body));
    assert!(body.velocity[0].abs() < 1e-3, "still pushing into the wall: {:?}", body.velocity);
}

#[test]
fn walking_up_a_slope_is_slower_by_the_tags_and_stays_on_the_ground() {
    let rise = 0.36; // about 20 degrees
    let map = ramp_map(rise);
    let m = map.movement;
    let mut body = standing(&map, RAMP_START_X - 5.0, 0.0, 0.0);
    run(&map, &mut body, controls(1.0, 0.0, 0.0), 300);
    assert!(body.position[0] > RAMP_START_X + 3.0, "x = {}", body.position[0]);
    assert!(!body.airborne);
    // on the ramp
    assert!(close(body.position[2], rise * (body.position[0] - RAMP_START_X), 0.05), "z = {}", body.position[2]);
    // the direction of travel along the slope, and the tags' scale for it
    let k = rise / halo_sim::math::sqrt(1.0 + rise * rise);
    let scale = (k - m.uphill_k0) * (m.uphill_velocity_scale - 1.0) / (m.uphill_k1 - m.uphill_k0) + 1.0;
    assert!(
        close(speed(&body), m.run_forward_speed * scale, 0.01),
        "{} not {}",
        speed(&body),
        m.run_forward_speed * scale
    );
    assert!(speed(&body) < m.run_forward_speed);
    assert!(body.velocity[2] > 0.0, "climbing");
}

#[test]
fn walking_down_a_slope_follows_the_ground_down() {
    let map = ramp_map(0.36);
    let mut body = standing(&map, RAMP_START_X + 20.0, 0.0, 0.36 * 20.0);
    run(&map, &mut body, controls(1.0, 0.0, core::f32::consts::PI), 150);
    assert!(body.position[0] < RAMP_START_X + 20.0 - 2.0, "x = {}", body.position[0]);
    assert!(body.velocity[2] < 0.0, "descending");
    assert!(!body.airborne, "a player running down a ramp stays on it");
}

#[test]
fn a_player_with_no_ground_under_them_falls_and_lands() {
    let map = flat_floor_map();
    let mut body = Body::at([0.0, 0.0, 2.0]);
    walk(&map, &mut body, &Controls::standing(0.0));
    walk(&map, &mut body, &Controls::standing(0.0));
    walk(&map, &mut body, &Controls::standing(0.0));
    assert!(body.airborne);
    // gravity adds the same to the fall every tick
    let (v0, z0) = (body.velocity[2], body.position[2]);
    walk(&map, &mut body, &Controls::standing(0.0));
    assert!(close(body.velocity[2], v0 - GRAVITY, 1e-7));
    assert!(body.position[2] < z0);

    let mut fastest = 0.0f32;
    for _ in 0..200 {
        walk(&map, &mut body, &Controls::standing(0.0));
        fastest = fastest.max(body.landing_velocity);
        if !body.airborne {
            break;
        }
    }
    assert!(!body.airborne, "never landed");
    assert!(close(body.position[2], 0.0, 1e-3), "landed at z = {}", body.position[2]);
    // the speed it hit the ground at is what falling damage will need: v = sqrt(2 g h) from 2 units
    let want = halo_sim::math::sqrt(2.0 * GRAVITY * 2.0);
    assert!(close(fastest, want, 0.01), "landed at {fastest} a tick, falling from 2 units is about {want}");
}

#[test]
fn every_position_a_walking_player_reaches_is_accepted_by_the_server() {
    // the wall, the slope and the turns: a long scripted walk, reported tick by tick
    for (map, x, z) in [(walled_floor_map(), WALL_X - 5.0, 0.0), (ramp_map(0.36), RAMP_START_X - 5.0, 0.0)] {
        let mut store = MemoryStore::new();
        store.set_player(Player { id: 1, position: [x, 0.0, z], yaw: 0.0, pitch: 0.0 });
        let mut body = standing(&map, x, 0.0, z);
        let mut rng = Rng::seeded(7);
        let mut yaw = 0.0f32;
        let (mut forward, mut strafe) = (1.0f32, 0.0f32);
        for tick in 0..6000u32 {
            if tick % 45 == 0 {
                // a new heading and throttle every so often
                yaw = (rng.next_f32() - 0.5) * 2.0 * core::f32::consts::PI;
                forward = (rng.next_f32() - 0.3) * 1.4;
                strafe = (rng.next_f32() - 0.5) * 2.0;
            }
            walk(&map, &mut body, &controls(forward.clamp(-1.0, 1.0), strafe, yaw));
            let input = PlayerInput { player: 1, position: body.position, yaw, pitch: 0.0 };
            let events = step(&mut store, &[input], &map, &mut rng);
            assert_eq!(events, [Event::MoveAccepted { player: 1 }], "tick {tick} at {:?}", body.position);
        }
    }
}

#[test]
fn the_same_controls_give_the_same_walk_every_time() {
    let map = ramp_map(0.36);
    let play = || {
        let mut body = standing(&map, RAMP_START_X - 5.0, 0.0, 0.0);
        for tick in 0..400 {
            walk(&map, &mut body, &controls(1.0, if tick % 70 < 20 { 0.5 } else { 0.0 }, 0.3 * (tick / 100) as f32));
        }
        body
    };
    assert_eq!(play(), play());
}
