# Large-scale mode: spike results

Measured 2026-10-02. Three throwaway spikes were agreed before the design document for the Rust and SpacetimeDB large-scale mode. This note records what each one showed. All three passed.

All runs: SpacetimeDB Standalone v2.10.2, one desktop (AMD Ryzen 7 5800X, 8 cores), everything on loopback, the server pinned to 2 cores, the gateway to 2 and the load generator to 4. Game data is the user's own Xbox disc image; none of it is in the repo.

## Map facts (from the parser written for the spikes)

The Xbox map files are zlib-compressed after a 0x800-byte header. All 13 multiplayer maps parse, and their collision data passed every check: all indices in range, and a ray dropped from each of the 829 player starts hits ground directly below it.

| Map | Player starts | CTF starts per team | Bounds x × y (world units) | Collision data |
|---|---|---|---|---|
| beavercreek | 70 | 26 / 22 | 43 × 21 | 331 KB |
| bloodgulch | 71 | 17 / 21 | 126 × 145 | 666 KB |
| boardingaction | 73 | 27 / 25 | 135 × 195 | 480 KB |
| carousel | 63 | 18 / 18 | 32 × 32 | 156 KB |
| chillout | 58 | 16 / 17 | 23 × 21 | 192 KB |
| damnation | 67 | 19 / 18 | 28 × 32 | 516 KB |
| hangemhigh | 67 | 19 / 20 | 30 × 39 | 210 KB |
| longest | 71 | 20 / 23 | 33 × 16 | 136 KB |
| prisoner | 58 | 16 / 18 | 24 × 15 | 327 KB |
| putput | 59 | 16 / 16 | 56 × 36 | 625 KB |
| ratrace | 48 | none | 32 × 32 | 193 KB |
| sidewinder | 76 | 21 / 23 | 109 × 110 | 901 KB |
| wizard | 48 | 16 / 16 | 25 × 25 | 206 KB |

Consequence for the design: no map has more than 76 player starts, and team games have about 20 per side. Two teams of 250 will spawn in waves on every map.

Not independently checked: facing, team and game-type fields on starts; netgame flags, equipment and vehicle placements. In 4 of 26,000 random rays the BSP result differs from a plain polygon test; this is believed to be the engine's own behaviour for one-sided leaves and was not confirmed against the running engine.

## Spike 1: the module tick against real collision

**Question:** can a SpacetimeDB module validate 500 players' movement and simulate loose objects against Blood Gulch's real collision data inside a 33 ms tick?

**Method:** the module holds the collision export as one row. Each tick it casts two rays per player (the move must not pass through a surface; it must end on ground) and one per loose object (gravity and bounce), then writes every row. A native driver walks the players over the real terrain and sends all positions as one call per tick.

| Players | Loose objects | Collision | Rays per tick | Mean tick | Tick rate |
|---|---|---|---|---|---|
| 500 | 200 | none (baseline) | 0 | 0.96 ms | 30 Hz |
| 500 | 200 | map kept in module memory, reloaded from the table if absent | 1,144 | 1.52 ms | 30 Hz |
| 500 | 200 | map re-read and re-parsed from the table every tick | 1,138 | 2.43 ms | 30 Hz |
| 2,000 | 1,000 | none (baseline) | 0 | 3.36 ms | 30 Hz |
| 2,000 | 1,000 | map kept in module memory | 4,619 | 4.76 ms | 30 Hz |

"Mean tick" is the server's own metric for reducer plus subscription query time.

- A ray against the real BSP costs about 0.3 to 0.5 µs inside the module, against about 0.3 µs natively.
- Reading single rows costs about 0.3 µs each (200,000 primary-key finds in 61 ms).
- Re-reading the whole 668 KB map every tick costs under 1 ms, so the map does not need to rely on module memory surviving between ticks. Keeping it in memory as a cache, with the table as the source, is the faster and still safe form.

**Answer:** yes, with about 20 times headroom at 500 players. The module-holds-everything design stands, and the fallback of moving geometry into the gateway is not needed for this workload.

**Limits:** two rays per player is validation, not Halo's full biped physics. One ray per object is not Halo's object or vehicle physics. About 1.5% of moves were rejected because the walker and the validator use slightly different tolerances; nothing was tuned.

## Spike 2: the UDP gateway

**Question:** does a native gateway beside an unmodified SpacetimeDB carry 500 players' per-tick traffic over UDP, with input batched into one module call per tick, and does packet loss stay harmless?

**Method:** the gateway holds one SpacetimeDB connection on loopback. It collects UDP input from every player and makes one reducer call per tick. When a tick's rows arrive it sends each player the other players' states in datagrams of at most 1,200 bytes, using the distributed netcode's distance tiers (every tick within 25 world units, every 2nd within 60, every 3rd within 120, every 4th beyond). 500 UDP players walk the real Blood Gulch terrain from its spawn points. A unit state is 36 bytes.

| Network | State age p50 | p99 | max | Near-player updates late by one tick | Stalls over 100 ms |
|---|---|---|---|---|---|
| Clean | 9.6 ms | 17.9 | 23.2 | 0.00% | 0 |
| 2% loss (simulated in the client) | 9.6 ms | 18.0 | 23.9 | 2.00% | 0.021% |
| 5% loss (simulated in the client) | 9.4 ms | 17.4 | 23.3 | 5.02% | 0.135% |
| 25 ms each way and 2% loss on UDP (`tc netem`) | 38.1 ms | 44.2 | 47.3 | 1.99% | 0.021% |

- All 900 ticks reached every player in every run.
- CPU: the database used 0.09 of a core and the gateway 0.37.
- The database receives the tick's inputs as one transaction. The earlier benchmark's input bottleneck (one call per player per tick) is gone.
- The server-to-gateway hop adds 1.3 ms. The rest of the delay is the gateway's single send thread, which takes about 17 ms to send one tick to 500 players. It needs several send threads or batched sends before 500 is comfortable.
- A lost update costs one tick and nothing queues behind it. Over TCP, the earlier benchmark stalled 2% of arrivals for more than 100 ms at 2% loss; here it is 0.02%.

**Answer:** yes. The gateway design works at 500 players and removes both problems found earlier.

**New finding, and it is the main one:** bandwidth. With today's distance tiers each player receives about 265 unit states per tick, which is about 300 KB/s (2.4 Mbit/s) down per player and about 150 MB/s (1.2 Gbit/s) up from the server. The tiers were designed for up to 128 players and thin very little on one map with 500. The design must cut this, for example by visibility, by smaller or delta-encoded states, and by slower rates for far players.

**Limits:** players stay near the 71 spawn points for the length of a run, which is a guess at where 500 players would be. Everything is on one machine. The gateway has no rejoin, no reliable channel and no authentication. One early clean run showed a delay tail up to 436 ms while another job was compiling on the machine; it did not recur in four later runs.

## Spike 3: Rust state drawn by the C renderer

**Question:** can a Rust library linked into the existing C client own players' state while the existing renderer draws them, and what does it cost?

**Method:** in a scratch copy of the repo at `d1c7243c`, the same Rust walker used by the other spikes was built as a 32-bit static library (`i686-unknown-linux-gnu`) and linked into `build/linux/halo`. One new C file, `port/linux/game/rust_bridge.c`, is called from `game_tick` just before `objects_update`. On the first tick with a spawned local player it creates one ordinary multiplayer biped per Rust walker (`object_new` with `multiplayer_information[0].unit`). Every tick it steps the Rust walkers, then for each one calls `unit_control` (throttle, facing, in-combat animation state) and `object_set_position`. The game ran hidden on Blood Gulch with screenshots; the local player stands and watches. Debug build, no link-time or profile-guided optimisation, 640×480, RTX 3080.

| Rust-driven players, all in view | Rust step plus adapter, per tick | Frame rate, cap off |
|---|---|---|
| 0 | none | about 1,330 fps |
| 16 | 0.07 to 0.09 ms | not measured (120 with the default cap) |
| 150 | 0.44 to 0.60 ms | about 320 fps |
| 500 | 1.4 to 1.7 ms | about 80 fps |

- The players appear in the world with the correct model, weapon and running animation, walking the terrain. A screenshot of the 500-player run is in the scratch folder (`spikes/run/crowd-500.png`).
- The Rust library links into the 32-bit game with no changes to the build rules beyond one line: the library path and `-lgcc_s -lutil -lrt -lpthread -lm -ldl -lc`. It passes the port's link check.
- The boundary used only floats, 32-bit integers and out-pointers. Nothing was returned by value, because the C side is built with `-freg-struct-return` and `-malign-double`.
- The adapter is cheap: about 3 µs per player per tick.
- Drawing is the cost that grows. About 23 µs per visible player per frame in this build, so about 330 players in view at 120 fps and about 700 at 60 fps. This is the renderer's own cost and does not depend on Rust. An optimised build should do better; that was not measured.

**Answer:** yes. The adapter approach works and its own cost is small.

**What the design must settle, found here:**

- **The C unit code still runs.** The adapter gets its walk animation by handing the engine a throttle, so the engine's own biped update still animates and still runs physics. Each tick the engine moved a biped about 0.075 to 0.1 world units from where Rust put it (one walking step), and sometimes several units, because engine bipeds collide with each other and fall while the Rust walkers do not. Rust overwrites the position every tick, so the error does not grow, but Rust does not yet own everything that is drawn. The design must choose between suspending engine physics for these bipeds and moving animation selection into Rust.
- **Same results on both sides is unproven.** The client library is native 32-bit x86 and the server module is WebAssembly. The walker uses `sin` and `cos` from each platform's own maths library. Nothing here checked that the two produce identical values; the design should use one maths implementation on both.
- **These bipeds are not players.** They have no player record, so they show as unknown contacts on the motion sensor and have no name, team or score. The real adapter has to create players, not just bipeds.
- **Starting a game alone.** The engine will not start a network game with one machine; two source changes in the scratch copy relaxed that (`server_has_enough_machines` and `server_ok_to_countdown`). A second local instance could not find the host's game.

**Limits:** walking only; no weapons fire, damage, vehicles or camera work. Frame rate was read from the spacing of screenshots, which costs a little itself. One machine, one resolution.

## Follow-up test A: a bandwidth budget in the gateway

**Question:** does a fixed byte budget per player, filled by priority, bound the bandwidth found in spike 2, and what update rates does it leave?

**Method:** the spike 2 gateway with its send path replaced. Every other player builds up priority each tick: full weight within 10 world units, falling with the square of distance to a floor of 0.06, doubled when within 60 degrees of where the recipient faces. Each tick the highest priorities are sent until the budget is full, and a sent player's priority starts again. A unit state is packed into 16 bytes (16-bit position per axis, 8-bit velocity per axis, 16-bit yaw, 8-bit pitch). Four send threads. Same 500 players on the real Blood Gulch terrain, starting at its 71 spawn points.

| Budget per player | States per tick | Players under 10 wu | 10 to 25 wu | 25 to 60 wu | Over 60 wu | Server upload at 500 |
|---|---|---|---|---|---|---|
| 24 KB/s | 47 | 11.6 Hz | 5.7 Hz | 1.8 Hz | 1.3 Hz | 12 MB/s |
| 45 KB/s | 88 | 20.2 Hz | 10.5 Hz | 3.5 Hz | 2.5 Hz | 22 MB/s |
| 90 KB/s | 179 | 30.0 Hz | 20.9 Hz | 8.4 Hz | 6.2 Hz | 45 MB/s |
| Spike 2 tiers, for comparison | 265 (36-byte states) | 30 Hz | 30 Hz | 15 Hz | 10 Hz | 150 MB/s |

- The budget holds exactly: measured downstream was 24, 45 and 90 KB/s.
- State age on arrival fell to 2.6 ms at the median (p99 4.2 ms) from 9.6 ms, and the gateway's time to send one tick fell from 17 ms to 3.7 ms, with the gateway at 0.26 of a core. Fewer datagrams and four send threads both contribute; they were not separated.
- With these weights and this crowding, 45 KB/s is not enough to keep every player within 10 world units at the full 30 Hz; about 37 players were that close on average. 90 KB/s is.
- With 2% loss at 45 KB/s the mean rates barely move, but the longest wait for a far player rose from 1.2 s to 3.4 s. A sent player's priority starts again whether or not the datagram arrived, so a lost far update waits a whole cycle. The design needs either acknowledgements or a cap on staleness.

**Answer:** yes, a budget bounds the cost. The earlier estimate of 40 to 50 KB/s was too low for full-rate nearby players at this density; about 90 KB/s per player (0.7 Mbit/s), or 45 MB/s from the server at 500, did it here.

**Limits:** the weights are a first guess and were not tuned. Player positions are a guess (clustered at spawn points). What far players look like at 2 to 8 Hz with interpolation was not looked at, only counted.

## Follow-up test B: engine physics switched off for adapter-driven players

**Question:** if the engine's own physics is switched off for the players Rust drives, does Rust own their position exactly, and do they still animate?

**Method:** the spike 3 adapter, with `unit_scripting_suspended(biped, TRUE)` called on each biped it creates. That sets the engine's existing `_unit_suspended_bit`, which makes biped physics keep the position it was given.

| | Engine moved each player per tick | Animation state | Frame rate at 500 in view |
|---|---|---|---|
| Physics on (spike 3) | 0.09 to 0.1 wu on average, up to 20 | move-front, with occasional airborne and landing states | about 77 to 89 fps |
| Physics suspended | 0.0000 | move-front on every sampled player | about 73 to 105 fps |

- Rust's position is what is drawn, exactly, and the running animation still plays (checked in a screenshot of the 500-player run).
- No frame-rate gain: the engine still works out the physics and then discards it.
- Suspending also zeroes the engine's velocity for the player, so anything in the client that reads it, such as the motion sensor, shows nothing. Rust has to supply velocity to those.

**Answer:** yes. Rust can own movement while the C engine keeps choosing and playing animations.

**Limits:** running forward only. Jumping, crouching, falling, melee, vehicles and death were not tried, and those are where animation and gameplay are tied together.

## Where the spike code is

Outside the tree, as agreed, in the session scratch folder under `spikes/` (`halomap`, `gateway`, `collision`, `rustbridge`, and `port`, the patched copy of this repo). That folder is temporary and will not survive a reboot.
