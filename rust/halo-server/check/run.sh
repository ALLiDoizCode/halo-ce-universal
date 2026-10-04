#!/usr/bin/env bash
# The 500-player Team Slayer check (issue #18): a whole server on one machine,
# a full match on Blood Gulch with simulated players, measured.
#
#   HALO_STDB_BIN=~/.local/share/spacetimedb-2.10.2 \
#   HALO_MAP_DIR=<the folder with bloodgulch.map and the others> \
#   rust/halo-server/check/run.sh
#
# What runs, each pinned to cores of its own so that they do not compete the
# way they would not on a real deployment (this machine: 8 cores, 16 threads;
# cpus n and n+8 are the two threads of one core, so only the first thread of
# each core is used, except for the game, and the others stay idle):
#
#   SpacetimeDB Standalone   cpus 0,1      (CPUS_STDB)
#   halo-server + gateway    cpus 2,3      (CPUS_SERVER)
#   halo-slayer-load         cpus 4,5,6    (CPUS_LOAD)
#   the game (GAME=1)        cpus 7,15     (CPUS_GAME)
#
# Settings, as environment variables (defaults in brackets):
#   PLAYERS [500]   simulated players (the game, if any, is one more)
#   SCORE_LIMIT [600]  team score that ends the match; SECONDS_LIMIT [900] its time limit
#   RESPAWN_SECONDS [5]  seconds a dead player waits; SUICIDE_SECONDS [10] more for a suicide or a
#                   death nobody caused; WAVE_SECONDS [5] between the waves of the fallback
#                   (all three are written into the server's configuration, and the report prints them)
#   END_SECS [90]   seconds the final scoreboard stays up; HOLD [END_SECS] seconds the
#                   simulated players stay seated after the end (they must outlast the scoreboard)
#   SHOTS [0.5]     shots a second per player with a target in range; RANGE [25];
#                   GUEST: set to 1 for the crowd to walk to the game (player id PLAYERS) and spare it;
#                   HUNT [400]: how far (world units) a player with no target looks for an enemy to walk to;
#                   NAV_CELL [0.5]: width (world units) of the squares of the grid of walkable ground the
#                   hunters are steered over, round walls and cliffs (0: no grid, straight at the enemy)
#   SEND_THREADS [4]  the gateway's sending threads
#   BUDGET [90000]  bytes a second per player; LOSS [0] chance a datagram of a
#                   simulated player is lost, each way
#   GAME [0]        1: also start the real game once the simulated players are in
#                   (needs build/linux/halo, HALO_DATA_ROOT, a display); GAME_SECS [400];
#                   GAME_LOG: set to 1 to have the game log every player it draws each second
#                   (large.log_players: a line each, which costs frames);
#                   SHOT_EVERY [0]: a screenshot of every Nth frame, into $OUT/shots;
#                   GAME_ENV extra "NAME=value" settings for it, space separated;
#                   GAME_LOSS: e.g. 0.02, to put the game behind a relay (halo-udp-loss) that
#                   loses that share of its datagrams, each way
#   BIN_DIR         a folder with halo-server and the other programs, already built (to compare two
#                   builds of the server by the same script); without it the script builds them
#   NO_BUILD [unset]  1: do not build anything (MATCH_WASM [the module's release build] is the match module to run)
#   MODULE_FEATURES [none]  cargo features for the match module, e.g. stage-timing (module-logs/ gets the
#                   host's timing lines; python3 check/stages.py module-logs/* summarises them)
#   OUT             where logs and reports go [rust/halo-server/target/slayer-check/<time>]
#
# The script builds the modules and programs it needs.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
rust=$(cd "$here/../.." && pwd)
repo=$(cd "$rust/.." && pwd)
: "${HALO_STDB_BIN:?HALO_STDB_BIN: the SpacetimeDB 2.10.x release folder}"
: "${HALO_MAP_DIR:?HALO_MAP_DIR: the folder with the .map files}"
PLAYERS=${PLAYERS:-500}
SCORE_LIMIT=${SCORE_LIMIT:-600}
SECONDS_LIMIT=${SECONDS_LIMIT:-900}
RESPAWN_SECONDS=${RESPAWN_SECONDS:-5}
SUICIDE_SECONDS=${SUICIDE_SECONDS:-10}
WAVE_SECONDS=${WAVE_SECONDS:-5}
END_SECS=${END_SECS:-90}
HOLD=${HOLD:-$END_SECS}
WINDOW=${WINDOW:-30}
SHOTS=${SHOTS:-0.5}
RANGE=${RANGE:-25}
HUNT=${HUNT:-400}
NAV_CELL=${NAV_CELL:-0.5}
BUDGET=${BUDGET:-90000}
SEND_THREADS=${SEND_THREADS:-4}
LOSS=${LOSS:-0}
GAME=${GAME:-0}
GAME_SECS=${GAME_SECS:-400}
SHOT_EVERY=${SHOT_EVERY:-0}
CPUS_STDB=${CPUS_STDB:-0,1}
CPUS_SERVER=${CPUS_SERVER:-2,3}
CPUS_LOAD=${CPUS_LOAD:-4,5,6}
CPUS_GAME=${CPUS_GAME:-7,15}
OUT=${OUT:-$here/../target/slayer-check/$(date +%Y%m%d-%H%M%S)}
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
ulimit -n 65536

# NO_BUILD=1 skips the builds (they are done beforehand, so that a run that holds the machine does not
# also hold it for a compile); MATCH_WASM names the match module to run then.
# MODULE_FEATURES=stage-timing: the match module times the stages of its tick (see stages.py)
# BIN_DIR: the programs are already built there, so only the modules are built
if [ -z "${NO_BUILD:-}" ]; then
  (cd "$rust/halo-match-module" && cargo build --locked --release --target wasm32-unknown-unknown ${MODULE_FEATURES:+--features "$MODULE_FEATURES"} 2>&1 | tail -1)
  (cd "$rust/halo-root-module" && cargo build --locked --release --target wasm32-unknown-unknown 2>&1 | tail -1)
  if [ -z "${BIN_DIR:-}" ]; then
    (cd "$rust/halo-server" && cargo build --locked --release 2>&1 | tail -1)
  fi
fi
MATCH_WASM=${MATCH_WASM:-$rust/halo-match-module/target/wasm32-unknown-unknown/release/halo_match_module.wasm}
bin="${BIN_DIR:-$rust/halo-server/target/release}"

# the server's configuration: one server, one rotation entry
cat > "$OUT/server.toml" <<TOML
[spacetimedb]
url = "http://127.0.0.1:3000"
start = false
owner_token_file = "owner.token"

[root]
database = "halo-root"
module = "$rust/halo-root-module/target/wasm32-unknown-unknown/release/halo_root_module.wasm"

[match]
module = "$MATCH_WASM"
maps_dir = "$HALO_MAP_DIR"

[[server]]
id = "lounge"
title = "Slayer check"
bind = "127.0.0.1:7777"
advertise = "127.0.0.1"
budget = $BUDGET
send_threads = $SEND_THREADS
log_secs = 1
handover_secs = 5
end_secs = $END_SECS

[[server.rotation]]
map = "bloodgulch"
game_type = "team_slayer"
capacity = $((PLAYERS + 10))
seconds = $SECONDS_LIMIT
score_limit = $SCORE_LIMIT
respawn_seconds = $RESPAWN_SECONDS
suicide_penalty_seconds = $SUICIDE_SECONDS
wave_seconds = $WAVE_SECONDS
TOML

stdb=""
server=""
cleanup() {
  # the server first: it takes its matches down (deletes their databases) on SIGTERM
  if [ -n "$server" ]; then
    kill -TERM "$server" 2>/dev/null || true
    for _ in $(seq 100); do kill -0 "$server" 2>/dev/null || break; sleep 0.3; done
    kill -KILL "$server" 2>/dev/null || true
    server=""
  fi
  if [ -n "$stdb" ]; then
    kill -TERM "$stdb" 2>/dev/null || true
    sleep 2
    kill -KILL "$stdb" 2>/dev/null || true
    stdb=""
  fi
}
trap cleanup EXIT

echo "== other work on the machine before the run" | tee "$OUT/machine.txt"
(pgrep -af 'cargo|ninja|build/linux/halo|spacetimedb' || true) | grep -v "pgrep\|run.sh" | tee -a "$OUT/machine.txt" || true
uptime | tee -a "$OUT/machine.txt"

mkdir -p "$OUT/stdb"
taskset -c "$CPUS_STDB" "$HALO_STDB_BIN/spacetimedb-standalone" start --listen-addr 127.0.0.1:3000 \
  --non-interactive --data-dir "$OUT/stdb/data" \
  --jwt-pub-key-path "$OUT/stdb/id_ecdsa.pub" --jwt-priv-key-path "$OUT/stdb/id_ecdsa" \
  > "$OUT/spacetimedb.log" 2>&1 &
stdb=$!
for _ in $(seq 100); do curl -sf http://127.0.0.1:3000/v1/ping >/dev/null && break; sleep 0.2; done
sleep 3

# the load first, so that it is waiting when the match is listed
taskset -c "$CPUS_LOAD" "$bin/halo-slayer-load" --spacetimedb http://127.0.0.1:3000 --maps "$HALO_MAP_DIR" \
  --players "$PLAYERS" --shots "$SHOTS" --range "$RANGE" --hunt "$HUNT" --nav-cell "$NAV_CELL" ${GUEST:+--guest "$PLAYERS"} --budget "$BUDGET" --loss "$LOSS" --hold "$HOLD" --window "$WINDOW" \
  --owner-token-file "$OUT/owner.token" --out "$OUT" > "$OUT/load.stdout" 2> "$OUT/load.log" &
load=$!
date +%s.%N > "$OUT/server.start"
taskset -c "$CPUS_SERVER" "$bin/halo-server" --config "$OUT/server.toml" > "$OUT/server.stdout" 2> "$OUT/server.log" &
server=$!

game=""
relay=""

# CPU time of each program every 5 seconds (clock ticks of user + system time, 100 a second):
# cpu.log has one line each, "<unix time> <name>=<ticks> ..." (summarise.py turns it into cores used)
(
  while true; do
    line="$(date +%s.%N)"
    for name in spacetimedb-standalone halo-server halo-slayer-load halo; do
      # (by the program's path: the name the kernel keeps is cut to 15 characters)
      case "$name" in
        halo) pid=$(pgrep -x -n halo || true) ;;
        halo-server) pid=$(pgrep -x -n halo-server || true) ;;
        *) pid=$(pgrep -n -f "(^|/)$name( |$)" || true) ;;
      esac
      if [ -n "$pid" ] && [ -r "/proc/$pid/stat" ]; then
        ticks=$(awk '{print $14 + $15}' "/proc/$pid/stat" 2>/dev/null || echo 0)
        line="$line $name=$ticks"
      fi
    done
    echo "$line" >> "$OUT/cpu.log"
    # CPU time of each thread of SpacetimeDB, which does not grow when the machine is busy the way a
    # tick's wall-clock time does: threads.log has one line each, "<unix time> <tid>:<name>:<ticks> ..."
    pid=$(pgrep -n -f "(^|/)spacetimedb-standalone( |$)" || true)
    if [ -n "$pid" ]; then
      tline="$(date +%s.%N)"
      for task in /proc/$pid/task/*; do
        tline="$tline $(awk '{gsub(/[()]/, "", $2); print $1 ":" $2 ":" $14 + $15}' "$task/stat" 2>/dev/null)"
      done
      echo "$tline" >> "$OUT/threads.log"
    fi
    # the same for the server: its threads (gateway-send-N, gateway-recv, the SDK's) say where a tick's
    # CPU goes (server-threads.log; python3 check/threads.py server-threads.log shows it per tick)
    pid=$(pgrep -x -n halo-server || true)
    if [ -n "$pid" ]; then
      tline="$(date +%s.%N)"
      for task in /proc/$pid/task/*; do
        tline="$tline $(awk '{gsub(/[()]/, "", $2); print $1 ":" $2 ":" $14 ":" $15}' "$task/stat" 2>/dev/null)"
      done
      echo "$tline" >> "$OUT/server-threads.log"
    fi
    # what the game has been sent over its connection to SpacetimeDB (the slow state: scores,
    # the player list, the items), in bytes since it connected
    rx=$(ss -tinp 'dport = :3000' 2>/dev/null | grep -A1 '"halo"' | grep -o 'bytes_received:[0-9]*' | head -1 | cut -d: -f2 || true)
    [ -n "$rx" ] && echo "$(date +%s.%N) $rx" >> "$OUT/slow-rx.log"
    sleep 5
  done
) &
sampler=$!
if [ "$GAME" = 1 ]; then
  for _ in $(seq 600); do grep -q "welcomed over UDP" "$OUT/load.log" && break; sleep 0.5; done
  mkdir -p "$OUT/game/saves" "$OUT/shots"
  if [ -n "${GAME_LOSS:-}" ]; then
    # the game's link to the gateway loses GAME_LOSS of its datagrams, each way
    taskset -c "$CPUS_LOAD" "$bin/halo-udp-loss" --listen 127.0.0.1:7790 --to 127.0.0.1:7777 --loss "$GAME_LOSS" \
      2> "$OUT/relay.log" &
    relay=$!
    GAME_GATEWAY=127.0.0.1:7790
  fi
  # shellcheck disable=SC2086
  (cd "$repo/build/linux" && exec env HALO_DATA_ROOT="${HALO_DATA_ROOT:?HALO_DATA_ROOT}" HALO_SAVE_ROOT="$OUT/game/saves" \
    HALO_LARGE_MAP=bloodgulch HALO_LARGE_GATEWAY="${GAME_GATEWAY:-127.0.0.1:7777}" \
    HALO_LARGE_SPACETIMEDB=http://127.0.0.1:3000 HALO_LARGE_DATABASE="$(grep -o 'hm-lounge-[0-9-]*' "$OUT/server.log" | head -1)" \
    ${GAME_LOG:+HALO_LARGE_LOG=1} HALO_NET_ONLINE=0 HALO_FULLSCREEN=0 HALO_NO_VSYNC=1 HALO_NO_AUDIO=1 HALO_HIDDEN_WINDOW=1 \
    HALO_UPDATE_ANSWER=no HALO_EXIT_AFTER="$GAME_SECS" HALO_SCREENSHOT_DIR="$OUT/shots" \
    HALO_SCREENSHOT_EVERY="${SHOT_EVERY:-0}" HALO_MAX_FPS=-1 \
    ${GAME_ENV:-} \
    flock "${GAME_LOCK:-/tmp/halo-game.lock}" taskset -c "$CPUS_GAME" ./halo > "$OUT/game.log" 2>&1) &
  game=$!
fi

wait "$load" || echo "the load program failed: see $OUT/load.log"
if [ -n "${MODULE_FEATURES:-}" ]; then
  # the module's own log (the stage timings): the match's database is gone once the server stops
  mkdir -p "$OUT/module-logs"
  # (a file a database: they are all called by the day's date, in folders of their own; the match the
  # crowd is in is the first, and its database is deleted a little after the match ends, so HOLD
  # should be shorter than END_SECS for this to find it)
  for f in $(find "$OUT/stdb/data" -name '*.log' -path '*module_logs*' 2>/dev/null); do
    cp "$f" "$OUT/module-logs/$(echo "${f#"$OUT"/stdb/data/}" | tr / _)" || true
  done
fi
if [ -n "$game" ]; then kill -TERM "$game" 2>/dev/null || true; wait "$game" 2>/dev/null || true; fi
if [ -n "$relay" ]; then kill -TERM "$relay" 2>/dev/null || true; fi
cleanup
kill "$sampler" 2>/dev/null || true
if [ "$GAME" = 1 ]; then
  ended=$(grep -o "ended at tick [0-9]*" "$OUT/load.log" | grep -o "[0-9]*$" | head -1 || true)
  python3 "$here/game-summary.py" "$OUT/game.log" 5 "${ended:-99999999}" | tee "$OUT/game-summary.txt"
  if [ -n "${GAME_LOG:-}" ]; then
    echo "-- how current the game's picture of the players around it was (ticks old, by distance)"
    python3 "$here/smoothness.py" "$OUT/game.log" 10 | tee "$OUT/smoothness.txt"
  fi
  if [ -s "$OUT/slow-rx.log" ]; then
    python3 - "$OUT/slow-rx.log" <<'PY' | tee -a "$OUT/game-summary.txt"
import sys
rows = [tuple(map(float, l.split())) for l in open(sys.argv[1])]
rates = [(b[1] - a[1]) / (b[0] - a[0]) / 1000 for a, b in zip(rows, rows[1:])]
if rates:
    print(f"the game's connection to SpacetimeDB (scores, player list, items): mean {sum(rates)/len(rates):.1f} KB/s, "
          f"highest 5 s {max(rates):.1f}, lowest {min(rates):.1f} ({len(rates)} samples)")
PY
  fi
fi
python3 "$here/summarise.py" "$OUT/server.log" | tee "$OUT/server-summary.txt"
echo "reports in $OUT"
