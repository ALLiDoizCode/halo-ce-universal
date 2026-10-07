#!/bin/bash
# Rebuilds and reruns the benchmark. Needs cargo with the wasm32-unknown-unknown
# target, and the SpacetimeDB 2.10.2 release unpacked into $STDB_BIN
# (spacetimedb-standalone and spacetimedb-cli).
#
#   STDB_BIN=/path/to/release ./run.sh
#
# The CPU lists assume an 8-core, 16-thread machine where cpu N and N+8 share
# a core; change SERVER_CPUS and CLIENT_CPUS for other machines.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
: "${STDB_BIN:?set STDB_BIN to the directory holding spacetimedb-standalone and spacetimedb-cli}"
SERVER_CPUS=${SERVER_CPUS:-0-3,8-11}
CLIENT_CPUS=${CLIENT_CPUS:-4-7,12-15}
WORK=$HERE/target
mkdir -p "$WORK/data" "$WORK/keys" "$WORK/cfg"
ulimit -n 65536

(cd "$HERE/module" && CARGO_TARGET_DIR=$WORK/module cargo build --release --target wasm32-unknown-unknown)
(cd "$HERE/client" && CARGO_TARGET_DIR=$WORK/client cargo build --release)

"$STDB_BIN/spacetimedb-standalone" start --listen-addr 127.0.0.1:3777 --data-dir "$WORK/data" \
    --non-interactive --jwt-pub-key-path "$WORK/keys/id_ecdsa.pub" --jwt-priv-key-path "$WORK/keys/id_ecdsa" \
    > "$WORK/server.log" 2>&1 &
SERVER=$!
trap 'kill $SERVER' EXIT
sleep 4
XDG_CONFIG_HOME=$WORK/cfg "$STDB_BIN/spacetimedb-cli" publish --server http://127.0.0.1:3777 --anonymous \
    --no-config -y -b "$WORK/module/wasm32-unknown-unknown/release/halobench_module.wasm" halobench
taskset -a -cp "$SERVER_CPUS" $SERVER > /dev/null

run() {
    taskset -c "$CLIENT_CPUS" "$WORK/client/release/halobench-client" "$@" \
        --compression none --secs 20 --warmup 5 --server-pid $SERVER
    sleep 2
}

run --players 128 --label "128, all visible"
run --players 256 --label "256, all visible"
run --players 256 --confirmed 0 --label "256, all visible, confirmed reads off"
run --players 512 --cells 16 --label "512, 32 visible"
run --players 1024 --cells 32 --label "1024, 32 visible"
run --players 1024 --cells 32 --inputs 0 --label "1024, 32 visible, no inputs"
run --players 1536 --cells 48 --label "1536, 32 visible"
run --players 2048 --cells 64 --label "2048, 32 visible"
