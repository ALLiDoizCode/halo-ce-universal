# Comparison scenarios

Each feature of the large-scale mode is accepted when the Rust simulation, run
on a scenario, gives a trace that matches the C engine's within stated
tolerances. This folder holds the scenarios; `tools/scenario_harness.py` runs
the C engine on them and compares traces. The engine side is
`port/linux/game/scenario_harness.c`.

## Run a scenario in the C engine

You need the Linux build (`python configure.py && ninja linux`) and your own
game data (the folder that contains `maps/`).

```
python tools/scenario_harness.py run tools/scenarios/walk_flat.scn walk_flat.tsv --data <data root>
```

`--data` defaults to `HALO_DATA_ROOT`, then `build/linux`, then the repository
root and `assets/`. `--binary` selects another game (default
`build/linux/halo`). The game runs hidden, without sound, with one machine
hosting a network test game on the scenario's map (the existing
`debug.network_test`), and ends when the scenario's last tick is written. It
takes about ten seconds.

Is the engine deterministic for a scenario? Run it again and compare:

```
python tools/scenario_harness.py repeat tools/scenarios/walk_flat.scn -n 3
```

This prints, for each run after the first, the comparison of its trace with
the first run's with no tolerance, and `deterministic` or `NOT deterministic`
(exit status 0 or 1). A run that differs is reported by quantity, with the
first tick over zero difference and the largest difference.

## Compare two traces

```
python tools/scenario_harness.py compare engine.tsv rust.tsv --scenario tools/scenarios/walk_flat.scn
python tools/scenario_harness.py compare engine.tsv rust.tsv --tolerance position=0.5 --tolerance state=0
```

The first trace is the reference. The tolerances are the scenario's
`tolerance` lines, then each `--tolerance quantity=value` over them; a
quantity with neither has tolerance 0. Output, for each quantity: pass or
fail, the largest difference with its tick, and for a fail the first tick over
the tolerance; then `PASS` or `FAIL`. Exit status: 0 pass, 1 fail, 2 for
files or options that cannot be used.

The quantities and how a difference is measured at one tick:

| Quantity | Difference |
| --- | --- |
| `position` | distance between the two (x, y, z), world units |
| `velocity` | distance between the two (vx, vy, vz), world units a second |
| `facing` | the larger of the yaw difference (wrapped, so that +pi and -pi are the same) and the pitch difference, radians |
| `state` | the number of bits of `state` that differ |

Traces of different lengths fail.

Run the Rust simulation's trace through the same `compare`: it needs only the
file in the format below.

## Run a scenario in the Rust simulation

`rust/halo-scenario` plays a scenario in the simulation's walking
(`halo_sim::walk`, on the map's own collision data and tags) and writes the
trace in the format below. It reads the same `.scn` files and needs the game's
own map files (`--maps`, or `HALO_MAP_DIR`):

```
cd rust
cargo run --release -p halo-scenario -- ../tools/scenarios/walk_flat.scn --maps <data root>/maps --out rust.tsv
python ../tools/scenario_harness.py compare engine.tsv rust.tsv --scenario ../tools/scenarios/walk_flat.scn
```

A scenario that jumps or crouches is refused until those are simulated.
The simulation has the map's collision BSP alone: a scenario that walks the
engine's player into scenery or another object (a rock, a crate) does not
match, as the engine stops there and the simulation does not, so scenarios
keep to open ground and the walls of the map itself.

## Add a scenario

Add `tools/scenarios/<name>.scn`; the file's `scenario` line must be its name.
To accept a feature, add scenarios that exercise it, run the engine to see
what it does (look at the trace; check that the scenario does what its comment
says), and set the tolerances. Keep a scenario short: the engine plays it in
real time (30 ticks a second).

## Scenario format

Plain text, one statement a line; `#` starts a comment; blank lines are
ignored.

```
scenario walk_flat            # the name
map bloodgulch                # the multiplayer map (its file name without .map)
start flat_field 77.9 -166.2 0.32 0.0
ticks 180
input 30 180 forward=1.0
tolerance position 0.25
```

- `start <name> <x> <y> <z> <yaw>`: the named starting location: the
  player's position in world units (a little above the ground, so that the
  player settles on it) and facing (yaw in radians, 0 along +x, turning
  towards +y).
- `ticks <n>`: the length, 1 to 36000 ticks of 1/30 second.
- `input <first> <end> <key>=<value> ...`: sets the keys for ticks `first` up to
  but not including `end`. Later lines win over earlier ones on the ticks they
  both cover, key by key. Keys:

  | Key | Meaning | Default |
  | --- | --- | --- |
  | `forward` | throttle ahead, -1 to 1 | 0 |
  | `strafe` | throttle to the left, -1 to 1 | 0 |
  | `yaw` | the facing, radians (absolute) | the start's yaw |
  | `pitch` | the aim up (+) or down (-), radians (absolute) | 0 |
  | `jump` | 1 while the jump button is held | 0 |
  | `crouch` | 1 while the crouch button is held | 0 |

  `Scenario.tick_inputs()` in `tools/scenario_harness.py` expands a scenario to
  the inputs of every tick.
- `tolerance <quantity> <value>`: the difference of a quantity that a
  comparison with this scenario allows by default (see above). They are starting
  values: a feature's ticket tunes them to what the feature needs.

In the engine, tick 0 is the first tick the local player's unit is in play with
input enabled. The unit is put at `start`, at rest, in that tick, and the
tick's inputs are the player's controls for it, in place of the controller's
(the engine's own conversion of facing and throttle to movement applies, as for
a person).

## Trace format

A text file, tab separated, one line a tick. The C engine writes it; the Rust
simulation must write the same.

```
# halo-trace 1
# scenario walk_flat
# map bloodgulch
# source c-engine
tick	x	y	z	vx	vy	vz	yaw	pitch	state
0	77.900000	-166.200000	0.312000	0.000000	0.000000	-0.234375	0.000000	0.000000	1
1	...
```

- The first line is `# halo-trace 1`. Other `#` lines are headers (`# <key> <value>`);
  `scenario`, `map` and `source` are the ones in use. Readers ignore the rest.
- Then the column line above, exactly, then one line for each tick from 0 to
  `ticks - 1`, in order, with no gaps. Readers split on any white space.
- Row `k` is the player's state **after** tick `k` has been simulated, with
  inputs `k` applied: a simulation starts from the `start`, at rest, applies
  `inputs[0]`, steps once, and writes row 0.
- Columns, all numbers (the engine writes six decimals):

  | Column | Meaning |
  | --- | --- |
  | `tick` | the tick, from 0 |
  | `x`, `y`, `z` | the unit's position, world units (its origin, near the feet) |
  | `vx`, `vy`, `vz` | its velocity, world units a second (the engine's units a tick times 30) |
  | `yaw`, `pitch` | where it aims, radians: yaw from -pi to pi, 0 along +x, turning towards +y; pitch up positive |
  | `state` | the movement state, a sum of bits: 1 airborne (not on the ground), 2 crouching |
