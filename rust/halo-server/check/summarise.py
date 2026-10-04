#!/usr/bin/env python3
"""Summarise a halo-server log written with log_secs = 1: tick time, ticks a
second, upload and the gateway's counters, second by second, over the seconds
the match was full (at least 98% of its players).

    summarise.py server.log [min-players]
"""
import os
import re
import sys

LINE = re.compile(
    r"\[\s*(?P<at>\d+)s\] (?P<server>\S+): match (?P<match>\d+) (?P<map>\S+) "
    r"tick (?P<whole>[\d.]+) ms \(module (?P<module>[\d.]+)\) x(?P<ticks>\d+) \| "
    r"players (?P<players>\d+)/(?P<cap>\d+) \((?P<udp>\d+) on UDP\) \| "
    r"out (?P<out>[\d.]+) MB/s \((?P<per>[\d.]+) KB/s a player\) \| "
    r"rejected moves (?P<rej>\d+) \(\+(?P<rejw>\d+) in (?P<win>[\d.]+) s\) \| "
    r"rejected hits (?P<hits>\d+) \(\+(?P<hitsw>\d+)\) \| "
    r"hits accepted (?P<accepted>\d+), refused as target-not-where-seen (?P<unseen>\d+) \| "
    r"inputs late (?P<late>\d+) unbound (?P<unbound>\d+) \| "
    r"gateway ticks missed (?P<missed>\d+), arrival p50 (?P<ap50>[\d.]+) max (?P<amax>[\d.]+) ms, send p50 (?P<sp50>[\d.]+) max (?P<smax>[\d.]+) ms"
)


def pct(values, q):
    values = sorted(values)
    return values[min(len(values) - 1, int(q * (len(values) - 1) + 0.5))]


def cpu(path, start, end):
    """Average cores used by each program between two unix times, from cpu.log."""
    rows = []
    try:
        for text in open(path):
            parts = text.split()
            rows.append((float(parts[0]), {k: int(v) for k, v in (p.split("=") for p in parts[1:])}))
    except OSError:
        return
    window = [r for r in rows if start <= r[0] <= end]
    if len(window) < 2:
        return
    (t0, a), (t1, b) = window[0], window[-1]
    used = {k: (b[k] - a[k]) / 100.0 / (t1 - t0) for k in b if k in a}
    print("cores used on average over the full-match seconds (from /proc, " + f"{t1 - t0:.0f} s): " + ", ".join(f"{k} {v:.2f}" for k, v in used.items()))


def threads(path, start, end):
    """The busiest threads of SpacetimeDB over the full-match seconds, in CPU time (threads.log). The
    thread that runs the match's tick is the one that is busy 30 times a second, so its CPU time a tick
    is its share of a core over 30: a figure that a busy machine does not stretch the way it does a
    tick's wall-clock time (though a sibling thread sharing the core still slows it)."""
    rows = []
    try:
        for text in open(path):
            fields = text.split()
            rows.append((float(fields[0]), {p.split(":")[0]: (p.split(":")[1], int(p.split(":")[2])) for p in fields[1:]}))
    except (OSError, ValueError, IndexError):
        return
    window = [r for r in rows if start <= r[0] <= end]
    if len(window) < 2:
        return
    (t0, a), (t1, b) = window[0], window[-1]
    used = sorted(((b[k][1] - a[k][1]) / 100.0 / (t1 - t0), k, b[k][0]) for k in b if k in a)[::-1][:3]
    print(
        f"busiest threads of spacetimedb-standalone, CPU time over {t1 - t0:.0f} s: "
        + ", ".join(f"{name} {tid} {cores:.3f} cores = {cores / 30 * 1000:.2f} ms a tick" for cores, tid, name in used)
    )


def parts(path, whole, full):
    """Tick age in its parts, side by side with what the players saw (load-report.txt, next to the log).

    The stamp on a tick is the start of the tick, so a player's tick age is the tick time, then the
    commit and the delivery to the gateway, then the gateway's send up to that player's datagram.
    The arrival figure (the stamp to the tick reaching the gateway) holds the first two; the player's
    age less it is the send. The gateway's figures are over the whole run so far, so the last full
    second's are used; the tick time is the mean over the full seconds.
    """
    last = full[-1]
    mean_whole = sum(whole) / len(whole)
    print(
        f"tick age in parts (ms): tick time {mean_whole:.2f} mean | on reaching the gateway p50 {last['ap50']:.2f} "
        f"max {last['amax']:.2f} (so commit and delivery add about {last['ap50'] - mean_whole:.2f} to the tick time) | "
        f"gateway send p50 {last['sp50']:.2f} max {last['smax']:.2f}"
    )
    try:
        text = open(os.path.join(os.path.dirname(path), "load-report.txt")).read()
    except OSError:
        return
    m = re.search(r"tick age on arrival\s+p50 ([\d.]+)\s+p99 ([\d.]+)\s+max ([\d.]+) ms", text)
    if m:
        p50 = float(m.group(1))
        print(
            f"tick age the players saw (load report): p50 {p50:.2f}  p99 {float(m.group(2)):.2f}  max {float(m.group(3)):.2f} ms; "
            f"p50 less the gateway's arrival p50 is {p50 - last['ap50']:.2f} ms (the send to the median player and the loopback)"
        )


def main():
    path = sys.argv[1]
    rows = []
    notes = []
    for text in open(path, errors="replace"):
        m = LINE.search(text)
        if m:
            rows.append({k: float(v) if k not in ("server", "map") else v for k, v in m.groupdict().items()})
        elif "halo-server" in text or "lounge:" in text:
            notes.append(text.rstrip())
    if not rows:
        print("no report lines in", path)
        return
    top = max(r["players"] for r in rows)
    floor = float(sys.argv[2]) if len(sys.argv) > 2 else 0.98 * top
    full = [r for r in rows if r["players"] >= floor]
    print(f"server log {path}: {len(rows)} report lines, most players {top:.0f}; {len(full)} seconds with {floor:.0f} or more players")
    if not full:
        return
    whole = [r["whole"] for r in full]
    module = [r["module"] for r in full]
    print(
        f"tick time (whole call, mean per second)  mean {sum(whole)/len(whole):.2f}  p50 {pct(whole, .5):.2f}  "
        f"p99 {pct(whole, .99):.2f}  max {max(whole):.2f} ms"
    )
    print(
        f"tick time (module's part)                mean {sum(module)/len(module):.2f}  p50 {pct(module, .5):.2f}  "
        f"p99 {pct(module, .99):.2f}  max {max(module):.2f} ms"
    )
    worst = sorted(full, key=lambda r: -r["whole"])[:5]
    print("worst seconds by tick time:", ", ".join(f"{r['whole']:.2f} ms at {r['at']:.0f}s" for r in worst))
    ticks = [r["ticks"] for r in full]
    # (a window is a little over or under a second: ticks per second = ticks / the window's length)
    rates = [r["ticks"] / r["win"] for r in full]
    print(
        f"ticks a second: fewest in a window {min(ticks):.0f} (rate {min(rates):.2f}/s), "
        f"lowest rate {min(rates):.2f}/s, mean {sum(rates)/len(rates):.2f}/s; windows under 29 ticks/s: {sum(1 for x in rates if x < 29)}"
    )
    out = [r["out"] for r in full]
    per = [r["per"] for r in full]
    print(
        f"upload: mean {sum(out)/len(out):.2f} MB/s, max {max(out):.2f} MB/s; "
        f"per player mean {sum(per)/len(per):.1f} KB/s, max of the seconds {max(per):.1f} KB/s"
    )
    last = full[-1]
    first = full[0]
    parts(path, whole, full)
    try:
        began = float(open(os.path.join(os.path.dirname(path), "server.start")).read())
        cpu(os.path.join(os.path.dirname(path), "cpu.log"), began + first["at"], began + last["at"])
        threads(os.path.join(os.path.dirname(path), "threads.log"), began + first["at"], began + last["at"])
    except (OSError, ValueError):
        pass
    print(
        f"rejected moves {last['rej']:.0f} (since the match began; {last['rej'] - first['rej']:.0f} while full), "
        f"hits accepted {last['accepted']:.0f}, rejected {last['hits']:.0f} of which as target-not-where-seen "
        f"{last['unseen']:.0f}; inputs late (sum of windows) {sum(r['late'] for r in full):.0f}, "
        f"unbound {sum(r['unbound'] for r in full):.0f}; gateway ticks missed {sum(r['missed'] for r in full):.0f}; "
        f"gateway send time p50 up to {max(r['sp50'] for r in full):.2f} ms, max {max(r['smax'] for r in full):.2f} ms"
    )
    for n in notes:
        if any(w in n for w in ("ended", "is over", "ready", "listed", "deleted", "could not", "stopped")):
            print(n)


if __name__ == "__main__":
    main()
