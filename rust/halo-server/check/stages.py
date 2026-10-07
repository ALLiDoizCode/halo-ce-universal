#!/usr/bin/env python3
"""Where a tick's time goes inside the match module, from the host's "Timing span" lines.

The match module, built with `--features stage-timing` (run.sh: `MODULE_FEATURES=stage-timing`), times the
stages of its `tick` reducer with the host's console timers; the host writes a line to the database's
log for each, with its duration. Give this the module-logs/ files run.sh copies out, and, to set the
stages beside the whole call, the server-summary's mean tick time.

    stages.py module-logs/*.log [--skip 0.3] [--whole-ms 3.2] [--module-ms 2.7]
    stages.py module-logs/*.log --ticks     # single ticks instead (below)

Each stage's mean per tick over the last part of the run (the first `--skip` of the lines are
dropped: the crowd is still joining), its share of the stages' sum, and what is left of the module's
and of the whole call. (The timers are themselves host calls: the stages add up to a little more
than the module's time with them off.)

`--ticks` prints, instead of that report, two single ticks of the whole run (no lines are skipped, so
the match's first full tick is in it): the slowest tick, and the first tick whose stages' sum is over
twice the median tick's (the first full tick, when the match starts). Each stage's time in that tick
stands beside the stage's median over the run. A tick starts where a stage that the tick has already
run is logged again.
"""
import argparse
import collections
import re

# (the log's lines are JSON: the quotes of the span's name are escaped)
SPAN = re.compile(r'Timing span \\?"(?P<name>[^"\\]+)\\?": (?P<value>[\d.]+)\s*(?P<unit>ns|µs|us|ms|s)\b')
UNIT = {"ns": 1e-6, "µs": 1e-3, "us": 1e-3, "ms": 1.0, "s": 1e3}


def split_ticks(spans):
    """The spans (name, ms) in log order, as one dict of stage -> ms per tick."""
    ticks = []
    for name, ms in spans:
        if not ticks or name in ticks[-1]:
            ticks.append({})
        ticks[-1][name] = ms
    return ticks


def median(v):
    v = sorted(v)
    return v[len(v) // 2]


def single_ticks(spans):
    """Report the slowest tick and the first tick over twice the median tick, stage by stage."""
    ticks = split_ticks(spans)
    sums = [sum(t.values()) for t in ticks]
    med_sum = median(sums)
    by_name = collections.defaultdict(list)
    for t in ticks:
        for name, ms in t.items():
            by_name[name].append(ms)
    med = {name: median(v) for name, v in by_name.items()}
    first_slow = next((i for i, s in enumerate(sums) if s > 2 * med_sum), None)
    print(f"{len(ticks)} ticks; median tick's stages' sum {med_sum:.3f} ms")
    picks = [("slowest tick", max(range(len(ticks)), key=sums.__getitem__))]
    picks.append(("first tick over twice the median", first_slow))
    for label, i in picks:
        if i is None:
            print(f"\n{label}: none")
            continue
        print(f"\n{label}: tick #{i + 1} of the run, stages' sum {sums[i]:.3f} ms")
        print(f"{'stage':<40} {'this tick':>10} {'median':>8} {'x median':>9}  ms")
        for name in sorted(ticks[i]):
            ms = ticks[i][name]
            ratio = f"{ms / med[name]:>8.1f}x" if med[name] > 0 else f"{'-':>9}"
            print(f"{name:<40} {ms:>10.3f} {med[name]:>8.3f} {ratio}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("logs", nargs="+")
    ap.add_argument("--skip", type=float, default=0.3)
    ap.add_argument("--whole-ms", type=float)
    ap.add_argument("--module-ms", type=float)
    ap.add_argument("--ticks", action="store_true", help="report single ticks (the slowest, the first full one)")
    args = ap.parse_args()

    spans = []  # (name, ms) in log order
    for path in args.logs:
        for line in open(path, errors="replace"):
            m = SPAN.search(line)
            if m:
                spans.append((m["name"], float(m["value"]) * UNIT[m["unit"]]))
    if not spans:
        print("no timing spans in", ", ".join(args.logs))
        return
    if args.ticks:
        single_ticks(spans)
        return
    spans = spans[int(len(spans) * args.skip):]
    by_name = collections.defaultdict(list)
    for name, ms in spans:
        by_name[name].append(ms)
    # one tick runs each stage once (a stage inside a branch the tick skipped has fewer samples): the
    # tick count is the most samples any stage has
    ticks = max(len(v) for v in by_name.values())
    total = sum(sum(v) for v in by_name.values()) / ticks
    print(f"{ticks} ticks; stages' sum {total:.3f} ms a tick")
    print(f"{'stage':<40} {'mean/tick':>10} {'share':>7} {'p50':>8} {'p99':>8} {'max':>8}  ms")
    for name in sorted(by_name):
        v = sorted(by_name[name])
        mean = sum(v) / ticks
        p = lambda q: v[min(len(v) - 1, int(q * (len(v) - 1) + 0.5))]
        print(f"{name:<40} {mean:>10.3f} {100 * mean / total:>6.1f}% {p(.5):>8.3f} {p(.99):>8.3f} {v[-1]:>8.3f}")
    if args.module_ms:
        print(f"module's part {args.module_ms:.2f} ms: stages {total:.2f}, the rest (the reducer's own entry and exit, the timers) {args.module_ms - total:.2f}")
    if args.whole_ms and args.module_ms:
        print(f"whole call {args.whole_ms:.2f} ms: outside the module (commit, subscriptions) {args.whole_ms - args.module_ms:.2f} ms = {100 * (args.whole_ms - args.module_ms) / args.whole_ms:.0f}%")


if __name__ == "__main__":
    main()
