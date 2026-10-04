#!/usr/bin/env python3
"""Where a tick's time goes inside the match module, from the host's "Timing span" lines.

The match module, built with `--features stage-timing` (run.sh: `MODULE_FEATURES=stage-timing`), times the
stages of its `tick` reducer with the host's console timers; the host writes a line to the database's
log for each, with its duration. Give this the module-logs/ files run.sh copies out, and, to set the
stages beside the whole call, the server-summary's mean tick time.

    stages.py module-logs/*.log [--skip 0.3] [--whole-ms 3.2] [--module-ms 2.7]

Each stage's mean per tick over the last part of the run (the first `--skip` of the lines are
dropped: the crowd is still joining), its share of the stages' sum, and what is left of the module's
and of the whole call. (The timers are themselves host calls: the stages add up to a little more
than the module's time with them off.)
"""
import argparse
import collections
import re

# (the log's lines are JSON: the quotes of the span's name are escaped)
SPAN = re.compile(r'Timing span \\?"(?P<name>[^"\\]+)\\?": (?P<value>[\d.]+)\s*(?P<unit>ns|µs|us|ms|s)\b')
UNIT = {"ns": 1e-6, "µs": 1e-3, "us": 1e-3, "ms": 1.0, "s": 1e3}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("logs", nargs="+")
    ap.add_argument("--skip", type=float, default=0.3)
    ap.add_argument("--whole-ms", type=float)
    ap.add_argument("--module-ms", type=float)
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
