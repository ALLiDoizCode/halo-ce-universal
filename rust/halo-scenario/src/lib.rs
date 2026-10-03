//! The comparison harness's scenarios, played in the Rust simulation.
//!
//! A scenario (`tools/scenarios/*.scn`, the format in its README) is a script
//! of per-tick controls on a map from a start. The C engine plays it and writes
//! a trace of the player's state (`tools/scenario_harness.py run`); [`trace`]
//! plays it in [`halo_sim::walk`] and writes the same trace, which
//! `tools/scenario_harness.py compare` then compares with the engine's.
//!
//! A scenario of firing (a `weapon` or a `target` line) also plays the
//! shooter's weapon ([`halo_sim::weapon`]) and the target's shields and health
//! ([`halo_sim::damage`]): each shot the weapon fires hits the target, `flight`
//! ticks later, on the part of its body the `part` input names (1, the body, unless it says otherwise).
//!
//! ```text
//! cargo run --release -p halo-scenario -- tools/scenarios/walk_flat.scn --maps <data root>/maps --out rust.tsv
//! python tools/scenario_harness.py compare engine.tsv rust.tsv --scenario tools/scenarios/walk_flat.scn
//! ```

use halo_sim::damage::Vitals;
use halo_sim::math::wrap_angle;
use halo_sim::walk::{walk, Body, Controls};
use halo_sim::weapon::Hands;
use halo_sim::{MapData, Rng};

/// One tick's inputs, as the harness expands them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TickInput {
    pub forward: f32,
    pub strafe: f32,
    pub yaw: f32,
    pub pitch: f32,
    pub jump: bool,
    pub crouch: bool,
    /// The trigger is held in.
    pub fire: bool,
    /// The part of the target (an index of its body's materials) a shot fired this tick hits.
    pub part: i16,
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
    /// A scenario of firing: the weapon tag's name (`weapons/pistol/pistol`).
    pub weapon: Option<String>,
    /// ... and the target's place: `x y z yaw`.
    pub target: Option<[f32; 4]>,
    /// How many ticks a shot takes to hit.
    pub flight: usize,
}

impl Scenario {
    pub fn firing(&self) -> bool {
        self.weapon.is_some() || self.target.is_some()
    }
}

/// An `input` line: the first tick, the tick it ends before, and its key=value words.
type InputLine = (usize, usize, Vec<(String, f32)>);

/// Read a scenario file's text (`tools/scenarios/README.md` has the format).
/// `tolerance` lines are the comparison's, not the simulation's, and are
/// skipped.
pub fn parse(text: &str) -> Result<Scenario, String> {
    let (mut name, mut map, mut start, mut ticks) = (None, None, None, None);
    let (mut weapon, mut target, mut flight) = (None, None, 0usize);
    let mut lines: Vec<InputLine> = Vec::new();
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
            "weapon" if rest.is_empty() => return Err(at("weapon takes a weapon tag's name")),
            // (the name may have spaces in it: weapons/assault rifle/assault rifle)
            "weapon" => weapon = Some(rest.join(" ")),
            "target" => {
                if rest.len() != 4 {
                    return Err(at("target takes x, y, z and yaw"));
                }
                target = Some([float(rest[0])?, float(rest[1])?, float(rest[2])?, float(rest[3])?]);
            }
            "flight" => {
                flight = rest.first().and_then(|t| t.parse().ok()).ok_or_else(|| at("flight takes a whole number"))?
            }
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
        TickInput {
            forward: 0.0,
            strafe: 0.0,
            yaw: start_yaw,
            pitch: 0.0,
            jump: false,
            crouch: false,
            fire: false,
            part: 1
        };
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
                    "fire" => input.fire = *value != 0.0,
                    "part" => input.part = *value as i16,
                    other => return Err(format!("unknown input key {other:?}")),
                }
            }
        }
    }
    Ok(Scenario { name, map, start, start_yaw, inputs, weapon, target, flight })
}

/// The movement state bits of the trace's last column.
const STATE_AIRBORNE: u32 = 1;
const STATE_CROUCHING: u32 = 2;

/// Play the scenario on `map` and write its trace (`tools/scenarios/README.md`
/// has the format): row `k` is the state after tick `k`. Each shot is taken
/// to hit the target `flight` ticks later, on the part the `part` input says.
pub fn trace(scenario: &Scenario, map: &MapData) -> Result<String, String> {
    trace_with_hits(scenario, map, None)
}

/// The hits a trace of the C engine saw land on its target: the tick and the
/// part of the target, for [`trace_with_hits`].
pub fn hits_of_trace(text: &str) -> Result<Vec<(usize, i16)>, String> {
    let mut hit_column = None;
    let mut hits = Vec::new();
    for line in text.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
        let words: Vec<&str> = line.split_whitespace().collect();
        match hit_column {
            None => {
                hit_column = Some(
                    words
                        .iter()
                        .position(|w| *w == "hit")
                        .ok_or("the trace has no hit column (it is not of firing)")?,
                )
            }
            Some(at) => {
                let tick = words.first().and_then(|w| w.parse::<usize>().ok()).ok_or("a row with no tick")?;
                let part = words.get(at).and_then(|w| w.parse::<i16>().ok()).ok_or("a row with no hit")?;
                if part >= 0 {
                    hits.push((tick, part));
                }
            }
        }
    }
    Ok(hits)
}

/// ... with the hits a client saw, instead of every shot hitting: the client
/// decides what it hit (its engine's shot goes where its aim and the frame it
/// is fired on put it), and the simulation takes the damage from there. The
/// weapon's rate of fire is still the simulation's own.
pub fn trace_with_hits(
    scenario: &Scenario,
    map: &MapData,
    observed: Option<&[(usize, i16)]>,
) -> Result<String, String> {
    let firing = scenario.firing();
    let mut out = format!(
        "# halo-trace {}\n# scenario {}\n# map {}\n# source rust-sim\ntick\tx\ty\tz\tvx\tvy\tvz\tyaw\tpitch\tstate{}\n",
        if firing { 2 } else { 1 },
        scenario.name,
        scenario.map,
        if firing { "\trounds\ttotal\theat\tshield\tbody\tstun\tdead\thit" } else { "" }
    );
    let weapon = match &scenario.weapon {
        Some(name) => {
            let full = format!("{}.weap", name.replace('/', "\\"));
            Some(
                map.combat
                    .weapons
                    .iter()
                    .find(|w| w.name == full)
                    .ok_or_else(|| format!("the map has no weapon {full}"))?,
            )
        }
        None => None,
    };
    let resistance = &map.combat.resistance;
    let damage = weapon
        .and_then(|w| w.triggers.first())
        .and_then(|t| t.projectile.as_ref())
        .and_then(|p| p.impact_damage.as_ref());
    let mut hands = weapon.map(Hands::new);
    let mut target = Vitals::full(resistance);
    let mut rng = Rng::seeded(1);
    // the hits on their way: (the tick they land on, how many, on what part)
    let mut flying: Vec<(usize, u16, i16)> = Vec::new();

    let mut body = Body::at(scenario.start);
    for (tick, input) in scenario.inputs.iter().enumerate() {
        walk(
            map,
            &mut body,
            &Controls {
                forward: input.forward,
                strafe: input.strafe,
                yaw: input.yaw,
                pitch: input.pitch,
                jump: input.jump,
                crouch: input.crouch,
            },
        );
        let mut hit = -1i32;
        if firing {
            // the engine's order: the weapon fires, the target's shield recharges, and the shots land
            if let (Some(hands), Some(weapon)) = (hands.as_mut(), weapon) {
                let shot = hands.update(weapon, input.fire);
                if shot.fired && observed.is_none() {
                    flying.push((tick + scenario.flight, shot.projectiles.max(1), input.part));
                }
            }
            if scenario.target.is_some() {
                target.tick(resistance);
                if let Some(damage) = damage {
                    let mut landed = Vec::new();
                    match observed {
                        Some(seen) => {
                            landed.extend(seen.iter().filter(|(at, _)| *at == tick).map(|(_, part)| (1, *part)))
                        }
                        None => flying.retain(|(at, count, part)| {
                            if *at == tick {
                                landed.push((*count, *part));
                                false
                            } else {
                                true
                            }
                        }),
                    }
                    for (count, part) in landed {
                        for _ in 0..count {
                            let total = halo_sim::damage::roll(damage, 1.0, 1.0, &mut rng);
                            target.hit(resistance, damage, part, false, total);
                            hit = i32::from(part);
                        }
                    }
                }
            }
        }
        let v = body.velocity_per_second();
        let state = if body.airborne { STATE_AIRBORNE } else { 0 } | if body.crouching { STATE_CROUCHING } else { 0 };
        out.push_str(&format!(
            "{tick}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\t{state}",
            body.position[0],
            body.position[1],
            body.position[2],
            v[0],
            v[1],
            v[2],
            wrap_angle(input.yaw),
            input.pitch,
        ));
        if firing {
            let (loaded, total, heat) = hands.map_or((0, 0, 0.0), |h| (h.rounds_loaded, h.rounds_total, h.heat));
            // (a scenario with no target has none to show: nothing, as the engine's trace has)
            let shown = if scenario.target.is_some() {
                target
            } else {
                Vitals { shield: 0.0, body: 0.0, shield_stun_ticks: 0, flags: 0 }
            };
            out.push_str(&format!(
                "\t{loaded}\t{total}\t{heat:.8}\t{:.8}\t{:.8}\t{}\t{}\t{hit}",
                shown.shield,
                shown.body,
                shown.shield_stun_ticks,
                i32::from(shown.is_dead())
            ));
        }
        out.push('\n');
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
        assert_eq!(
            (s.name.as_str(), s.map.as_str(), s.start, s.start_yaw, s.inputs.len()),
            ("walk", "flat", [1.0, 2.0, 3.0], 0.5, 6)
        );
        let forward: Vec<f32> = s.inputs.iter().map(|i| i.forward).collect();
        assert_eq!(forward, [0.0, 0.0, 1.0, 1.0, 0.5, 0.0]);
        let yaw: Vec<f32> = s.inputs.iter().map(|i| i.yaw).collect();
        assert_eq!(yaw, [0.5, 0.5, 1.0, 1.0, 1.0, 0.5]);
        assert!(!s.firing());
    }

    #[test]
    fn a_scenario_with_something_unreadable_is_refused() {
        assert!(parse("scenario a\nmap b\nstart s 0 0 0 0\nticks 2\ninput 0 3 forward=1\n").is_err());
        assert!(parse("scenario a\nmap b\nticks 2\n").is_err());
        assert!(parse("scenario a\nmap b\nstart s 0 0 0 0\nticks 2\nbogus\n").is_err());
        assert!(parse("scenario a\nmap b\nstart s 0 0 0 0\nticks 2\ntarget 1 2 3\n").is_err());
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
    fn a_scenario_that_jumps_leaves_the_ground_and_comes_back() {
        let s = parse("scenario j\nmap fixture\nstart s 0 0 0 0\nticks 60\ninput 10 11 jump=1\n").unwrap();
        let text = trace(&s, &halo_sim::fixtures::flat_floor_map()).unwrap();
        let states: Vec<&str> = text.lines().skip(5).map(|l| l.split('\t').nth(9).unwrap()).collect();
        assert_eq!(states[9], "0");
        assert_eq!(states[10], "1", "airborne from the jump's tick");
        assert_eq!(states[59], "0", "back on the ground");
    }

    #[test]
    fn a_scenario_that_crouches_is_crouching_from_its_first_tick() {
        let s = parse("scenario c\nmap fixture\nstart s 0 0 0 0\nticks 20\ninput 5 20 crouch=1\n").unwrap();
        let text = trace(&s, &halo_sim::fixtures::flat_floor_map()).unwrap();
        let states: Vec<&str> = text.lines().skip(5).map(|l| l.split('\t').nth(9).unwrap()).collect();
        assert_eq!((states[4], states[5], states[19]), ("0", "2", "2"));
    }

    const SHOOT: &str = "scenario shoot\nmap fixture\nstart s 0 0 0 0\ntarget 3 0 0 3.14\nweapon weapons/pistol/pistol\nflight 1\nticks 120\ninput 10 120 fire=1 part=1\n";

    /// The columns of a firing trace's row, by name.
    fn column(text: &str, tick: usize, name: &str) -> f32 {
        let lines: Vec<&str> = text.lines().collect();
        let at = lines[4].split('\t').position(|c| c == name).unwrap();
        lines[5 + tick].split('\t').nth(at).unwrap().parse().unwrap()
    }

    #[test]
    fn a_scenario_of_firing_has_a_trace_of_the_weapon_and_the_target() {
        let s = parse(SHOOT).unwrap();
        assert!(s.firing());
        assert_eq!(s.flight, 1);
        let text = trace(&s, &halo_sim::fixtures::flat_floor_map()).unwrap();
        assert!(text.starts_with("# halo-trace 2\n"));
        assert!(text.lines().nth(4).unwrap().ends_with("rounds\ttotal\theat\tshield\tbody\tstun\tdead\thit"));
        // the first shot is fired on tick 10 and lands on tick 11
        assert_eq!(column(&text, 9, "rounds"), 12.0);
        assert_eq!(column(&text, 10, "rounds"), 11.0);
        assert_eq!(column(&text, 10, "shield"), 1.0);
        assert_eq!(column(&text, 11, "hit"), 1.0);
        assert!((column(&text, 11, "shield") - (1.0 - 25.0 / 75.0)).abs() < 1e-6);
        assert_eq!(column(&text, 11, "stun"), 180.0);
        assert_eq!(column(&text, 12, "stun"), 179.0);
        assert_eq!(column(&text, 12, "hit"), -1.0);
        // the shots go on every 9 ticks until the target is dead (five hits)
        assert_eq!(column(&text, 119, "dead"), 1.0);
        assert_eq!(column(&text, 40, "dead"), 0.0);
    }

    #[test]
    fn the_hits_a_trace_saw_land_are_the_ones_the_simulation_applies() {
        let s = parse(SHOOT).unwrap();
        let map = halo_sim::fixtures::flat_floor_map();
        // the engine's trace: only the first of the shots hit, on the head's part, a tick later than the scenario's flight
        let engine = "# halo-trace 2\ntick\tx\thit\n10\t0\t-1\n12\t0\t0\n13\t0\t-1\n";
        assert_eq!(hits_of_trace(&engine.replace("10\t0\t-1\n", "0\t0\t-1\n")).unwrap(), [(12, 0)]);
        let text = trace_with_hits(&s, &map, Some(&[(12, 0)])).unwrap();
        // the pistol fires on ticks 10, 19, ... and its second shot hits nothing: one hit, on the head
        assert_eq!(column(&text, 11, "hit"), -1.0);
        assert_eq!(column(&text, 12, "hit"), 0.0);
        assert_eq!(column(&text, 20, "hit"), -1.0);
        assert_eq!(column(&text, 119, "dead"), 0.0, "one hit does not kill");
        assert_eq!(column(&text, 10, "rounds"), 11.0, "the simulation's own weapon fired, whatever hit");
        assert!(hits_of_trace("# halo-trace 1\ntick\tx\n0\t0\n").is_err(), "a trace of no firing has no hits");
    }

    #[test]
    fn a_weapon_the_map_does_not_have_is_an_error() {
        let s = parse("scenario a\nmap b\nstart s 0 0 0 0\nweapon weapons/spoon/spoon\nticks 2\n").unwrap();
        assert!(trace(&s, &halo_sim::fixtures::flat_floor_map()).unwrap_err().contains("spoon"));
    }
}
