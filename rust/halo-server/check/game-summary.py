#!/usr/bin/env python3
"""Summarise the real game's log from a check run: frames a second, the slowest
frame, the remote units it held, and what the large-mode adapter cost a tick.

    game-summary.py game.log [seconds-to-leave-out-at-the-start [tick-the-match-ended-at]]

The match is the stretch from the first second with remote units to the gateway
tick the match ended at (the load program says it in its log); after that the final
scoreboard is up, and is summarised on its own.
"""
import re
import sys

FRAMES = re.compile(
    r"large mode: (?P<n>\d+) frames in (?P<s>[\d.]+) s: (?P<fps>[\d.]+) a second, the slowest (?P<slow>[\d.]+) ms, (?P<units>\d+) remote units"
)
GATEWAY_TICK = re.compile(r"gateway tick (?P<tick>\d+),")
ADAPTER = re.compile(r"large mode: the adapter cost (?P<mean>[\d.]+) ms a tick over (?P<ticks>\d+) ticks, (?P<worst>[\d.]+) ms at worst")
PLAYERS = re.compile(r"large mode: (?P<remote>\d+) remote units, (?P<players>\d+) with players")
LOCAL = re.compile(r"the server says the local player is (?P<state>\w+)")


def pct(values, q):
    values = sorted(values)
    return values[min(len(values) - 1, int(q * (len(values) - 1) + 0.5))]


def main():
    path = sys.argv[1]
    frames, adapters, players, states = [], [], [], []
    over = False
    end_tick = int(sys.argv[3]) if len(sys.argv) > 3 else None
    for text in open(path, errors="replace"):
        if end_tick is not None and (m := GATEWAY_TICK.search(text)) and int(m["tick"]) >= end_tick:
            over = True
        if m := FRAMES.search(text):
            frames.append({**{k: float(v) for k, v in m.groupdict().items()}, "over": over})
        elif m := ADAPTER.search(text):
            adapters.append({k: float(v) for k, v in m.groupdict().items()})
        elif m := PLAYERS.search(text):
            players.append(int(m["players"]))
        elif m := LOCAL.search(text):
            states.append(m["state"])
    inside = [f for f in frames if f["units"] > 0]
    scoreboard = [f for f in inside if f["over"]]
    inside = [f for f in inside if not f["over"]][int(sys.argv[2]) if len(sys.argv) > 2 else 5 :]
    if not inside:
        print("no frame lines with remote units in", path)
        return
    if scoreboard:
        sfps = [f["fps"] for f in scoreboard]
        print(f"final scoreboard up: {len(scoreboard)} seconds, frames a second min {min(sfps):.1f} median {pct(sfps, .5):.1f}")
    fps = [f["fps"] for f in inside]
    print(f"game log {path}: {len(inside)} seconds in the match with remote units (the first 5 left out)")
    print(f"seconds under 60 frames: {sum(1 for x in fps if x < 60)}; under 30: {sum(1 for x in fps if x < 30)}")
    print(
        f"frames a second: min {min(fps):.1f}  p1 {pct(fps, .01):.1f}  p50 {pct(fps, .5):.1f}  mean {sum(fps)/len(fps):.1f}  max {max(fps):.1f}"
    )
    slow = [f["slow"] for f in inside]
    print(f"slowest frame in a second: median {pct(slow, .5):.1f} ms, worst {max(slow):.1f} ms")
    units = [f["units"] for f in inside]
    print(f"remote units held: min {min(units):.0f}  median {pct(units, .5):.0f}  max {max(units):.0f}")
    if players:
        print(f"remote units with an engine player record: max {max(players)}")
    if adapters:
        mean = [a["mean"] for a in adapters if a["ticks"] > 0]
        worst = [a["worst"] for a in adapters]
        print(
            f"adapter cost a tick: mean of seconds {sum(mean)/len(mean):.3f} ms  p99 of seconds {pct(mean, .99):.3f} ms  worst tick {max(worst):.3f} ms"
        )
    if states:
        counts = {s: states.count(s) for s in sorted(set(states))}
        print(f"the local player's states told by the server: {counts}")


if __name__ == "__main__":
    main()
