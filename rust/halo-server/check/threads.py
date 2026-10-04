#!/usr/bin/env python3
"""CPU of each thread of halo-server between two moments of a run, from server-threads.log.

  python3 check/threads.py server-threads.log [from_secs [to_secs]]   (seconds since the first sample)

Prints, per thread name, the CPU in milliseconds a tick (30 ticks a second) over that stretch, as user time
(the program's own code: planning, packing) and system time (the kernel on its behalf: the sends), and the total.
"""
import collections
import sys

rows = []
for line in open(sys.argv[1]):
    parts = line.split()
    # "<tid>:<name>:<user ticks>:<system ticks>"
    rows.append((float(parts[0]), {t.split(":")[0]: (t.split(":")[1], int(t.split(":")[2]), int(t.split(":")[3])) for t in parts[1:]}))
t0 = rows[0][0]
lo = float(sys.argv[2]) if len(sys.argv) > 2 else 0
hi = float(sys.argv[3]) if len(sys.argv) > 3 else 1e9
rows = [r for r in rows if lo <= r[0] - t0 <= hi]
secs = rows[-1][0] - rows[0][0]
# a thread's CPU in the stretch: from its first sample in it to its last (threads come and go)
seen = {}
for _, threads in rows:
    for tid, (name, user, system) in threads.items():
        s = seen.setdefault(tid, [name, user, system, user, system])
        s[3], s[4] = user, system
by_name = collections.defaultdict(lambda: [0.0, 0.0])
for name, user0, system0, user1, system1 in seen.values():
    by_name[name][0] += (user1 - user0) * 10.0  # ms of CPU
    by_name[name][1] += (system1 - system0) * 10.0
total = [0.0, 0.0]
print(f"{'':24} {'user':>6} {'system':>7}   (ms of CPU a tick)")
for name, (user, system) in sorted(by_name.items(), key=lambda kv: -(kv[1][0] + kv[1][1])):
    per_tick = [user / (secs * 30), system / (secs * 30)]
    total = [total[0] + per_tick[0], total[1] + per_tick[1]]
    if sum(per_tick) >= 0.02:
        print(f"{name:24} {per_tick[0]:6.2f} {per_tick[1]:7.2f}")
print(f"{'total':24} {total[0]:6.2f} {total[1]:7.2f}   over {secs:.0f} s")
