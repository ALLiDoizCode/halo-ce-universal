//! The step's behaviour on a player's reported movement, asserted on the
//! events and state after a step and nothing else.

use halo_sim::fixtures::flat_floor_map;
use halo_sim::{
    step, Event, MapData, MemoryStore, Player, PlayerInput, RejectReason, Rng, Store, MAX_MOVE_SPEED, TICKS_PER_SECOND,
};

const MAX_STEP: f32 = MAX_MOVE_SPEED / TICKS_PER_SECOND as f32;

fn player_at(position: [f32; 3]) -> MemoryStore {
    let mut store = MemoryStore::new();
    store.set_player(Player { id: 7, position, yaw: 1.0, pitch: 0.5 });
    store
}

fn report(store: &mut MemoryStore, map: &MapData, position: [f32; 3]) -> Event {
    let input = PlayerInput { player: 7, position, yaw: 2.0, pitch: -0.25 };
    let events = step(store, &[input], map, &mut Rng::seeded(0));
    assert_eq!(events.len(), 1);
    events[0]
}

fn rejected(reason: RejectReason) -> Event {
    Event::MoveRejected { player: 7, reason }
}

#[test]
fn a_valid_move_is_accepted_and_moves_the_player() {
    let map = flat_floor_map();
    let mut store = player_at([0.0, 0.0, 0.0]);
    let event = report(&mut store, &map, [MAX_STEP * 0.9, 0.0, 0.0]);
    assert_eq!(event, Event::MoveAccepted { player: 7 });
    let p = store.player(7).unwrap();
    assert_eq!(p.position, [MAX_STEP * 0.9, 0.0, 0.0]);
    assert_eq!((p.yaw, p.pitch), (2.0, -0.25));
}

#[test]
fn staying_put_is_valid() {
    let map = flat_floor_map();
    let mut store = player_at([3.0, -4.0, 0.0]);
    assert_eq!(report(&mut store, &map, [3.0, -4.0, 0.0]), Event::MoveAccepted { player: 7 });
}

#[test]
fn a_move_faster_than_the_speed_bound_is_rejected_and_the_player_stays() {
    let map = flat_floor_map();
    let mut store = player_at([0.0, 0.0, 0.0]);
    let before = store.player(7).unwrap();
    assert_eq!(report(&mut store, &map, [MAX_STEP * 1.1, 0.0, 0.0]), rejected(RejectReason::TooFast));
    assert_eq!(store.player(7).unwrap(), before);
}

#[test]
fn the_speed_bound_counts_every_axis() {
    let map = flat_floor_map();
    let mut store = player_at([0.0, 0.0, 0.0]);
    // each axis alone is within the bound, the diagonal is not
    let each = MAX_STEP * 0.8;
    assert_eq!(report(&mut store, &map, [each, each, 0.0]), rejected(RejectReason::TooFast));
}

#[test]
fn a_move_through_the_floor_is_rejected_and_the_player_stays() {
    let map = flat_floor_map();
    let mut store = player_at([0.0, 0.0, 0.0]);
    let before = store.player(7).unwrap();
    assert_eq!(report(&mut store, &map, [0.0, 0.0, -0.1]), rejected(RejectReason::ThroughSurface));
    assert_eq!(store.player(7).unwrap(), before);
}

#[test]
fn a_move_that_ends_in_the_air_is_rejected_and_the_player_stays() {
    let map = flat_floor_map();
    let mut store = player_at([0.0, 0.0, 0.0]);
    let before = store.player(7).unwrap();
    assert_eq!(report(&mut store, &map, [0.0, 0.0, 0.1]), rejected(RejectReason::OffGround));
    assert_eq!(store.player(7).unwrap(), before);
}

#[test]
fn reported_numbers_that_are_not_finite_are_rejected() {
    let map = flat_floor_map();
    let mut store = player_at([0.0, 0.0, 0.0]);
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert_eq!(report(&mut store, &map, [bad, 0.0, 0.0]), rejected(RejectReason::NotFinite));
    }
    let input = PlayerInput { player: 7, position: [0.0; 3], yaw: f32::NAN, pitch: 0.0 };
    let events = step(&mut store, &[input], &map, &mut Rng::seeded(0));
    assert_eq!(events, [rejected(RejectReason::NotFinite)]);
    assert_eq!(store.player(7).unwrap().yaw, 1.0);
}

#[test]
fn an_input_for_a_player_who_does_not_exist_is_rejected() {
    let map = flat_floor_map();
    let mut store = MemoryStore::new();
    let input = PlayerInput { player: 9, position: [0.0; 3], yaw: 0.0, pitch: 0.0 };
    let events = step(&mut store, &[input], &map, &mut Rng::seeded(0));
    assert_eq!(events, [Event::MoveRejected { player: 9, reason: RejectReason::UnknownPlayer }]);
    assert!(store.player_ids().is_empty());
}

#[test]
fn each_player_in_a_batch_is_judged_alone_and_events_follow_input_order() {
    let map = flat_floor_map();
    let mut store = MemoryStore::new();
    for id in [1, 2, 3] {
        store.set_player(Player { id, position: [id as f32, 0.0, 0.0], yaw: 0.0, pitch: 0.0 });
    }
    let input = |player, x| PlayerInput { player, position: [x, 0.0, 0.0], yaw: 0.0, pitch: 0.0 };
    let events = step(&mut store, &[input(3, 3.05), input(1, 99.0), input(2, 2.05)], &map, &mut Rng::seeded(0));
    assert_eq!(
        events,
        [
            Event::MoveAccepted { player: 3 },
            Event::MoveRejected { player: 1, reason: RejectReason::TooFast },
            Event::MoveAccepted { player: 2 },
        ]
    );
    assert_eq!(store.player(1).unwrap().position, [1.0, 0.0, 0.0]);
    assert_eq!(store.player(2).unwrap().position, [2.05, 0.0, 0.0]);
    assert_eq!(store.player(3).unwrap().position, [3.05, 0.0, 0.0]);
}

#[test]
fn a_player_can_walk_across_the_floor_one_tick_at_a_time() {
    let map = flat_floor_map();
    let mut store = player_at([-40.0, 0.0, 0.0]);
    let mut x = -40.0;
    for _ in 0..600 {
        x += MAX_STEP * 0.5;
        assert_eq!(report(&mut store, &map, [x, 0.0, 0.0]), Event::MoveAccepted { player: 7 });
    }
    assert_eq!(store.player(7).unwrap().position, [x, 0.0, 0.0]);
}

#[test]
fn the_same_inputs_give_the_same_state() {
    let map = flat_floor_map();
    let run = || {
        let mut store = player_at([0.0, 0.0, 0.0]);
        let mut rng = Rng::seeded(42);
        let mut events = Vec::new();
        for _ in 0..200 {
            let (dx, dy) = ((rng.next_f32() - 0.5) * 0.3, (rng.next_f32() - 0.5) * 0.3);
            let p = store.player(7).unwrap().position;
            let input = PlayerInput { player: 7, position: [p[0] + dx, p[1] + dy, 0.0], yaw: 0.0, pitch: 0.0 };
            events.extend(step(&mut store, &[input], &map, &mut rng));
        }
        (halo_sim::snapshot(&store), events)
    };
    assert_eq!(run(), run());
}
