//! The comparison harness's scenarios, played in the Rust simulation.
//!
//! A scenario (`tools/scenarios/*.scn`, the format in its README) is a script
//! of per-tick controls on a map from a start. The C engine plays it and writes
//! a trace of the player's state (`tools/scenario_harness.py run`); [`trace`]
//! plays it in [`halo_sim::walk`] and writes the same trace, which
//! `tools/scenario_harness.py compare` then compares with the engine's.
//!
//! ```text
//! cargo run --release -p halo-scenario -- tools/scenarios/walk_flat.scn --maps <data root>/maps --out rust.tsv
//! python tools/scenario_harness.py compare engine.tsv rust.tsv --scenario tools/scenarios/walk_flat.scn
//! ```

use halo_sim::math::wrap_angle;
use halo_sim::walk::{walk, Body, Controls};
use halo_sim::MapData;

/// One tick's inputs, as the harness expands them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TickInput {
    pub forward: f32,
    pub strafe: f32,
    pub yaw: f32,
    pub pitch: f32,
    pub jump: bool,
    pub crouch: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Scenario {
    pub name: String,
    /// The multiplayer map's file name without `.map`.
    pub map: String,
    pub start: [f32; 3],
    pub start_yaw: f32,
    /// The inputs of every tick.
    pub inputs: Vec<TickInput>,
}

/// Read a scenario file's text (`tools/scenarios/README.md` has the format).
/// `tolerance` lines are the comparison's, not the simulation's, and are
/// skipped.
pub fn parse(text: &str) -> Result<Scenario, String> {
    let (mut name, mut map, mut start, mut ticks) = (None, None, None, None);
    let mut lines: Vec<(usize, usize, Vec<(String, f32)>)> = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let words: Vec<&str> = line.split('#').next().unwrap_or("").split_whitespace().collect();
        let Some((keyword, rest)) = words.split_first() else { continue };
        let at = |what: &str| format!("line {}: {what}", number + 1);
        let float = |w: &str| w.parse::<f32>().map_err(|_| at(&format!("{w:?} is not a number")));
        match *keyword {
            "scenario" => name = rest.first().map(|s| s.to_string()),
            "map" => map = rest.first().map(|s| s.to_string()),
            "start" => {
                if rest.len() != 5 {
                    return Err(at("start takes a name, x, y, z and yaw"));
                }
                start = Some(([float(rest[1])?, float(rest[2])?, float(rest[3])?], float(rest[4])?));
            }
            "ticks" => ticks = Some(rest.first().and_then(|t| t.parse::<usize>().ok()).ok_or_else(|| at("ticks"))?),
            "tolerance" => {}
            "input" => {
                if rest.len() < 2 {
                    return Err(at("input takes two ticks and keys"));
                }
                let first = rest[0].parse::<usize>().map_err(|_| at("input's first tick"))?;
                let end = rest[1].parse::<usize>().map_err(|_| at("input's end tick"))?;
                let mut keys = Vec::new();
                for word in &rest[2..] {
                    let (key, value) = word.split_once('=').ok_or_else(|| at(&format!("{word:?} is not key=value")))?;
                    keys.push((key.to_string(), float(value)?));
                }
                lines.push((first, end, keys));
            }
            other => return Err(at(&format!("unknown line {other:?}"))),
        }
    }
    let (Some(name), Some(map), Some((start, start_yaw)), Some(ticks)) = (name, map, start, ticks) else {
        return Err("the scenario needs a scenario, map, start and ticks line".into());
    };
    let mut inputs = vec![
        TickInput { forward: 0.0, strafe: 0.0, yaw: start_yaw, pitch: 0.0, jump: false, crouch: false };
        ticks
    ];
    for (first, end, keys) in lines {
        if first > end || end > ticks {
            return Err(format!("input ticks {first} to {end} are outside 0 to {ticks}"));
        }
        for input in &mut inputs[first..end] {
            for (key, value) in &keys {
                match key.as_str() {
                    "forward" => input.forward = *value,
                    "strafe" => input.strafe = *value,
                    "yaw" => input.yaw = *value,
                    "pitch" => input.pitch = *value,
                    "jump" => input.jump = *value != 0.0,
                    "crouch" => input.crouch = *value != 0.0,
                    other => return Err(format!("unknown input key {other:?}")),
                }
            }
        }
    }
    Ok(Scenario { name, map, start, start_yaw, inputs })
}

/// The movement state bits of the trace's last column.
const STATE_AIRBORNE: u32 = 1;

/// Play the scenario on `map` and write its trace (`tools/scenarios/README.md`
/// has the format): row `k` is the state after tick `k`.
///
/// Jumping and crouching are not in the simulation yet; a scenario that asks
/// for them is refused rather than traced wrongly.
pub fn trace(scenario: &Scenario, map: &MapData) -> Result<String, String> {
    if scenario.inputs.iter().any(|i| i.jump || i.crouch) {
        return Err(format!("{}: jumping and crouching are not simulated yet", scenario.name));
    }
    let mut out = format!(
        "# halo-trace 1\n# scenario {}\n# map {}\n# source rust-sim\ntick\tx\ty\tz\tvx\tvy\tvz\tyaw\tpitch\tstate\n",
        scenario.name, scenario.map
    );
    let mut body = Body::at(scenario.start);
    for (tick, input) in scenario.inputs.iter().enumerate() {
        walk(map, &mut body, &Controls { forward: input.forward, strafe: input.strafe, yaw: input.yaw, pitch: input.pitch });
        let v = body.velocity_per_second();
        let state = if body.airborne { STATE_AIRBORNE } else { 0 };
        out.push_str(&format!(
            "{tick}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\t{state}\n",
            body.position[0],
            body.position[1],
            body.position[2],
            v[0],
            v[1],
            v[2],
            wrap_angle(input.yaw),
            input.pitch,
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WALK: &str = "scenario walk\nmap flat\nstart here 1 2 3 0.5\nticks 6\ninput 2 5 forward=1.0 yaw=1\ninput 4 5 forward=0.5 # later wins\ntolerance position 0.25\n";

    #[test]
    fn a_scenario_expands_to_the_inputs_of_every_tick() {
        let s = parse(WALK).unwrap();
        assert_eq!((s.name.as_str(), s.map.as_str(), s.start, s.start_yaw, s.inputs.len()), ("walk", "flat", [1.0, 2.0, 3.0], 0.5, 6));
        let forward: Vec<f32> = s.inputs.iter().map(|i| i.forward).collect();
        assert_eq!(forward, [0.0, 0.0, 1.0, 1.0, 0.5, 0.0]);
        let yaw: Vec<f32> = s.inputs.iter().map(|i| i.yaw).collect();
        assert_eq!(yaw, [0.5, 0.5, 1.0, 1.0, 1.0, 0.5]);
    }

    #[test]
    fn a_scenario_with_something_unreadable_is_refused() {
        assert!(parse("scenario a\nmap b\nstart s 0 0 0 0\nticks 2\ninput 0 3 forward=1\n").is_err());
        assert!(parse("scenario a\nmap b\nticks 2\n").is_err());
        assert!(parse("scenario a\nmap b\nstart s 0 0 0 0\nticks 2\nbogus\n").is_err());
    }

    #[test]
    fn the_trace_has_a_row_for_every_tick_in_the_harness_format() {
        let s = parse("scenario flat_walk\nmap fixture\nstart s 0 0 0 0\nticks 40\ninput 5 40 forward=1.0\n").unwrap();
        let text = trace(&s, &halo_sim::fixtures::flat_floor_map()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "# halo-trace 1");
        assert_eq!(lines[4], "tick\tx\ty\tz\tvx\tvy\tvz\tyaw\tpitch\tstate");
        assert_eq!(lines.len(), 5 + 40);
        let last: Vec<&str> = lines[44].split('\t').collect();
        assert_eq!(last[0], "39");
        assert!(last[1].parse::<f32>().unwrap() > 1.0, "the player walked ahead: {last:?}");
        assert_eq!(last[9], "0");
    }

    #[test]
    fn a_scenario_that_jumps_is_refused_not_traced_wrongly() {
        let s = parse("scenario j\nmap fixture\nstart s 0 0 0 0\nticks 4\ninput 1 2 jump=1\n").unwrap();
        assert!(trace(&s, &halo_sim::fixtures::flat_floor_map()).is_err());
    }
}
