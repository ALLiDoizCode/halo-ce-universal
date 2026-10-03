"""The comparison harness: scenarios, traces, and the comparison of two traces.

A scenario (tools/scenarios/*.scn) is a script of per-tick inputs on a named
map from a named start. The C engine plays it headlessly and writes a trace of
the player's state for every tick (port/linux/game/scenario_harness.c); the
Rust simulation of the large-scale mode writes a trace in the same format; this
tool compares two traces within per-quantity tolerances. The formats are
documented in tools/scenarios/README.md.

    python tools/scenario_harness.py run tools/scenarios/walk_flat.scn trace.tsv
    python tools/scenario_harness.py repeat tools/scenarios/walk_flat.scn
    python tools/scenario_harness.py compare engine.tsv rust.tsv --scenario tools/scenarios/walk_flat.scn

Exit status: 0 pass, 1 fail (a difference beyond a tolerance, or a run that
did not give a trace), 2 unusable arguments or files.
"""

from __future__ import annotations

import argparse
import math
import os
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path

REPOSITORY = Path(__file__).resolve().parent.parent
SCENARIOS = Path(__file__).resolve().parent / "scenarios"

# the quantities a comparison has a tolerance for, and their units
QUANTITIES = {
    "position": "world units",
    "velocity": "world units a second",
    "facing": "radians",
    "state": "bits that differ",
}
# ... and the quantities of a scenario of firing (a trace of version 2)
COMBAT_QUANTITIES = {
    "rounds": "rounds, loaded and left",
    "heat": "of the weapon's heat",
    "shield": "of the target's full shield",
    "body": "of the target's full health",
    "stun": "ticks of the target's shield stun",
    "life": "1 where the target's death differs",
    "hit": "1 where the part of the target that was hit differs",
}
ALL_QUANTITIES = {**QUANTITIES, **COMBAT_QUANTITIES}
TRACE_COLUMNS = ("tick", "x", "y", "z", "vx", "vy", "vz", "yaw", "pitch", "state")
COMBAT_COLUMNS = ("rounds", "total", "heat", "shield", "body", "stun", "dead", "hit")
INPUT_KEYS = ("forward", "strafe", "yaw", "pitch", "jump", "crouch", "fire", "part")


class HarnessError(Exception):
    """A scenario, trace or run that cannot be used (exit status 2)."""


class RunFailed(HarnessError):
    """A run of the engine that gave no trace (exit status 1)."""


# ---------- scenarios


@dataclass
class Scenario:
    name: str
    map: str
    start_name: str
    start: tuple[float, float, float]
    start_yaw: float
    ticks: int
    # the tolerances a comparison to this scenario's trace allows by default
    tolerances: dict[str, float] = field(default_factory=dict)
    # (first tick, end tick, {key: value}), in file order
    inputs: list[tuple[int, int, dict[str, float]]] = field(default_factory=list)
    # a scenario of firing: the weapon tag's name the player is given (such as
    # weapons/pistol/pistol), the target's place (x, y, z, yaw), and how many
    # ticks a shot takes to reach it
    weapon: str | None = None
    target: tuple[float, float, float, float] | None = None
    flight: int = 0

    @property
    def firing(self) -> bool:
        return self.weapon is not None or self.target is not None

    def tick_inputs(self) -> list[dict[str, float]]:
        """The inputs of every tick: what the game is given for it."""
        ticks = [
            {"forward": 0.0, "strafe": 0.0, "yaw": self.start_yaw, "pitch": 0.0, "jump": 0.0, "crouch": 0.0,
             "fire": 0.0, "part": 1.0}
            for _ in range(self.ticks)
        ]
        for first, end, values in self.inputs:
            for tick in range(first, end):
                ticks[tick].update(values)
        return ticks


def parse_scenario(text: str, source: str = "scenario") -> Scenario:
    fields: dict[str, list[str]] = {}
    inputs: list[tuple[int, int, dict[str, float]]] = []
    tolerances: dict[str, float] = {}
    for number, line in enumerate(text.splitlines(), 1):
        words = line.split("#", 1)[0].split()
        if not words:
            continue
        where = f"{source}:{number}"
        keyword, rest = words[0], words[1:]
        try:
            if keyword in ("scenario", "map"):
                if len(rest) != 1:
                    raise HarnessError(f"{where}: {keyword} takes one name")
                fields[keyword] = rest
            elif keyword == "start":
                if len(rest) != 5:
                    raise HarnessError(f"{where}: start takes a name, x, y, z and yaw")
                fields[keyword] = rest
            elif keyword == "ticks":
                fields[keyword] = rest[:1]
            elif keyword == "weapon":
                if not rest:
                    raise HarnessError(f"{where}: weapon takes a weapon tag's name")
                # (the name may have spaces in it: weapons/assault rifle/assault rifle)
                fields[keyword] = [" ".join(rest)]
            elif keyword == "target":
                if len(rest) != 4:
                    raise HarnessError(f"{where}: target takes x, y, z and yaw")
                fields[keyword] = rest
            elif keyword == "flight":
                if len(rest) != 1:
                    raise HarnessError(f"{where}: flight takes a whole number")
                fields[keyword] = [str(int(rest[0]))]
            elif keyword == "tolerance":
                if len(rest) != 2 or rest[0] not in ALL_QUANTITIES:
                    raise HarnessError(f"{where}: tolerance takes one of {', '.join(ALL_QUANTITIES)} and a number")
                tolerances[rest[0]] = float(rest[1])
            elif keyword == "input":
                values: dict[str, float] = {}
                for word in rest[2:]:
                    key, equals, value = word.partition("=")
                    if not equals or key not in INPUT_KEYS:
                        raise HarnessError(f"{where}: unknown input {word!r} (keys: {', '.join(INPUT_KEYS)})")
                    values[key] = float(value)
                inputs.append((int(rest[0]), int(rest[1]), values))
            else:
                raise HarnessError(f"{where}: unknown line {keyword!r}")
        except (ValueError, IndexError) as error:
            raise HarnessError(f"{where}: unreadable ({error})") from error
    for needed in ("scenario", "map", "start", "ticks"):
        if needed not in fields:
            raise HarnessError(f"{source}: no {needed} line")
    ticks = int(fields["ticks"][0]) if fields["ticks"] else 0
    if not 0 < ticks <= 36000:
        raise HarnessError(f"{source}: ticks must be from 1 to 36000")
    for first, end, _ in inputs:
        if not 0 <= first <= end <= ticks:
            raise HarnessError(f"{source}: input ticks {first} to {end} are outside 0 to {ticks}")
    start = fields["start"]
    target = fields.get("target")
    return Scenario(
        name=fields["scenario"][0], map=fields["map"][0], start_name=start[0],
        start=(float(start[1]), float(start[2]), float(start[3])), start_yaw=float(start[4]),
        ticks=ticks, tolerances=tolerances, inputs=inputs,
        weapon=fields["weapon"][0] if "weapon" in fields else None,
        target=tuple(float(v) for v in target) if target else None,
        flight=int(fields["flight"][0]) if "flight" in fields else 0,
    )


def load_scenario(path: Path) -> Scenario:
    try:
        return parse_scenario(Path(path).read_text(encoding="utf-8"), str(path))
    except OSError as error:
        raise HarnessError(f"cannot read {path}: {error}") from error


# ---------- traces


@dataclass
class Trace:
    header: dict[str, str]
    # one row per tick, in TRACE_COLUMNS order after the tick number (and, in a
    # trace of version 2, COMBAT_COLUMNS after those)
    rows: list[tuple[float, ...]]

    @property
    def firing(self) -> bool:
        return self.header.get("halo-trace") == "2"


def parse_trace(text: str, source: str = "trace") -> Trace:
    header: dict[str, str] = {}
    rows: list[tuple[float, ...]] = []
    seen_columns = False
    for number, line in enumerate(text.splitlines(), 1):
        if line.startswith("#"):
            key, _, value = line[1:].strip().partition(" ")
            header[key] = value.strip()
            continue
        words = line.split()
        if not words:
            continue
        if not seen_columns:
            version = header.get("halo-trace")
            if version not in ("1", "2"):
                raise HarnessError(f"{source}:{number}: not a halo-trace file (no '# halo-trace 1' or 2 first)")
            columns = TRACE_COLUMNS + (COMBAT_COLUMNS if version == "2" else ())
            if tuple(words) != columns:
                raise HarnessError(f"{source}:{number}: columns must be {' '.join(columns)}")
            seen_columns = True
            continue
        try:
            row = tuple(float(word) for word in words)
        except ValueError as error:
            raise HarnessError(f"{source}:{number}: unreadable ({error})") from error
        if len(row) != len(columns):
            raise HarnessError(f"{source}:{number}: {len(row)} columns, not {len(columns)}")
        if row[0] != len(rows):
            raise HarnessError(f"{source}:{number}: tick {row[0]:g} where tick {len(rows)} is next")
        rows.append(row)
    if not seen_columns:
        raise HarnessError(f"{source}: not a halo-trace file (no column line)")
    return Trace(header, rows)


def load_trace(path: Path) -> Trace:
    try:
        return parse_trace(Path(path).read_text(encoding="utf-8"), str(path))
    except OSError as error:
        raise HarnessError(f"cannot read {path}: {error}") from error


# ---------- comparison


def angle_difference(a: float, b: float) -> float:
    """The angle from b to a in radians, from -pi to pi."""
    return (a - b + math.pi) % (2.0 * math.pi) - math.pi


def row_differences(a: tuple[float, ...], b: tuple[float, ...]) -> dict[str, float]:
    differences = {
        "position": math.dist(a[1:4], b[1:4]),
        "velocity": math.dist(a[4:7], b[4:7]),
        "facing": max(abs(angle_difference(a[7], b[7])), abs(a[8] - b[8])),
        "state": float(bin(int(a[9]) ^ int(b[9])).count("1")),
    }
    if len(a) > 10 and len(b) > 10:
        # (rounds, total, heat, shield, body, stun, dead, hit)
        differences.update({
            "rounds": float(max(abs(a[10] - b[10]), abs(a[11] - b[11]))),
            "heat": abs(a[12] - b[12]),
            "shield": abs(a[13] - b[13]),
            "body": abs(a[14] - b[14]),
            "stun": abs(a[15] - b[15]),
            "life": float(a[16] != b[16]),
            "hit": float(a[17] != b[17]),
        })
    return differences


@dataclass
class QuantityResult:
    tolerance: float
    largest: float = 0.0
    largest_tick: int | None = None
    first_over: int | None = None  # the first tick whose difference exceeds the tolerance

    @property
    def passed(self) -> bool:
        return self.first_over is None


@dataclass
class Comparison:
    ticks: tuple[int, int]
    quantities: dict[str, QuantityResult]

    @property
    def passed(self) -> bool:
        return self.ticks[0] == self.ticks[1] and all(q.passed for q in self.quantities.values())


def compare_traces(a: Trace, b: Trace, tolerances: dict[str, float] | None = None) -> Comparison:
    """Every tick of two traces, a quantity at a time: pass when no quantity's
    difference at any tick is over its tolerance (default 0) and the traces
    are as long as each other."""
    if a.firing != b.firing:
        raise HarnessError("one trace is of a scenario of firing (version 2) and the other is not")
    names = ALL_QUANTITIES if a.firing else QUANTITIES
    allowed = {name: 0.0 for name in names}
    allowed.update({name: value for name, value in (tolerances or {}).items() if name in names})
    unknown = set(tolerances or {}) - set(ALL_QUANTITIES)
    if unknown:
        raise HarnessError(f"no such quantity: {', '.join(sorted(unknown))}")
    results = {name: QuantityResult(allowed[name]) for name in names}
    for tick, (row_a, row_b) in enumerate(zip(a.rows, b.rows)):
        for name, difference in row_differences(row_a, row_b).items():
            result = results[name]
            if result.largest_tick is None or not difference <= result.largest:
                result.largest, result.largest_tick = difference, tick
            # (not <=: a difference that is not a number is over)
            if not difference <= result.tolerance and result.first_over is None:
                result.first_over = tick
    return Comparison((len(a.rows), len(b.rows)), results)


def format_comparison(comparison: Comparison) -> str:
    lines = []
    if comparison.ticks[0] != comparison.ticks[1]:
        lines.append(f"FAIL ticks: {comparison.ticks[0]} in the first trace, {comparison.ticks[1]} in the second")
    for name, result in comparison.quantities.items():
        unit = ALL_QUANTITIES[name]
        largest = f"largest {result.largest:.6g} at tick {result.largest_tick}" if result.largest_tick is not None else "no ticks"
        if result.passed:
            lines.append(f"pass {name}: {largest} (tolerance {result.tolerance:g} {unit})")
        else:
            lines.append(
                f"FAIL {name}: first over the tolerance at tick {result.first_over}; {largest} "
                f"(tolerance {result.tolerance:g} {unit})"
            )
    lines.append("PASS" if comparison.passed else "FAIL")
    return "\n".join(lines)


# ---------- running the C engine


def default_binary() -> Path:
    return REPOSITORY / "build" / "linux" / "halo"


def run_scenario(scenario_path: Path, trace_path: Path, binary: Path, data_root: Path,
                 timeout: float = 120.0, save_root: Path | None = None, keep_log: Path | None = None) -> None:
    """Plays a scenario in the C engine headlessly (hidden window, no audio,
    one machine hosting a network test game) and leaves its trace at
    trace_path (and the game's log at keep_log, if given). Raises HarnessError
    when the engine gives none."""
    scenario = load_scenario(scenario_path)
    if not binary.exists():
        raise HarnessError(f"no game at {binary} (build it: python configure.py && ninja linux)")
    if not (data_root / "maps").is_dir():
        raise HarnessError(f"no maps/ in the game data root {data_root}")
    trace_path = trace_path.resolve()
    trace_path.unlink(missing_ok=True)
    with tempfile.TemporaryDirectory(prefix="halo-scenario-") as temporary:
        environment = dict(
            os.environ,
            HALO_DATA_ROOT=str(data_root.resolve()),
            HALO_SAVE_ROOT=str(save_root or temporary),
            HALO_NETWORK_TEST=f"host:{scenario.map}",
            HALO_NETWORK_TEST_START="4",
            HALO_NET_ONLINE="0",
            HALO_FULLSCREEN="0",
            HALO_NO_VSYNC="1",
            HALO_NO_AUDIO="1",
            HALO_HIDDEN_WINDOW="1",
            HALO_UPDATE_ANSWER="no",
            HALO_EXIT_AFTER=str(int(timeout)),
            HALO_SCENARIO=str(Path(scenario_path).resolve()),
            HALO_SCENARIO_TRACE=str(trace_path),
        )
        log = Path(temporary) / "halo.log"
        with open(log, "w") as output:
            try:
                status = subprocess.run(
                    [str(binary.resolve())], cwd=binary.parent, env=environment, stdout=output,
                    stderr=subprocess.STDOUT, timeout=timeout + 30,
                ).returncode
            except subprocess.TimeoutExpired:
                status = None
        if keep_log:
            keep_log.write_text(log.read_text(errors="replace"))
        if not trace_path.exists():
            tail = "".join(log.read_text(errors="replace").splitlines(keepends=True)[-8:])
            raise RunFailed(
                f"the game gave no trace for {scenario.name} (exit status {status}); the end of its log:\n{tail}")


# ---------- command line


def parse_tolerance_options(options: list[str]) -> dict[str, float]:
    tolerances = {}
    for option in options:
        name, equals, value = option.partition("=")
        try:
            if not equals or name not in ALL_QUANTITIES:
                raise ValueError(name)
            tolerances[name] = float(value)
        except ValueError:
            raise HarnessError(f"--tolerance takes <{'|'.join(ALL_QUANTITIES)}>=<number>, not {option!r}") from None
    return tolerances


def find_data_root(option: str | None) -> Path:
    candidate = option or os.environ.get("HALO_DATA_ROOT")
    if candidate:
        return Path(candidate)
    for place in (REPOSITORY / "build" / "linux", REPOSITORY, REPOSITORY / "assets"):
        if (place / "maps").is_dir():
            return place
    raise HarnessError("no game data: pass --data or set HALO_DATA_ROOT to the folder that contains maps/")


def command_run(arguments: argparse.Namespace) -> int:
    run_scenario(arguments.scenario, arguments.trace, Path(arguments.binary), find_data_root(arguments.data),
                 keep_log=arguments.log)
    print(f"wrote {arguments.trace}")
    return 0


def command_repeat(arguments: argparse.Namespace) -> int:
    """Runs a scenario several times and reports how the runs differ from the
    first: the engine is deterministic when none do (any difference is
    reported, with the first tick and the size of the largest)."""
    binary, data = Path(arguments.binary), find_data_root(arguments.data)
    with tempfile.TemporaryDirectory(prefix="halo-repeat-") as temporary:
        traces = []
        for index in range(arguments.runs):
            path = Path(temporary) / f"run{index}.tsv"
            run_scenario(arguments.scenario, path, binary, data)
            traces.append(load_trace(path))
    deterministic = True
    for index, trace in enumerate(traces[1:], 2):
        comparison = compare_traces(traces[0], trace)
        print(f"run {index} against run 1:\n{format_comparison(comparison)}")
        deterministic = deterministic and comparison.passed
    print("deterministic" if deterministic else "NOT deterministic")
    return 0 if deterministic else 1


def command_compare(arguments: argparse.Namespace) -> int:
    tolerances = dict(load_scenario(arguments.scenario).tolerances) if arguments.scenario else {}
    tolerances.update(parse_tolerance_options(arguments.tolerance))
    comparison = compare_traces(load_trace(arguments.first), load_trace(arguments.second), tolerances)
    print(format_comparison(comparison))
    return 0 if comparison.passed else 1


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    commands = parser.add_subparsers(dest="command", required=True)

    def engine_options(command: argparse.ArgumentParser) -> None:
        command.add_argument("--binary", default=str(default_binary()), help="the game (default build/linux/halo)")
        command.add_argument("--data", help="the folder that contains maps/ (default HALO_DATA_ROOT)")

    run = commands.add_parser("run", help="play a scenario in the C engine and write its trace")
    run.add_argument("scenario", type=Path)
    run.add_argument("trace", type=Path)
    run.add_argument("--log", type=Path, help="keep the game's log here")
    engine_options(run)
    run.set_defaults(handler=command_run)

    repeat = commands.add_parser("repeat", help="play a scenario again and report whether the traces differ")
    repeat.add_argument("scenario", type=Path)
    repeat.add_argument("-n", "--runs", type=int, default=2)
    engine_options(repeat)
    repeat.set_defaults(handler=command_repeat)

    compare = commands.add_parser("compare", help="compare two traces within tolerances")
    compare.add_argument("first", type=Path, help="the reference, usually the C engine's")
    compare.add_argument("second", type=Path)
    compare.add_argument("--scenario", type=Path, help="take the tolerances of its tolerance lines")
    compare.add_argument("--tolerance", action="append", default=[], metavar="QUANTITY=VALUE")
    compare.set_defaults(handler=command_compare)

    arguments = parser.parse_args(argv)
    if arguments.command == "repeat" and arguments.runs < 2:
        parser.error("--runs must be at least 2")
    try:
        return arguments.handler(arguments)
    except HarnessError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1 if isinstance(error, RunFailed) else 2


if __name__ == "__main__":
    sys.exit(main())
