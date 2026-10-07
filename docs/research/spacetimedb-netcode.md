# SpacetimeDB and the distributed netcode: can it raise the 128-player cap?

Research note, 2026-10-02. Repo state read: `halo-ce-universal` at `d1c7243c`.

## Summary

SpacetimeDB clients connect over WebSocket (TCP) only; there is no unreliable, unordered or UDP channel in the source at the current release (v2.10.2) or on master, and QUIC/WebTransport is an open, unscheduled request that the maintainers call "top of mind" without a date. Modules run sandboxed as WebAssembly (Rust, C#, C++) or on V8 (TypeScript), must keep all state in tables, and cannot call native code; there is no C client SDK. A module can run a fixed-interval scheduled reducer, and nothing in the scheduler forbids 33 ms, but missed ticks are skipped rather than caught up and no first-party figure exists for a 30 Hz simulation workload, for subscription fan-out, or for client count per database. Those facts rule out running this game's simulation inside a module without rewriting it, make a per-tick state relay strictly worse on the wire than the existing UDP path, and leave lobby/score/identity use open. None of the three shapes touches the actual source of the 128 cap, which is the engine's own `char` indices, 7-bit and 8-bit wire fields and fixed-size records; lifting it is an independent change to this codebase.

## How to read the citations

- **Halo repo**: paths relative to the repo root, with line numbers at `d1c7243c`.
- **`STDB:`** a path in `clockworklabs/SpacetimeDB` at master commit `0ba810e6360c93090b86310122ea2629b6113737` (2026-10-01, workspace version `2.11.0`, unreleased). URL form: `https://github.com/clockworklabs/SpacetimeDB/blob/0ba810e6360c93090b86310122ea2629b6113737/<path>`. Read from a shallow clone.
- **"also at v2.10.2"**: the same constant or absence was re-checked with `git grep` against the tag `v2.10.2` (commit `58d2a40718c59535fd5c267e836d473298eb84d5`, released 2026-09-29, marked "Latest" on <https://github.com/clockworklabs/SpacetimeDB/releases>).
- **Docs**: read from the repo's `docs/docs/` tree at the master commit (the source of <https://spacetimedb.com/docs>), not from the rendered site. Site paths are given from each page's `slug` front matter. I did not check that the deployed site matches master.
- **Blog / pricing**: fetched from spacetimedb.com on 2026-10-02 and read as raw page text.
- **[source]** = verified by reading code. **[docs]** = stated in first-party docs, release notes or posts, not checked against code. **[inference]** = my own reasoning or arithmetic from cited facts.

## Current netcode (verified)

Each believed point, checked against the code.

1. **Native C port, 30 Hz, a player's machine hosts and simulates everything: confirmed.**
   - `TICKS_PER_SECOND = 30` in `source/cseries/cseries.h:34`.
   - The host is authoritative and is itself a playing machine: `port/linux/NETCODE.md:24-26` ("Host authoritative"), and pings are "0 for the host's own players" (`port/linux/NETCODE.md:201-203`, `port/linux/game/network_distributed.h:170-173`). A search for `dedicated` in `source/networking`, `source/game`, `port/linux` and `port/windows` found no dedicated-server mode.
   - Rendering is already decoupled from the tick: frames are interpolated between the last two ticks at the display's refresh rate (`port/linux/game/render_interpolation.c:4-14`). Client frame rate is therefore a rendering matter, not a netcode one.

2. **The distributed netcode, its files and its reliable/unreliable split: confirmed.**
   - Described in `port/linux/NETCODE.md`; implemented in `port/linux/game/network_distributed.c` (3,847 lines), `network_objects.c` (2,615) and `network_damage.c` (2,121).
   - Message kinds and their delivery are listed in `port/linux/game/network_distributed.h:21-70`: unreliable for player prediction, unit states, statistics, inventories, object states, damage events, vehicle prediction, player inputs, relayed actions, pings; reliable for object changes, game state, objects-synchronized, client-ready, hit reports, pickups, client identity.
   - Unreliable messages for one machine in one tick are gathered into one or more batch datagrams of at most 1,200 bytes: `distributed_batch_add` and `distributed_batches_flush` (`network_distributed.c:851-906`), `DATAGRAM_MAXIMUM_SIZE = 1200` (`source/networking/network_connection.h:18`). Correction of detail: it is "as few datagrams as they fill", not strictly one per machine per tick (`port/linux/NETCODE.md:382-387`).
   - The unreliable channel is a UDP endpoint and the reliable channel is a TCP stream endpoint: `create_transport_endpoint(_transport_type_udp)` at `source/networking/network_connection.c:1177` and `:1469`, `create_transport_endpoint(_transport_type_tcp)` at `:1152`. `TCP_NODELAY` is set (`port/linux/src/posix_net.c:246`; `port/linux/NETCODE.md:365-369`). So the "reliable channel" is plain TCP with an outgoing queue and a 15 s write timeout (`network_connection.c:228-234`, `:272-276`), not a reliability layer over UDP.
   - Every message carries the sender's tick, and a late unreliable message of a kind is dropped (`port/linux/NETCODE.md:388-392`).

3. **Session limits and the origin of 128: confirmed, with additions.**
   - `HALO_PORT_MAXIMUM_NETWORK_PLAYERS 128` and `HALO_PORT_MAXIMUM_NETWORK_MACHINES 128` (`port/linux/include/halo_port_limits.h:21-22`). The header gives the reason: "player, machine and team indices are stored in signed chars (0..127 with NONE), and a finishing place in 7 bits" (`:11-13`).
   - The fields are declared plain `char`: `machine_index`, `team_index`, `player_list_index` in `struct network_player` (`source/game/players.h:76-79`); `machine_index` in `struct network_machine` (`source/networking/network_game_manager.h:29`). `maximum_players` was already widened to `byte` with the comment "128 does not fit a signed char" (`network_game_manager.h:51-53`).
   - Further ceilings just above 128 that the user's summary did not list:
     - The distributed messages carry a player index in one byte with `NO_PLAYER = 0xFF` (`network_distributed.h:96-97`), and a message's entry count in one byte (`struct distributed_message_header`, `:102-108`; `RELIABLE_ENTRIES` caps at 255, `:135-136`).
     - A message header's length is 12 bits; "the per-tick update of 128 players is 3,857" of 4,095 bytes (`halo_port_limits.h:76-78`).
     - The host `select`s on one socket per machine with `HALO_PORT_FD_SETSIZE 256` (`halo_port_limits.h:24-27`).
     - `struct network_game` is a fixed-layout record sized by the limits (13,092 bytes at 128, sent in 4 fragments; `halo_port_limits.h:33-50`, `:84-87`).

4. **The send seam: partly wrong.**
   - `distributed_send` is the send seam: declared `network_distributed.h:142`, defined `network_distributed.c:936-962`, with `distributed_send_to_machine` and `distributed_send_to_machine_reliably` beside it (`.h:145-147`). Below it sit four transport functions in the engine's own networking code: `network_distributed_server_send_to_machine`, `..._reliably`, `..._send_to_all_reliably` (`source/networking/network_server_message_handler.c:821-878`) and `network_distributed_client_send`, `..._reliably` (`source/networking/network_game_globals.c:265-293`). Those five functions are the narrowest point at which a different transport could be substituted.
   - There are no `distributed_message_*` handler functions. `_distributed_message_*` are enum constants for message types (`network_distributed.h:21-70`). The receive seam is a single dispatcher, `network_distributed_handle_message(machine_index, message, size)` (`network_distributed.c:3607`), which is not declared in `network_distributed.h` but ad hoc in its two callers (`source/networking/network_server_message_handler.c:269`, `source/networking/network_client_message_handler.c:205`). It fans out to static `distributed_handle_*` functions and to `network_objects_handle_*` / `network_damage_handle_*` (`network_distributed.h:206-210`, `:232-233`).

Facts needed to judge fit:

- **What a client sends per tick** (`network_distributed_tick`, `network_distributed.c:3187-3192`): its local players' input (`struct distributed_player_input`, 52 bytes each, carrying the current action plus the buttons of the three previous ticks; `:317-333`, `source/game/player_queues_new.h:69`), its own players' predicted unit states (`:1646-1672`), the vehicle it drives, and any hit reports (reliable, 108 bytes each; `network_damage.c:276-295`).
- **What the host sends each client per tick**: every player's unit state and relayed input, rate-limited per client by distance, visibility and aim (every tick within 25 world units, every 2nd within 60, every 3rd within 120, every 4th beyond, every 6th when hidden or dead; `network_distributed.c:257-275`, `port/linux/NETCODE.md:410-431`), plus nearby moving objects (44-byte `struct distributed_object_state`, `network_objects.c:255-268`).
- **Stated sizes**: a unit state is 35 bytes for a player on foot, a relayed input 14 to 20 bytes (`port/linux/NETCODE.md:395-409`); the internet-play tunnel adds 36 bytes per datagram (`:384-385`).
- **Bandwidth budget**: none is stated anywhere in `NETCODE.md` or the three source files (searched for `bandwidth`, `per second`, `bytes a second`). [inference] From the stated sizes, the worst case with no interest reduction is 127 x (35 + 14..20) = 6.2 to 7.0 KB per client per tick, about 190 to 210 KB/s per client, and about 24 to 27 MB/s of host upload at 128 players. The interest rules above exist to keep the real figure well below that; the repo records no measurement.
- **Loss tolerance is designed around unreliable delivery**: input is repeated across four datagrams so "nothing waits for a lost one to be sent again", and the note records that the game's own reliable per-tick update was dropped as an input carrier because it is "held up by any loss" (`port/linux/NETCODE.md:370-381`).

## Findings

### 1. Transport

- **Clients connect by WebSocket over TCP.** The only client endpoint for live data is `GET /v1/database/:name_or_identity/subscribe`, an HTTP upgrade to WebSocket (`STDB: docs/docs/00300-resources/00200-reference/00200-http-api/00300-database.md:363-382`). The server implements it with `tokio-tungstenite` (`STDB: crates/client-api/src/util/websocket.rs:13-19`, `STDB: Cargo.toml:336`). [source]
- **Subprotocols**: `v1.json.spacetimedb`, `v1.bsatn.spacetimedb`, `v2.bsatn.spacetimedb`, `v3.bsatn.spacetimedb` (`STDB: crates/client-api-messages/src/websocket/v1.rs:23-24`, `v2.rs:9`, `v3.rs:13`; negotiated in `STDB: crates/client-api/src/routes/subscribe.rs:65-71`, `:200-222`; also at v2.10.2). v3 reuses the v2 message schema and only lets several messages share one WebSocket frame (`v3.rs:1-11`). [source]
- **No unreliable, unordered or UDP channel is shipped.** Evidence of absence: a case-insensitive whole-word search for `udp`, `quic`, `quinn`, `webtransport`, `webrtc`, `enet`, `kcp` across `crates/`, `sdks/`, `docs/docs/`, `modules/`, `templates/`, `README.md` and `Cargo.toml` at master returned one hit, a `443:443/udp` port mapping for a Caddy reverse proxy in a self-hosting example (`STDB: docs/docs/00300-resources/00100-how-to/00700-self-hosted-key-rotation.md:169`). A search for `UdpSocket` returned nothing. The same search over `crates/` and `sdks/` at v2.10.2 returned nothing. Matches for "unordered" are internal Rust channel names for WebSocket control frames, not a delivery mode (`subscribe.rs:617-659`). [source]
- **Roadmap**: QUIC is requested in issue #2619 (open since 2025-04-16), <https://github.com/clockworklabs/SpacetimeDB/issues/2619>. Maintainer statements there, in order:
  - bfops (collaborator), 2025-04-16: "We've discussed adding support for other networking protocols, but it hasn't been a priority so far."
  - cloutiertyler (Clockwork Labs founder), 2026-04-09: "I have created an internal design for QUIC transport, but we're not sure on the prioritization of it internally."
  - cloutiertyler, 2026-09-09: "This is definitely top of mind! We are always following the feature list."
  - On the duplicate #2958 (closed 2025-07-29), cloutiertyler, 2025-07-21: "This is on our roadmap." <https://github.com/clockworklabs/SpacetimeDB/issues/2958>
  - "QUIC / WebTransport support" is listed on the first-party feature-voting page <https://spacetimedb.com/features> as #5206, requested 2026-06-03 by cloutiertyler.
  - No date, milestone or branch was found. The one dated roadmap that exists ("Spacetime Continuum", 2026-10-31) covers inter-database communication and tiered storage, not transport (<https://spacetimedb.com/blog/how-does-spacetime-scale>).
- **Delivery is held for durability by default in 2.x.** `DEFAULT_CONFIRMED_READS = true` (`STDB: crates/client-api/src/lib.rs:35`; also at v2.10.2), applied to v2/v3 connections and not v1 (`subscribe.rs:136-144`). The migration guide says updates "may arrive a few milliseconds later, as the server waits for durability confirmation" and that a game may opt out with `withConfirmedReads(false)`; the Unreal SDK has no opt-out method (`STDB: docs/docs/00300-resources/00100-how-to/00600-migrating-to-2.0.md:1723-1778`, site `/docs/upgrade`). [docs + source]
- **Slow clients are disconnected, not skipped.** A client whose outgoing queue reaches 16,384 messages is kicked (`STDB: crates/core/src/client/client_connection.rs:457-466`, `:819`; also at v2.10.2). Incoming WebSocket messages are capped at 32 MiB (`subscribe.rs:269`). [source]
- **No in-band ping**: issue #5926 (open, 2026-09-11, filed by a contributor) proposes adding one and records that BitCraft measures latency with ICMP, HTTP and reducer timings because the protocol has none. <https://github.com/clockworklabs/SpacetimeDB/issues/5926>

### 2. Modules

- **Languages**: Rust, C#, TypeScript and C++ (`STDB: docs/docs/00100-intro/00100-getting-started/00300-language-support.md`, site `/docs/intro/language-support`). TypeScript and C++ modules are "new in 2.0" (`STDB: docs/docs/00100-intro/00100-getting-started/00500-faq.md`, site `/docs/intro/faq`). [docs]
- **Runtime**: Rust, C# and C++ compile to WebAssembly and run under Wasmtime; TypeScript runs on V8 (language-support doc above; `STDB: crates/core/src/host/wasmtime/mod.rs:90-96`, `STDB: crates/core/src/host/v8/`). The ABI is the C ABI on `wasm32` (`STDB: docs/docs/00300-resources/00200-reference/00300-internals/00100-module-abi-reference.md:22`). C++ modules build with Emscripten (`STDB: crates/bindings-cpp/README.md`, "Prerequisites"). [source + docs]
- **Native C code**: a module cannot call native code or link a native library. Reducers "cannot interact with the outside world": no network requests, no file system access, no system calls, only database operations (`STDB: docs/docs/00200-core-concepts/00200-functions/00300-reducers/00300-reducers.md:509-518`). Procedures add outbound HTTP only (`STDB: docs/docs/00200-core-concepts/00200-functions/00400-procedures.md:581-588`). [docs] C source can be compiled to WebAssembly and linked into a C++ or Rust module; that is how the C++ bindings themselves work, but I found no first-party statement about linking third-party C libraries. [inference]
- **State must live in tables.** "Relying on global variables, static variables, or module-level state to persist across reducer calls is **undefined behavior**"; reasons given include that SpacetimeDB "may run each reducer in a fresh WASM or JS instance" and may re-execute a reducer (`reducers.md:520-532`). The founder's scaling post adds: "A reducer cannot perform I/O, read clocks, or generate randomness, and every data access goes through the reducer context" (<https://spacetimedb.com/blog/how-does-spacetime-scale>). [docs]
- **Execution model**: "the execution model for Spacetime databases is single-threaded by design"; "each database is a single-threaded actor" (same post). [docs]
- **Per-reducer execution limit**: the default budget is `DEFAULT_BUDGET = PER_EXECUTION_SEC * 60`, documented in the source as "Roughly 1 minute of runtime", measured in Wasmtime fuel at an assumed 2x10^9 fuel per second (`STDB: crates/client-api-messages/src/energy.rs:133-142`; also at v2.10.2). Standalone always uses this default (`STDB: crates/core/src/energy.rs:27-34`). Wall-clock time is not enforced for WebAssembly: the epoch-interrupt callback logs "Wasm has been running for ..." about once a second and resumes (`STDB: crates/core/src/host/wasmtime/wasmtime_module.rs:354-362`, `:910-912`). For V8 the timeout thread is commented out at the commit read (`STDB: crates/core/src/host/v8/budget.rs:30-38`). What budget Maincloud applies is not in the public repo: not found. [source]
- **Memory limit**: no Wasmtime resource limiter is configured (searched `crates/core/src` for `limiter`, `ResourceLimiter`, `StoreLimits`: no hits), so the only ceiling I could establish for WebAssembly modules is the 4 GiB address space of `wasm32`. [inference from the `wasm32` ABI] V8 modules have a configurable heap limit that defaults to 1 GiB (`STDB: crates/core/src/config.rs:372-375`; also at v2.10.2). Table data is separately bounded by host RAM: "the practical limit is the available RAM on the host" (FAQ). Maincloud-specific memory limits: not found.

### 3. Client SDKs

- **Shipped SDKs**: Rust, C# (including Unity and a Godot project file), TypeScript, and Unreal Engine C++ (`STDB: sdks/` contains `csharp`, `rust`, `typescript`, `unreal`; also at v2.10.2; language-support doc). Client codegen targets are exactly `Csharp`, `TypeScript`, `Rust`, `UnrealCpp` (`STDB: crates/cli/src/subcommands/generate.rs:701-712`). [source]
- **No C SDK, and no standalone C++ SDK.** Issue #5238 (open, 2026-06-05) asks for a standalone C++ client; a contributor replies that "the pure c++ sdk got delayed" and "they still plan to make a c++ sdk but there is more pressing stuff". That reply is from a community contributor, not a Clockwork Labs employee as far as I can tell. <https://github.com/clockworklabs/SpacetimeDB/issues/5238>
- **Rust SDK behind an FFI**: nothing is shipped. A search of `sdks/rust` for `extern "C"`, `no_mangle`, `cbindgen` found none; `crate-type = ["cdylib", ...]` appears only in test clients. The SDK does expose a polling loop suited to a game frame (`advance_one_message`, `frame_tick`, `run_threaded`; `STDB: sdks/rust/src/db_connection.rs:561-665`). [source] A C-callable shim would be a small Rust crate written for this project around the generated bindings; that is feasible in general Rust terms but is not something the project documents or supports. [inference]
- **Wire format for a hand-written client**:
  - Message schema: the Rust definitions in `STDB: crates/client-api-messages/src/websocket/v2.rs` are the reference; the crate's README marks it "Unstable Crate ... may change without notice". JSON dumps of the schema are checked in (`STDB: crates/client-api-messages/ws_schema.json`, `ws_schema-2.json`), with regeneration steps in `DEVELOP.md`.
  - Encoding: BSATN, documented at `STDB: docs/docs/00300-resources/00200-reference/00300-internals/00300-bsatn.md`.
  - The HTTP reference page for `/subscribe` still names only the v1 subprotocols (`00300-database.md:373-382`) although the server and all SDKs use v2 or v3 (`STDB: sdks/rust/src/websocket.rs:250`, `sdks/csharp/src/SpacetimeDBClient.cs:235`, `sdks/unreal/.../Websocket.cpp:52`, `crates/bindings-typescript/src/sdk/websocket_protocols.ts:3-4`). So prose documentation of the current protocol is incomplete; the source is the documentation. [source]
  - Server messages may arrive Brotli- or gzip-compressed with a one-byte tag (`STDB: crates/client-api-messages/src/websocket/common.rs:40-54`); a hand-written client must handle or disable that.

### 4. Scheduled work

- **Mechanism**: a schedule table with a `ScheduleAt` column triggers a reducer or procedure; `ScheduleAt::Interval` repeats. The docs' own example comments a 50 ms interval as "Game tick" (`STDB: docs/docs/00200-core-concepts/00300-tables/00500-schedule-tables.md`, site `/docs/tables/schedule-tables`). [docs]
- **Minimum interval**: none is documented and none is enforced. The scheduler takes intervals in microseconds and only bounds the maximum delay (about 2.18 years); the only special case is an interval of zero (`STDB: crates/core/src/host/scheduler.rs:185-188`, `:808-830`). [source] Timing is driven by a `tokio_util` `DelayQueue` (`scheduler.rs:31`, `:93`); its effective granularity is that of Tokio's timer, which I did not verify from a first-party SpacetimeDB source: not found. A 33.3 ms interval is therefore permitted; how precisely it fires is unmeasured.
- **Missed ticks are skipped, not caught up.** "If the database is busy or offline long enough to miss one or more interval ticks, SpacetimeDB schedules the next future tick rather than running missed ticks back-to-back" (schedule-tables doc; implemented in `next_interval_tick_after`, `scheduler.rs:808-830`; also at v2.10.2). A fixed-step simulation would have to compute its own step count from `ctx.timestamp`. [docs + source]
- **Lateness is treated as normal above 30 ms.** `SCHEDULED_FUNCTION_DELAY_WARNING_THRESHOLD = 30 ms` (`scheduler.rs:191`; also at v2.10.2), logged at trace level. The threshold was 50 ms and a warning when introduced in v2.8.0 and was lowered to trace in v2.10.2 because of "repetitive warning-level log messages" for functions that overrun their interval (release notes for v2.8.0 and v2.10.2, <https://github.com/clockworklabs/SpacetimeDB/releases>). The warning threshold is about one 30 Hz tick. [source + docs]
- **A scheduled reducer is an ordinary transaction**: it runs on the database's single thread, is written to the commit log, and triggers subscription evaluation like any other (items 2 and 5). [inference from those items]
- **Documented or measured cost of a tick**: not found. No first-party figure for scheduler jitter, per-tick overhead, or a 20/30/60 Hz game loop. One community data point appears in a first-party issue thread: a contributor states "My game runs on a 50ms tick rate" (#5445, <https://github.com/clockworklabs/SpacetimeDB/issues/5445>); it carries no measurements.

### 5. Subscriptions

- **Shape**: a client subscribes to SQL queries (or typed query-builder equivalents), receives all matching rows once (`SubscribeApplied`), then receives inserts and deletes as transactions commit (`STDB: docs/docs/00200-core-concepts/00400-subscriptions.md`, site `/docs/clients/subscriptions`; message types in `websocket/v2.rs`). [docs + source]
- **Query restrictions**: `SELECT * FROM table [WHERE ...]` only; whole rows, no column projection; at most a two-table join, with indexes required on both join columns; "Arithmetic expressions are not supported" in `WHERE` (`STDB: docs/docs/00300-resources/00200-reference/00400-sql-reference.md:14-141`). [docs]
- **Spatial filtering**: possible only as comparisons on stored columns, for example a precomputed chunk or cell id, or a box on `x`/`y` columns. A radius test needs arithmetic and so cannot be a subscription predicate. The docs steer toward region-keyed queries shared by many clients ("entities in region X" rather than "entities near me"; `STDB: docs/docs/00200-core-concepts/00200-functions/00500-views.md`, site `/docs/functions/views`). The repo's own perf module models this with a chunk-indexed location table (`STDB: modules/perf-test/src/lib.rs:16-73`). [docs]
- **Per-client filtering**: three mechanisms.
  - A `WHERE` on an identity column, as in the first-party video demo's `audio_frame_event.where(r => r.to.eq(identity))` (<https://spacetimedb.com/blog/video-conferencing-over-a-database-with-spacetimedb>).
  - Views taking `ViewContext`, which see the caller's identity; the docs warn "Per-user views require separate computation for each subscriber. With 1,000 connected users, that's 1,000 separate view computations" (views doc).
  - Row-level security with `:sender`, marked "experimental, unstable ... Use Views Instead" (`STDB: docs/docs/00300-resources/00100-how-to/00400-row-level-security.md`, site `/docs/how-to/rls`).
- **Batching and delivery**: "Each database transaction ... generates exactly zero or one update message sent to clients", in commit order; updates for all of a client's subscription sets are bundled into one `TransactionUpdate` (`STDB: docs/docs/00200-core-concepts/00400-subscriptions/00200-subscription-semantics.md`, site `/docs/clients/subscriptions/semantics`). A client unaffected by a transaction receives nothing (`v2.rs`, `TransactionUpdate` doc comment). Since v2.2.0/v2.3.0 the v3 protocol may coalesce several such messages into one WebSocket frame (release notes; `v3.rs:1-11`). There is no per-client send rate, priority or "latest wins" mode: every committed change to a subscribed row is delivered, in order. [docs + source]
- **An update is a whole row, sent as a delete plus an insert.** `PersistentTableRows { inserts, deletes }`; the source notes "we may add a variant for in-place updates of rows" in future (`v2.rs`, `TableUpdateRows`). There is no field-level delta. [source]
- **Event tables** are the mechanism for transient per-tick data: rows are broadcast to subscribers on commit and never stored in table state or the client cache, but "the inserts are still recorded in the commitlog" (`STDB: docs/docs/00200-core-concepts/00300-tables/00550-event-tables.md`, site `/docs/tables/event-tables`). They require the v2 protocol or later. [docs]
- **Reducer arguments are not broadcast in 2.x** (they were in 1.x through reducer callbacks), so client-to-client relay must go through a table or event table (migration guide, "Reducer callbacks removed"). [docs]

### 6. Scale numbers

What is published:

- **Transaction throughput**: 279,024 TPS (p50 8 ms, p99 12 ms) uncontended and 303,919 TPS (p50 7 ms, p99 11 ms) contended, for a two-row account-transfer reducer, 64 clients each pipelining up to 40 requests, client and server on the same machine, single-node Standalone (`STDB: templates/keynote-2/README.md`, "Results Summary" and "Machine Topology"; discussed in <https://spacetimedb.com/blog/benchmarking>, 2026-05-14). The post says latency is bounded by durable writes: "There's nothing magical we can do to reduce latency as dramatically as we can increase throughput if we want to provide the same durability guarantees". Replicated cloud databases reach "roughly 300k TPS for the benchmark transactions" with "somewhat increased latency" (<https://spacetimedb.com/blog/how-does-spacetime-scale>; benchmarking post). [docs]
- **Concurrent clients**: no number. The strongest statements are "SpacetimeDB is capable of connecting to many thousands of clients simultaneously with no modifications" (benchmarking post, Claim 9) and that BitCraft is "synchronized to thousands of players in real-time" (`STDB: README.md:85`). The docs use "1000 concurrent players updating positions at 60Hz" as an illustration of why to split tables, with no measurement attached (`STDB: docs/docs/00200-core-concepts/00300-tables.md:89`). [docs]
- **Subscription fan-out, updates delivered per second, end-to-end latency to a subscriber**: not found. The published benchmark measures reducer round trips, not delivery to third-party subscribers. The repo's `modules/keynote-benchmarks` updates 1,000,000 position rows in one reducer but its README publishes no result.
- **A horizontal-scaling step for reads is not shipped**: "Scale each database's networking horizontally (read replicas)" is "Planned" in the six-step table of the scaling post; today subscriptions are evaluated on the node hosting the database.

BitCraft:

- The whole backend is SpacetimeDB modules (`STDB: README.md:85`; <https://bitcraftonline.com/news/spacetimedb-and-bitcraft>, 2023-08-28).
- It is not one database. "BitCraft actually [is] implemented as a set of many SpacetimeDB databases which handle a spatial partition in the world" (BitCraft post). "A single root database maintains global data, while region databases handle different parts of the world and different groups of players" (scaling post, 2026-09-03).
- Players per region database, total concurrent players, tick rate and latency: not found in any first-party source I read.
- How the workload differs from a 30 Hz shooter, from the published server source (`clockworklabs/BitCraftPublic` at `998349436a903512410842ad6a588215b93ad040`, pinned to `spacetimedb = "=2.10.0"`): [source]
  - Movement is not simulated per tick on the server. The client submits a move segment (`origin`, `destination`, `duration`, a client timestamp) and the `player_move` reducer validates and stores it (`BitCraftServer/packages/game/src/messages/action_request.rs:14-21`, `.../game/handlers/player/player_move.rs:14-80`).
  - Server-side periodic work runs as "agents" on schedule tables. The intervals hard-coded in the source are 1 s (duel), 5 s (region population), 10 minutes, 1 hour and 1 day; the rest are read from parameter tables whose values are not in the repo (`BitCraftServer/packages/game/src/agents/`). I found no fixed world tick.
  - So BitCraft is event-driven with second-scale timers and client-side interpolation of validated moves; a shooter that advances and replicates every player every 33 ms is a different load profile, and BitCraft's existence is not evidence about it either way. [inference]

### 7. Hosting

- **Standalone (self-hosted)**: "the single-node version available on GitHub" (scaling post). Started with `spacetime start` or the `clockworklabs/spacetime` Docker image (FAQ; `STDB: docs/docs/00300-resources/00100-how-to/00100-deploy/00200-self-hosting.md`, site `/docs/how-to/deploy/self-hosting`, which fronts it with Nginx for TLS). Tunables are in `config.toml`: commit log, WebSocket ping/idle/queue, outbound HTTP (`STDB: docs/docs/00300-resources/00200-reference/00100-cli-reference/00200-standalone-config.md`). No energy accounting: the null energy monitor "records nothing and always returns the default budget" (`STDB: crates/core/src/energy.rs:27-34`). No replication. [docs + source]
- **Maincloud / SpacetimeDB Cloud (managed)**: "the proprietary, clusterized version" with state-machine replication (scaling post). Billed in energy (TeV). From <https://spacetimedb.com/pricing> on 2026-10-02:
  - Free: 2,500 TeV per month, described as about 3,000,000 function calls, 12.5 GB egress, 1 GB table storage.
  - Pro, $25 per month: 100,000 TeV, about 120,000,000 function calls, 500 GB egress, 40 GB storage; "then 2,592 TeV / $ thereafter"; automatic replication and backups.
  - Team, $250 per month: 250,000 TeV, about 300,000,000 function calls; "Ability to reserve dedicated nodes".
  - Enterprise: "Custom on-prem & cloud deployment options (BYO Cloud)".
- **Limits that differ**: the only ones I could establish are the energy quota and its pricing (managed only), replication and backups (managed only), and "resource limits depend on your plan" on Maincloud versus hardware you control when self-hosting (FAQ). Concrete Maincloud limits on connections per database, memory per database, reducer budget or message rate: not found.
- [inference] A 30 Hz scheduled reducer is 2.6 million calls per day by itself. 128 clients each calling one input reducer per tick is 3,840 calls per second, about 330 million per day. Against the pricing page's own "function calls" equivalence that exceeds the Pro tier's monthly allowance in under a day. The equivalence is approximate and per-call energy depends on the work done, so this is an order-of-magnitude indication only.
- **Match-based games**: the FAQ's recommended pattern is "an external orchestration service that creates and destroys SpacetimeDB databases for each room or match". [docs]
- **Licence**: Business Source License 1.1, converting to AGPL v3 with a linking exception (FAQ; `STDB: LICENSE.txt`). The additional-use grant terms were not analysed here.

## Version-dependent items

| Claim | Applies to | 1.x versus 2.x and later |
| --- | --- | --- |
| Current release | v2.10.2, 2026-09-29; master is 2.11.0 (unreleased) | Earliest 2.0.x release on GitHub is v2.0.1, 2026-02-20; v1.12.0 was 2026-02-04; v1.0.0 was 2025-03-03 |
| WebSocket-only transport | every version read (v2.10.2, master) | Unchanged from 1.x as far as the v1 protocol definitions show; QUIC is unscheduled |
| Protocol `v2.bsatn` | 2.0 and later | 1.x used `v1.bsatn` / `v1.json`; the server still accepts v1 |
| Protocol `v3.bsatn` (frame batching) | introduced v2.2.0, server-side batching v2.3.0 | Only the TypeScript SDK requests v3 at master; Rust, C# and Unreal request v2 |
| Confirmed reads on by default | 2.0 and later, v2/v3 connections | Off by default in 1.x and on v1 connections |
| Event tables | 2.0 and later, v2 protocol or later | 1.x used reducer callbacks, which broadcast reducer arguments; removed in 2.0 |
| TypeScript and C++ modules | 2.0 and later (C++ bindings brought up to 2.0 APIs in v2.1.0) | 1.x: Rust and C# only |
| Procedures and outbound HTTP without the `unstable` feature | v2.5.0 and later | Unstable before that |
| Scheduled-function delay tracking | warning at 50 ms from v2.8.0; trace at 30 ms from v2.10.2 | Absent before v2.8.0 |
| Interval schedules skip missed ticks | "the current implementation" per the docs at master; present in v2.10.2 source | Behaviour in 1.x not checked |
| Row-level security | experimental in every version read | Docs now say to use views instead |
| Tiered storage and async inter-database calls | announced for 2026-10-31, not shipped as of this note | n/a |

The documentation tree carries one frozen snapshot, `1.12.0` (`STDB: docs/versions.json`); everything else cited is the unversioned tree at master.

## Closing assessment

This section is judgement built on the findings above. It is kept apart from them on purpose.

### (a) SpacetimeDB as the authoritative game server, simulation inside a module

**Ruled out for this codebase as a port; possible only as a rewrite of the simulation.** This verdict is forced by documented facts, not by missing numbers.

- The simulation is a native C engine whose state lives in process memory. A module may not rely on memory persisting between reducer calls; all state must be table rows (finding 2). Moving the engine's object, player and physics state into tables is a rewrite of the game, not a netcode change.
- A module cannot call native code (finding 2). The engine could in principle be compiled to `wasm32`, but the state rule above still applies, and a database is single-threaded with reducers that cannot read clocks or do I/O (finding 2).
- Every tick would be a durable transaction with whole-row updates fanned out over TCP (findings 1, 5).
- Not forced, and unknown: whether a module could physically run a 128-plus-player tick in 33 ms. No first-party number exists for that (findings 4, 6). The verdict does not depend on it.

### (b) SpacetimeDB as a state relay between the existing host and its clients

**Not ruled out as something that would function. Ruled out as an improvement to per-tick delivery by a hard fact. Whether it would be tolerable is a judgement call on missing numbers.**

- Hard fact: the only transport is WebSocket over TCP, with ordered delivery of every committed change and no drop-stale mode (findings 1, 5). The existing netcode sends per-tick state as UDP datagrams specifically so that loss never stalls newer state, and its own notes record removing input from the reliable channel for that reason (verified context, last bullet). A relay would put that traffic back behind TCP head-of-line blocking, add a hop through the server, and by default hold each update for durability (finding 1).
- Hard fact: there is no C SDK, so the host and every client would need either a project-maintained Rust FFI shim or a hand-written WebSocket and BSATN client against an unstable, source-documented protocol (finding 3).
- Hard fact: the per-client interest management the host does today (distance, visibility and aim tiers) has no equivalent on the server side; it would have to become per-recipient rows or region-keyed subscriptions (finding 5).
- What a relay could genuinely offer: the host would upload each tick once and the server would fan it out, which addresses host upstream bandwidth. By my arithmetic that is the resource that grows fastest with player count (verified context). It would not reduce host CPU, since the host still simulates everything.
- Missing numbers: no published fan-out rate, subscriber latency or client-per-database figure, and no measurement in this repo of actual host bandwidth at 128 (finding 6; verified context). So "it works but feels worse under loss" is a well-grounded expectation, and "it would or would not hold 200 players at 30 Hz" is not answerable from primary sources. A measurement on a self-hosted Standalone instance with confirmed reads off would be needed to say more.
- On the managed service the call volume alone would be costly (finding 7); self-hosting avoids that but then someone runs a server, which the current host-on-a-player's-machine model does not need.

### (c) SpacetimeDB only for slow-changing state, per-tick traffic left on UDP

**Allowed by every finding; nothing rules it out.** Lobby listings, scores, identity and bans are low-rate, reliable, ordered data, which is what the product is built for (findings 5, 6, 7), and the match-per-database pattern is the documented one (finding 7).

- Costs that are facts: no C SDK (finding 3); a hosted or self-hosted service becomes a dependency of a game that today needs none; BSL licence (finding 7).
- Whether it is worth it compared with what the port already has for discovery and identity is a product judgement outside this note. It has no bearing on the player cap or on per-tick performance.

### Does SpacetimeDB address the source of the 128 cap?

**No. It is an independent change.** The cap comes from this engine's data layout: `char` player, machine and team indices, a 7-bit finishing place, fixed-size `struct network_game` arrays, a one-byte player index with `0xFF` as none and a one-byte entry count in the distributed messages, a 12-bit message length that the 128-player update already nearly fills, and a 256-descriptor `select` set (verified context, point 3). Shapes (b) and (c) leave all of that in place. Shape (a) would remove it only as a side effect of rewriting the simulation with a new schema. Conversely, widening those fields is possible without SpacetimeDB.

What then limits a session above 128 is host CPU for the simulation and host upstream bandwidth for fan-out. Only (b) bears on the second, and only with the transport cost described above. The 60 to 120 fps target is a client rendering property already decoupled from the tick by interpolation (verified context, point 1); none of the three shapes changes it, apart from whatever CPU a client spends decoding updates.

## Measured locally (added after the assessment)

The missing numbers named under (a) and (b) were measured on 2026-10-02 against Standalone v2.10.2; the method, tables and limits are in `spacetimedb-bench/RESULTS.md`. In short, on one desktop over loopback:

- A 30 Hz scheduled reducer rewriting one row per player held its rate with 4 to 15 ms median added latency up to 256 players with everyone visible, and up to 512 players with 32 visible each. So the "unknown" in (a) is answered for a tick that does trivial work; it says nothing about the cost of the real simulation.
- The first limit reached was input sent as one reducer call per client per tick. The tick slowed somewhere between 512 and 1,024 players and stopped at 2,048.
- With 25 ms of delay each way and 2% packet loss, about 2% of arrivals stalled for more than 100 ms. That is the head-of-line cost expected under (b), now with a size.

These results change no verdict above. They do replace "not answerable" in (b) with a figure, and they show that a game written for SpacetimeDB could run a 30 Hz loop at several hundred players on a good network.

## Searched for and not found

- Any UDP, QUIC, WebTransport, WebRTC, ENet or KCP code in SpacetimeDB at master or v2.10.2 (whole-word search of `crates/`, `sdks/`, `docs/docs/`, `modules/`, `templates/`, `README.md`, `Cargo.toml`). One unrelated hit, a reverse-proxy port mapping.
- A date, milestone or branch for QUIC or WebTransport (issues #2619, #2958, #5206, #5305; release notes v2.0.1 through v2.10.2; the scaling post's roadmap table).
- A C client SDK, a standalone C++ client SDK, or a C API on the Rust SDK (`sdks/`, codegen targets, issue #5238).
- A prose specification of the v2/v3 WebSocket protocol in the docs (only the v1 names appear in the HTTP reference).
- A documented minimum schedule interval, scheduler jitter figures, or any measured cost of a fixed-rate tick.
- A Wasmtime memory limit or a wall-clock reducer timeout in the host source; Maincloud's reducer budget and memory limits.
- Published figures for concurrent clients per database, subscription fan-out, updates delivered per second, or subscriber-side latency.
- BitCraft's concurrent player counts, players per region database, tick rate or latency in any first-party source; the values of BitCraft's parameterised agent intervals.
- Concrete Maincloud limits on connections, memory or message rate per database.
- A dedicated-server mode in this repo, and any stated bandwidth budget or measured bandwidth for the distributed netcode.
- Tokio timer granularity as it affects scheduled reducers, from a first-party SpacetimeDB source.
