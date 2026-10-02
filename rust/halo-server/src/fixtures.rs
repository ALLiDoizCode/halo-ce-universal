//! What the tests of the orchestration, and of the client that reads its
//! list, are made from, so that none of them needs the game's data: a flat
//! floor for a map, and a configuration for a throwaway SpacetimeDB.

use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use halo_match_driver::root_bindings::{register, DbConnection};
use halo_match_driver::server::{build_module, build_root_module};
use halo_sim::fixtures::{flat_floor_map, start_at, with_starts};
use spacetimedb_sdk::DbContext;

use crate::config::Config;
use crate::maps::{default_capacity, LoadedMap, MapSource};

/// A flat floor under every map name but `nomap`, which cannot be loaded.
pub struct FlatFloors;

impl MapSource for FlatFloors {
    fn load(&self, name: &str) -> Result<LoadedMap, String> {
        if name == "nomap" {
            return Err("nomap.map: no such map".into());
        }
        // starting locations 10 world units apart (the rules keep enemies 2 apart)
        let starts: Vec<_> = (0..4).map(|i| start_at(i as f32 * 10.0 - 15.0, 0.0, -1)).collect();
        Ok(LoadedMap { data: with_starts(flat_floor_map(), &starts), default_capacity: default_capacity(name) })
    }
}

/// The root and the match module, built for WebAssembly (once).
pub fn modules() -> &'static (PathBuf, PathBuf) {
    static MODULES: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();
    MODULES.get_or_init(|| (build_root_module(), build_module()))
}

/// The first of two UDP ports in a row that are free now.
pub fn free_udp_pair() -> u16 {
    loop {
        let first = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = first.local_addr().unwrap().port();
        if port < 65000 && UdpSocket::bind(("127.0.0.1", port + 1)).is_ok() {
            return port;
        }
    }
}

pub fn free_tcp_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// A folder of the test's own, empty.
pub fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("halo-server-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The configuration of servers on the SpacetimeDB at `url` (which the
/// orchestration starts from `stdb_bin` when that is given, keeping its data in
/// `dir`), each with its own UDP ports; `servers` is `(id, rotation)`, the
/// rotation being the TOML of its `[[server.rotation]]` tables. The owner's
/// token is read from `dir/owner.token`.
pub fn test_config(dir: &Path, url: &str, stdb_bin: Option<&Path>, servers: &[(&str, &str)]) -> Config {
    let (root_wasm, match_wasm) = modules();
    let start = match stdb_bin {
        Some(bin) => format!("start = true\nbin_dir = {:?}\ndata_dir = {:?}\n", bin, dir.join("stdb")),
        None => String::new(),
    };
    let mut text = format!(
        r#"
[spacetimedb]
url = "{url}"
{start}owner_token_file = {token:?}
[root]
module = {root_wasm:?}
[match]
module = {match_wasm:?}
maps_dir = "unused"
"#,
        token = dir.join("owner.token"),
    );
    for (id, rotation) in servers {
        text.push_str(&format!(
            r#"
[[server]]
id = "{id}"
title = "Server {id}"
bind = "127.0.0.1:{udp}"
budget = 50000
send_threads = 2
log_secs = 1
handover_secs = 1
end_secs = 1
{rotation}
"#,
            udp = free_udp_pair(),
        ));
    }
    Config::parse(&text, dir).expect("a valid configuration")
}

/// A player's first word to the server list: `register` at the root database
/// `halo-root` on `url`, as the identity of `token`, under `name`. The module's
/// answer (an error is the refusal of a banned identity).
pub fn register_at_root(url: &str, token: &str, name: &str) -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let conn = DbConnection::builder()
        .with_uri(url)
        .with_database_name("halo-root")
        .with_token(Some(token.to_string()))
        .build()
        .map_err(|e| e.to_string())?;
    conn.run_threaded();
    conn.reducers
        .register_then(name.to_string(), move |_, result| {
            let _ = tx.send(result);
        })
        .map_err(|e| e.to_string())?;
    let answer = rx.recv_timeout(std::time::Duration::from_secs(20)).map_err(|_| "no answer".to_string())?;
    let _ = conn.disconnect();
    answer.map_err(|e| e.to_string())?
}
