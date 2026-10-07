# SpacetimeDB 30 Hz fan-out benchmark: results

Measured 2026-10-02 on one desktop. Companion to `../spacetimedb-netcode.md`, which found no published figure for this workload.

## What was tested

- **Server**: SpacetimeDB Standalone v2.10.2 (release binary, commit `58d2a407`), data directory on disk, pinned to 4 of the machine's 8 cores.
- **Module** (`module/src/lib.rs`): one `unit_state` row per player (56 bytes). A scheduled reducer runs every 33.3 ms, reads every player's latest input and rewrites every row. The per-player work is a few lines of arithmetic.
- **Load client** (`client/src/main.rs`): one real SDK connection per player (Rust SDK 2.10.2), pinned to the other 4 cores. Each connection subscribes to `unit_state`, calls a `send_input` reducer 30 times a second, and records the age of each tick's state on arrival (client clock minus the server timestamp written by the tick).
- **Visibility**: "all visible" means every client subscribes to every player. "32 visible" means players are spread over interest cells of 32 and each client subscribes to its own cell (`WHERE cell = n`).
- **Machine**: AMD Ryzen 7 5800X (8 cores, 16 threads), 31 GB RAM, Linux 7.2.5. Client and server share the machine and talk over loopback.

## Results

Compression off, confirmed reads on (the 2.x default) unless stated. Times in milliseconds.

| Players | Visible | Tick rate held | State age p50 | p99 | max | Arrival gaps over 100 ms | Server cores | Load-client cores |
|---|---|---|---|---|---|---|---|---|
| 128 | all | 30.0 Hz | 4.2 | 30.8 | 51.6 | 0 | 0.59 | 0.87 |
| 256 | all | 30.0 Hz | 14.0 | 25.5 | 62.8 | 0 | 1.15 | 2.80 |
| 256, confirmed reads off | all | 30.0 Hz | 7.7 | 13.7 | 19.9 | 0 | 1.11 | 2.75 |
| 512 | all | broke down | 47.4 | 15,002 | 19,100 | 17,298 | 2.39 | 7.12 |
| 512 | 32 | 30.0 Hz | 15.0 | 30.7 | 62.0 | 0 | 1.65 | 3.02 |
| 1,024 | 32 | 29.5 Hz, then 26.6 Hz on a repeat | 20.1 / 33.7 | 73.6 / 374 | 156 / 558 | 581 / 8,125 | 3.69 / 3.49 | 6.53 / 6.05 |
| 1,024, no inputs | 32 | 30.0 Hz | 12.5 | 20.6 | 27.4 | 0 | 0.95 | 2.68 |
| 1,536 | 32 | 21.4 Hz | 31.8 | 1,213 | 2,658 | 21,843 | 4.17 | 7.37 |
| 2,048 | 32 | 0.25 Hz (collapsed) | 5,142 | 11,076 | 11,136 | all | 3.83 | 6.78 |

No client was disconnected in any run.

### With delay and packet loss

128 players, all visible, 32 clients, in a private network namespace with `tc netem` on loopback. Delay and loss apply in each direction.

| Network | State age p50 | p99 | max | Arrival gaps over 100 ms (of 28,800) |
|---|---|---|---|---|
| 25 ms each way, no loss | 27.2 | 52.5 | 66.0 | 0 |
| 25 ms, 0.5% loss | 30.2 | 79.1 | 173 | 168 |
| 25 ms, 2% loss | 31.9 | 116 | 254 | 574 |
| 25 ms, 5% loss | 32.1 | 209 | 1,009 | 1,380 |

### Bandwidth

Loopback bytes divided by clients, so it includes both directions and TCP overhead.

- 128 visible, no compression: about 445 KB/s per client. That is 116 bytes per row update, twice the 56-byte row, which matches the whole-row delete-plus-insert format.
- 128 visible, Brotli (the SDK default): about 100 KB/s per client.
- 32 visible, no compression: about 117 KB/s per client.

## What the numbers say

- A SpacetimeDB tick loop held 30 Hz with low single-digit to 15 ms added latency up to 256 players all visible, and up to 512 players with 32 visible, on half of a desktop CPU.
- The limit met first was not the tick or the fan-out. It was input. One reducer call per client per tick is 30,720 transactions a second at 1,024 players; with inputs switched off the same 1,024-player run was clean at under one server core. Somewhere between 512 and 1,024 players this input design starts to delay the tick, and by 2,048 the tick stops.
- Confirmed reads cost about 6 ms at the median here (14.0 against 7.7 ms).
- Each lost packet produced a stall of more than 100 ms for the client that lost it: the count of long gaps tracks the loss rate (2.0% at 2% loss, 4.8% at 5%). Under the same loss a UDP state update that is lost costs one tick, a 67 ms gap, and nothing queues behind it.

## What this does not show

- **Simulation cost.** The tick does trivial arithmetic. Halo's per-tick collision, physics, projectiles and weapons would run in the same single-threaded reducer; nothing here measures that.
- **The 512 all-visible row** is not a clean server measurement: the load client used 7.1 of its 8 logical cores, so the collapse may be the harness. It is 6 million row updates a second and 820 MB/s on loopback either way.
- **1,024 and above** are close to the load client's limit too (6 to 7.4 of 8 logical cores), and the two 1,024 runs differ, so read the knee as "between 512 and 1,024 on this machine", not as a precise figure.
- **Real networks.** Loopback has a 64 KB MTU, so one tick's update is one packet. On the internet the same update is about ten packets uncompressed, so a given loss rate would hit more ticks than the loss table shows.
- **Clients on other machines**, the managed service, and any version other than 2.10.2.
- **Brotli at scale.** Brotli decompression in the Rust SDK cost about 2 ms of client CPU per message, which swamped the load client above a few dozen connections, so the scale runs use no compression. An early set of runs with Brotli left on showed a tick rate of 23 Hz at 128 players; that was the load client starving the server on a shared machine, not the server, and those runs are discarded.

## Rerun

`STDB_BIN=/path/to/spacetime-2.10.2 ./run.sh`. The script collects the steps that were run by hand for the table above; it has not itself been run end to end. The loss table needs `unshare -Urn` and `tc qdisc add dev lo root netem ...` around the same server and client.
