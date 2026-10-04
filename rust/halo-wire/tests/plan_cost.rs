//! What one tick's planning costs the gateway, in time that does not depend on a socket or a
//! loaded machine's scheduler much: 500 recipients, each with its own planner, planned for the
//! same crowd (499 others, `near` within 10 wu, 177 at 10 to 25 wu, the rest beyond 60 wu).
//!
//!     cargo test --release -p halo-wire --test plan_cost -- --ignored --nocapture

use std::time::Instant;

use halo_wire::{Bounds, Entry, Observer, PackedState, Planner, PlannerConfig, UnitState};

const BOUNDS: Bounds = Bounds { min: [-200.0; 3], max: [200.0; 3] };
const RECIPIENTS: usize = 500;

fn entry(player: u16, position: [f32; 3]) -> Entry {
    let state = UnitState { player, position, velocity: [0.0; 3], yaw: 0.0, pitch: 0.0, tick: 0, flags: 0 };
    Entry { position, packed: PackedState::pack(&state, &BOUNDS) }
}

fn world(near: usize) -> Vec<Entry> {
    let mut radii: Vec<f32> = (0..near).map(|i| 1.0 + (i as f32 * 0.618) % 1.0 * 8.9).collect();
    radii.extend((0..177).map(|i| 10.5 + (i as f32 * 0.618) % 1.0 * 14.0));
    radii.extend((0..499 - near - 177).map(|i| 61.0 + (i as f32 * 0.618) % 1.0 * 40.0));
    let mut world = vec![entry(0, [0.0; 3])];
    for (i, r) in radii.iter().enumerate() {
        let angle = 2.0 + i as f32 * 2.399;
        world.push(entry(i as u16 + 1, [r * angle.cos(), r * angle.sin(), 0.0]));
    }
    world
}

#[test]
#[ignore]
fn planning_one_tick_for_500_recipients() {
    let me = Observer { player: 0, position: [0.0; 3], yaw: 0.0, pitch: 0.0 };
    for near in [69usize, 150] {
        let world = world(near);
        let mut planners: Vec<Planner> =
            (0..RECIPIENTS).map(|_| Planner::new(PlannerConfig::with_budget(90_000))).collect();
        let (warm, measured) = (60u32, 120u32);
        for tick in 0..warm {
            for p in &mut planners {
                p.plan(&me, &world, tick);
            }
        }
        // best of three, to keep a busy machine out of the figure
        let mut best = f64::MAX;
        let mut tick = warm;
        for _ in 0..3 {
            let start = Instant::now();
            for _ in 0..measured {
                for p in &mut planners {
                    std::hint::black_box(p.plan(&me, &world, tick));
                }
                tick += 1;
            }
            best = best.min(start.elapsed().as_secs_f64() * 1e3 / measured as f64);
        }
        println!(
            "{near} near: planning a tick for {RECIPIENTS} recipients {best:.2} ms ({:.1} us each)",
            best * 1e3 / RECIPIENTS as f64
        );
    }
}
