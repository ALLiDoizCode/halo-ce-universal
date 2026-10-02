"""Tests for the comparison harness (tools/scenario_harness.py, tools/scenarios).

The run of the C engine needs the game and the developer's own game data: it
is skipped without them (HALO_DATA_ROOT, or maps/ at build/linux, the
repository's root or assets/).
"""

import math
import os
from pathlib import Path

import pytest

from tools import scenario_harness as harness
from tools.scenario_harness import HarnessError

SCENARIO = """
# a comment
scenario test_walk
map bloodgulch
start spot 1.5 -2 3 0.5   # name x y z yaw
ticks 6
input 1 5 forward=1 strafe=-0.5
input 3 4 forward=0.25 jump=1
tolerance position 0.1
"""


def trace_text(rows, header="# halo-trace 1\n# scenario t\n"):
    lines = ["\t".join(str(value) for value in row) for row in rows]
    return header + "\t".join(harness.TRACE_COLUMNS) + "\n" + "\n".join(lines) + "\n"


def row(tick, x=0.0, y=0.0, z=0.0, vx=0.0, vy=0.0, vz=0.0, yaw=0.0, pitch=0.0, state=0):
    return (tick, x, y, z, vx, vy, vz, yaw, pitch, state)


def make_trace(rows):
    return harness.parse_trace(trace_text(rows))


# ---------- scenarios


def test_a_scenario_is_a_named_map_start_and_per_tick_inputs():
    scenario = harness.parse_scenario(SCENARIO)
    assert (scenario.name, scenario.map, scenario.start_name) == ("test_walk", "bloodgulch", "spot")
    assert scenario.start == (1.5, -2.0, 3.0) and scenario.start_yaw == 0.5 and scenario.ticks == 6
    assert scenario.tolerances == {"position": 0.1}
    ticks = scenario.tick_inputs()
    assert len(ticks) == 6
    # untouched ticks: no throttle, facing the start's way
    assert ticks[0] == {"forward": 0.0, "strafe": 0.0, "yaw": 0.5, "pitch": 0.0, "jump": 0.0, "crouch": 0.0}
    # an input line sets only the keys it names, over its ticks (the end is not included)
    assert ticks[1]["forward"] == 1.0 and ticks[1]["strafe"] == -0.5
    assert ticks[4]["forward"] == 1.0 and ticks[5]["forward"] == 0.0
    # a later line wins over an earlier on the ticks it covers
    assert ticks[3]["forward"] == 0.25 and ticks[3]["jump"] == 1.0 and ticks[3]["strafe"] == -0.5
    assert ticks[4]["jump"] == 0.0


@pytest.mark.parametrize("text, message", [
    ("map m\nstart s 0 0 0 0\nticks 1\n", "no scenario line"),
    ("scenario a\nstart s 0 0 0 0\nticks 1\n", "no map line"),
    ("scenario a\nmap m\nstart s 0 0 0\nticks 1\n", "start takes"),
    ("scenario a\nmap m\nstart s 0 0 0 0\nticks 0\n", "ticks must be"),
    ("scenario a\nmap m\nstart s 0 0 0 0\nticks 4\ninput 2 9 forward=1\n", "outside"),
    ("scenario a\nmap m\nstart s 0 0 0 0\nticks 4\ninput 0 2 sideways=1\n", "unknown input"),
    ("scenario a\nmap m\nstart s 0 0 0 0\nticks 4\ntolerance speed 1\n", "tolerance takes"),
    ("scenario a\nmap m\nstart s 0 0 0 0\nticks 4\nwarp 1\n", "unknown line"),
])
def test_a_scenario_that_cannot_be_played_is_rejected_with_its_line(text, message):
    with pytest.raises(HarnessError, match=message):
        harness.parse_scenario(text)


def test_the_scenarios_that_ship_cover_the_first_features():
    shipped = {path.stem: harness.load_scenario(path) for path in harness.SCENARIOS.glob("*.scn")}
    assert {"stand_still", "walk_flat", "walk_slope"} <= set(shipped)
    for stem, scenario in shipped.items():
        assert scenario.name == stem
        assert scenario.map and scenario.start_name
        assert set(scenario.tolerances) == set(harness.QUANTITIES)
    assert any(any(tick["forward"] for tick in s.tick_inputs()) for s in shipped.values())
    assert not any(tick["forward"] for tick in shipped["stand_still"].tick_inputs())


# ---------- traces


def test_a_trace_is_read_back_as_a_row_a_tick():
    trace = make_trace([row(0, x=1.5), row(1, x=2.5, state=3)])
    assert trace.header["scenario"] == "t"
    assert [r[1] for r in trace.rows] == [1.5, 2.5] and trace.rows[1][9] == 3


@pytest.mark.parametrize("text, message", [
    ("tick\tx\n", "not a halo-trace 1"),
    ("# halo-trace 1\ntick\tx\n", "columns must be"),
    ("# halo-trace 1\n" + "\t".join(harness.TRACE_COLUMNS) + "\n1 0 0 0 0 0 0 0 0 0\n", "tick 1 where tick 0"),
    ("# halo-trace 1\n" + "\t".join(harness.TRACE_COLUMNS) + "\n0 0 0\n", "3 columns"),
    ("# halo-trace 1\n", "no column line"),
])
def test_a_trace_that_is_not_one_is_rejected(text, message):
    with pytest.raises(HarnessError, match=message):
        harness.parse_trace(text)


# ---------- comparison


def test_identical_traces_pass_with_no_tolerance():
    rows = [row(0, x=1.0, vx=2.0, yaw=0.5), row(1, x=2.0, vx=2.0, yaw=0.5)]
    comparison = harness.compare_traces(make_trace(rows), make_trace(rows))
    assert comparison.passed
    assert all(result.largest == 0 for result in comparison.quantities.values())


def test_a_difference_over_the_tolerance_fails_with_its_first_tick_and_the_largest():
    a = make_trace([row(0), row(1), row(2), row(3)])
    b = make_trace([row(0, x=0.1), row(1, x=0.3), row(2, x=0.9), row(3, x=0.4)])
    comparison = harness.compare_traces(a, b, {"position": 0.2})
    result = comparison.quantities["position"]
    assert not comparison.passed and not result.passed
    assert result.first_over == 1
    assert result.largest_tick == 2 and result.largest == pytest.approx(0.9)
    # the other quantities are unaffected
    assert comparison.quantities["velocity"].passed
    report = harness.format_comparison(comparison)
    assert "FAIL position: first over the tolerance at tick 1" in report
    assert "largest 0.9 at tick 2" in report and report.endswith("FAIL")


def test_a_difference_within_the_tolerance_passes():
    a = make_trace([row(0), row(1)])
    b = make_trace([row(0, x=0.1), row(1, y=-0.1)])
    assert harness.compare_traces(a, b, {"position": 0.1}).passed
    assert not harness.compare_traces(a, b).passed


def test_position_and_velocity_are_distances_in_three_dimensions():
    a = make_trace([row(0)])
    b = make_trace([row(0, x=3.0, y=4.0, vz=2.0)])
    comparison = harness.compare_traces(a, b)
    assert comparison.quantities["position"].largest == pytest.approx(5.0)
    assert comparison.quantities["velocity"].largest == pytest.approx(2.0)


def test_facing_wraps_around_and_takes_the_worse_of_yaw_and_pitch():
    a = make_trace([row(0, yaw=math.pi - 0.01), row(1, pitch=0.2)])
    b = make_trace([row(0, yaw=-math.pi + 0.01), row(1, pitch=0.5)])
    result = harness.compare_traces(a, b).quantities["facing"]
    assert result.first_over == 0
    assert result.largest == pytest.approx(0.3) and result.largest_tick == 1
    assert harness.compare_traces(a, b, {"facing": 0.3}).passed is True


def test_state_differs_by_the_bits_that_are_not_the_same():
    a = make_trace([row(0, state=0), row(1, state=1)])
    b = make_trace([row(0, state=3), row(1, state=1)])
    result = harness.compare_traces(a, b).quantities["state"]
    assert result.largest == 2 and result.first_over == 0


def test_traces_of_different_lengths_fail_even_when_what_they_share_agrees():
    comparison = harness.compare_traces(make_trace([row(0), row(1)]), make_trace([row(0)]))
    assert not comparison.passed
    assert "2 in the first trace, 1 in the second" in harness.format_comparison(comparison)


def test_a_value_that_is_not_a_number_fails():
    a = make_trace([row(0), row(1)])
    b = make_trace([row(0), row(1, x=float("nan"))])
    result = harness.compare_traces(a, b, {"position": 1e9}).quantities["position"]
    assert result.first_over == 1 and not result.passed


def test_a_tolerance_for_no_quantity_is_an_error():
    with pytest.raises(HarnessError, match="no such quantity"):
        harness.compare_traces(make_trace([row(0)]), make_trace([row(0)]), {"speed": 1.0})


# ---------- command line


def test_compare_exits_by_the_outcome_and_takes_tolerances_from_the_scenario(tmp_path, capsys):
    scenario = tmp_path / "s.scn"
    scenario.write_text(SCENARIO)  # tolerance position 0.1
    first, second = tmp_path / "a.tsv", tmp_path / "b.tsv"
    first.write_text(trace_text([row(0), row(1)]))
    second.write_text(trace_text([row(0, x=0.05), row(1, x=0.05)]))

    assert harness.main(["compare", str(first), str(second)]) == 1
    assert harness.main(["compare", str(first), str(second), "--scenario", str(scenario)]) == 0
    # (an option wins over the scenario)
    assert harness.main(["compare", str(first), str(second), "--scenario", str(scenario),
                         "--tolerance", "position=0.01"]) == 1
    assert "FAIL position" in capsys.readouterr().out


def test_compare_exits_2_for_files_it_cannot_use(tmp_path, capsys):
    bad = tmp_path / "bad.tsv"
    bad.write_text("nonsense\n")
    assert harness.main(["compare", str(bad), str(bad)]) == 2
    assert harness.main(["compare", str(tmp_path / "missing.tsv"), str(bad)]) == 2
    assert harness.main(["compare", str(bad), str(bad), "--tolerance", "speed=1"]) == 2
    assert "error:" in capsys.readouterr().err


# ---------- the C engine


def game_data_root():
    candidates = [harness.REPOSITORY / "build" / "linux", harness.REPOSITORY, harness.REPOSITORY / "assets"]
    if "HALO_DATA_ROOT" in os.environ:
        candidates.insert(0, Path(os.environ["HALO_DATA_ROOT"]))
    return next((place for place in candidates if (place / "maps" / "bloodgulch.map").is_file()), None)


needs_engine = pytest.mark.skipif(
    game_data_root() is None or not harness.default_binary().exists(),
    reason="the built game (ninja linux) and the game data (maps/bloodgulch.map) are needed",
)


@needs_engine
def test_the_engine_plays_a_scenario_the_same_each_time_and_it_climbs_the_slope(tmp_path):
    scenario = harness.SCENARIOS / "walk_slope.scn"
    traces = []
    for index in range(2):
        path = tmp_path / f"run{index}.tsv"
        harness.run_scenario(scenario, path, harness.default_binary(), game_data_root())
        traces.append(harness.load_trace(path))
    assert len(traces[0].rows) == harness.load_scenario(scenario).ticks
    assert harness.compare_traces(traces[0], traces[1]).passed
    first, last = traces[0].rows[0], traces[0].rows[-1]
    assert last[2] > first[2] + 10  # walked north (+y) ...
    assert last[3] > first[3] + 3   # ... and up
    assert abs(last[1] - first[1]) < 0.5
