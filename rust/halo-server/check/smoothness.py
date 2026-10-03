#!/usr/bin/env python3
"""How current the real game's picture of the players around it was, from a game
log made with GAME_LOG=1 (large.log_players): each second the game logs the
gateway's newest tick and, for every player it holds, the tick of the state it
last had of them and where. The age of a player's state, in ticks (a tick is
33.3 ms), is the newest tick less the state's tick; the players are put in
bands by their distance from the game's own player.

    smoothness.py game.log [seconds-to-leave-out-at-the-start]

Prints, for each band: how many player-seconds, the age's median, p99 and
largest, and the share of them older than 4 ticks (133 ms).
"""
import math
import re
import sys

GATEWAY = re.compile(r"large mode: tick \d+ joined .*gateway tick (?P<tick>\d+),")
PLAYER = re.compile(r"large mode: player (?P<id>\d+) tick (?P<tick>\d+) \((?P<x>-?[\d.]+) (?P<y>-?[\d.]+) (?P<z>-?[\d.]+)\)")
LOCAL = re.compile(r"large mode: local unit \((?P<x>-?[\d.]+) (?P<y>-?[\d.]+) (?P<z>-?[\d.]+)\)")
BANDS = [(0, 10), (10, 25), (25, 60), (60, 1e9)]


def pct(values, q):
    values = sorted(values)
    return values[min(len(values) - 1, int(q * (len(values) - 1) + 0.5))]


def main():
    skip = int(sys.argv[2]) if len(sys.argv) > 2 else 10
    seconds = []  # (gateway tick, local position, [(distance, age)])
    local = None
    current = None
    for text in open(sys.argv[1], errors="replace"):
        if m := LOCAL.search(text):
            local = (float(m["x"]), float(m["y"]), float(m["z"]))
        elif m := GATEWAY.search(text):
            current = (int(m["tick"]), local, [])
            seconds.append(current)
        elif (m := PLAYER.search(text)) and current is not None and current[1] is not None:
            at = (float(m["x"]), float(m["y"]), float(m["z"]))
            d = math.dist(at, current[1])
            current[2].append((d, current[0] - int(m["tick"])))
    seconds = [s for s in seconds[skip:] if s[2]]
    if not seconds:
        print("no player lines: run with GAME_LOG=1")
        return
    print(f"{len(seconds)} seconds of the game's picture, {sum(len(s[2]) for s in seconds)} player-seconds")
    for lo, hi in BANDS:
        ages = [a for s in seconds for d, a in s[2] if lo <= d < hi]
        if not ages:
            continue
        older = sum(1 for a in ages if a > 4) / len(ages) * 100
        label = f"{lo:>3} to {hi:<4.0f}" if hi < 1e8 else f"{lo:>3} to ... "
        print(
            f"  {label} wu  {len(ages):>7} player-seconds   age in ticks: median {pct(ages, .5):>3}  p99 {pct(ages, .99):>3}  "
            f"largest {max(ages):>3}   older than 4 ticks: {older:.2f}%"
        )


if __name__ == "__main__":
    main()
