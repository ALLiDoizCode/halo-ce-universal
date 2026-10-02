//! The operator's configuration file (TOML): everything the server is told, in
//! one place. See [`EXAMPLE`] for the whole of it, and `halo-server --help`.
//!
//! Relative paths in the file are relative to the file's own folder.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use halo_sim::rules::Rules;
use halo_sim::TICKS_PER_SECOND;
use serde::Deserialize;

/// An example with every setting, and what each one does. The tests parse it,
/// so it stays true.
pub const EXAMPLE: &str = r#"# halo-server: one file for the whole large-scale server.
#
#   halo-server --config server.toml            run the servers
#   halo-server --config server.toml ban <identity> [reason...]
#   halo-server --config server.toml unban <identity>
#   halo-server --config server.toml bans

[spacetimedb]
# The SpacetimeDB the server runs on: one instance, which holds the root
# database and every match's own database. The licence allows one instance per
# operator.
url = "http://127.0.0.1:3000"
# true: halo-server starts Standalone itself (the release unpacked in bin_dir,
# its data and keys in data_dir) and stops it when it stops. false: it is
# already running (and is run by you, with the keys you chose).
start = true
bin_dir = "spacetimedb"          # holds spacetimedb-standalone
data_dir = "spacetimedb-data"
# The identity halo-server publishes and owns every database as: made on first
# start, then kept here. Whoever holds this file owns the server's databases.
owner_token_file = "owner.token"

[root]
# The permanent database of the server list, the identities and the bans.
database = "halo-root"
# Built with: cargo build --release --target wasm32-unknown-unknown
#   in rust/halo-root-module
module = "halo_root_module.wasm"

[match]
# The match module (rust/halo-match-module, built the same way): each match is
# a fresh database of it.
module = "halo_match_module.wasm"
# Your own copy of the game's maps (the folder with bloodgulch.map and the
# others); no game data is shipped with the server.
maps_dir = "maps"

# One [[server]] per entry of the server list. Several can run side by side on
# the one SpacetimeDB.
[[server]]
id = "lounge"                    # a-z, 0-9 and -, up to 24 characters; names the databases
title = "The Lounge"             # what the list shows
# Where players send their per-tick UDP. A match uses this port, the next one
# after it, and so on in turn, so that the next match's gateway is up while the
# last one's players are moving over: open both.
bind = "0.0.0.0:7777"
# The host players reach this machine by, which the list tells them.
advertise = "play.example.org"
budget = 90000                   # bytes a second each player may be sent (headers included)
send_threads = 4                 # threads the gateway sends with
log_secs = 10                    # seconds between lines of the log
handover_secs = 5                # seconds the last match stays up after the next is announced
end_secs = 15                    # seconds a finished match shows its final scoreboard before the next

# The rotation: the server plays these in order, and from the top again.
# A match ends when a player (a team, in team_slayer) reaches the score limit
# or the time is up, shows its final scoreboard for end_secs, and the next
# match begins.
[[server.rotation]]
map = "bloodgulch"
game_type = "slayer"             # slayer, or team_slayer: the original rules of each
variant = ""                     # a name for the list; the rules are the settings below
# capacity = 300                 # most players on this map; without it, the map's own
                                 # (bloodgulch 500, sidewinder 300, the small maps 16 to 64)
seconds = 600                    # the time limit: the match ends after this long (0: none)
# score_limit = 25               # the score that ends it: kills, or the team's in team_slayer
                                 # (without it, the original's: 15 for slayer, 50 for team_slayer)
# respawn_seconds = 5            # seconds a dead player waits (never under 3)
# suicide_penalty_seconds = 10   # seconds more after a suicide or a death nobody caused
# wave_seconds = 5               # when no starting location is free, players spawn in waves this far apart

[[server.rotation]]
map = "sidewinder"
game_type = "team_slayer"
capacity = 200
seconds = 900
score_limit = 100
budget = 60000                   # this map only: another per-player budget
"#;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub spacetimedb: SpacetimeDb,
    pub root: Root,
    #[serde(rename = "match")]
    pub matches: Matches,
    #[serde(rename = "server")]
    pub servers: Vec<Server>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpacetimeDb {
    #[serde(default = "default_url")]
    pub url: String,
    #[serde(default)]
    pub start: bool,
    pub bin_dir: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
    #[serde(default = "default_token_file")]
    pub owner_token_file: PathBuf,
}

fn default_url() -> String {
    "http://127.0.0.1:3000".into()
}

fn default_token_file() -> PathBuf {
    "owner.token".into()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Root {
    #[serde(default = "default_root_database")]
    pub database: String,
    pub module: PathBuf,
}

fn default_root_database() -> String {
    "halo-root".into()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Matches {
    pub module: PathBuf,
    pub maps_dir: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    pub id: String,
    pub title: Option<String>,
    pub bind: SocketAddr,
    #[serde(default = "default_advertise")]
    pub advertise: String,
    #[serde(default = "default_budget")]
    pub budget: u32,
    #[serde(default = "default_send_threads")]
    pub send_threads: usize,
    #[serde(default = "default_log_secs")]
    pub log_secs: u64,
    #[serde(default = "default_handover_secs")]
    pub handover_secs: u64,
    /// How long a finished match stays up for its players to read the final
    /// scoreboard.
    #[serde(default = "default_end_secs")]
    pub end_secs: u64,
    pub rotation: Vec<Rotation>,
}

fn default_advertise() -> String {
    "127.0.0.1".into()
}

fn default_budget() -> u32 {
    90_000
}

fn default_send_threads() -> usize {
    4
}

fn default_log_secs() -> u64 {
    10
}

fn default_handover_secs() -> u64 {
    5
}

fn default_end_secs() -> u64 {
    15
}

impl Server {
    pub fn title(&self) -> &str {
        self.title.as_deref().unwrap_or(&self.id)
    }

    /// Where match number `n` (from 1) of this server takes its UDP: the
    /// configured port, then the next, alternating.
    pub fn gateway_bind(&self, n: u64) -> SocketAddr {
        let mut addr = self.bind;
        addr.set_port(self.bind.port() + (n.wrapping_sub(1) % 2) as u16);
        addr
    }

    /// What the list tells players to send their UDP to for match `n`.
    pub fn gateway_advertised(&self, n: u64) -> String {
        format!("{}:{}", self.advertise, self.gateway_bind(n).port())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rotation {
    pub map: String,
    /// `slayer` or `team_slayer`.
    #[serde(default = "default_game_type")]
    pub game_type: String,
    /// A name the list shows; the settings below are the variant's rules.
    #[serde(default)]
    pub variant: String,
    /// The most players on this map; the map's own default without it (see
    /// [`crate::maps::default_capacity`]).
    pub capacity: Option<u16>,
    /// The time limit, from when the match is announced; 0 for none.
    #[serde(default = "default_seconds")]
    pub seconds: u32,
    /// The score that ends the match; the game type's original without it.
    pub score_limit: Option<u32>,
    pub respawn_seconds: Option<u32>,
    pub suicide_penalty_seconds: Option<u32>,
    pub wave_seconds: Option<u32>,
    /// Overrides the server's `budget` for this map.
    pub budget: Option<u32>,
}

fn default_game_type() -> String {
    "slayer".into()
}

fn default_seconds() -> u32 {
    600
}

impl Rotation {
    /// The rules of the game this step plays: the original variant of its
    /// game type, with the settings it overrides.
    pub fn rules(&self) -> Result<Rules, String> {
        let mut rules = match self.game_type.as_str() {
            "slayer" => Rules::slayer(),
            "team_slayer" => Rules::team_slayer(),
            other => {
                return Err(format!("game_type {other:?}: only slayer and team_slayer are played so far"));
            }
        };
        let ticks = |seconds: u32| seconds.saturating_mul(TICKS_PER_SECOND);
        rules.time_limit_ticks = ticks(self.seconds);
        if let Some(limit) = self.score_limit {
            rules.score_limit = limit;
        }
        if let Some(seconds) = self.respawn_seconds {
            rules.respawn_ticks = ticks(seconds);
        }
        if let Some(seconds) = self.suicide_penalty_seconds {
            rules.suicide_penalty_ticks = ticks(seconds);
        }
        if let Some(seconds) = self.wave_seconds {
            if seconds == 0 {
                return Err("wave_seconds must be at least 1".into());
            }
            rules.wave_ticks = ticks(seconds);
        }
        Ok(rules)
    }
}

impl Config {
    /// Read and check a file; relative paths in it become relative to its folder.
    pub fn load(path: &Path) -> Result<Config, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let base = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        Config::parse(&text, base).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn parse(text: &str, base: &Path) -> Result<Config, String> {
        let mut config: Config = toml::from_str(text).map_err(|e| e.to_string())?;
        let at = |p: &mut PathBuf| {
            if p.is_relative() {
                *p = base.join(&*p);
            }
        };
        at(&mut config.spacetimedb.owner_token_file);
        for p in [&mut config.spacetimedb.bin_dir, &mut config.spacetimedb.data_dir].into_iter().flatten() {
            at(p);
        }
        at(&mut config.root.module);
        at(&mut config.matches.module);
        at(&mut config.matches.maps_dir);
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), String> {
        if self.spacetimedb.start && (self.spacetimedb.bin_dir.is_none() || self.spacetimedb.data_dir.is_none()) {
            return Err("[spacetimedb] start = true needs bin_dir and data_dir".into());
        }
        if !valid_name(&self.root.database, 40) {
            return Err(format!("[root] database {:?}: a-z, 0-9, - and _ only", self.root.database));
        }
        if self.servers.is_empty() {
            return Err("there is no [[server]]".into());
        }
        let mut ids = BTreeSet::new();
        let mut ports = BTreeSet::new();
        for server in &self.servers {
            let id = &server.id;
            if !valid_name(id, 24) || id.contains('_') {
                return Err(format!("server id {id:?}: a-z, 0-9 and - only, at most 24 characters"));
            }
            if !ids.insert(id.clone()) {
                return Err(format!("two servers are called {id:?}"));
            }
            if server.rotation.is_empty() {
                return Err(format!("server {id:?} has no [[server.rotation]]: nothing to play"));
            }
            if server.send_threads == 0 {
                return Err(format!("server {id:?}: send_threads must be at least 1"));
            }
            if server.bind.port() == u16::MAX || server.bind.port() == 0 {
                return Err(format!("server {id:?}: bind needs a port below 65535 (the next one is used too)"));
            }
            for port in [server.bind.port(), server.bind.port() + 1] {
                if !ports.insert(port) {
                    return Err(format!("server {id:?}: UDP port {port} is also another server's"));
                }
            }
            for step in &server.rotation {
                if !valid_name(&step.map, 40) {
                    return Err(format!("server {id:?}: map {:?}: a-z, 0-9 and _ only", step.map));
                }
                if step.capacity == Some(0) {
                    return Err(format!("server {id:?}, map {}: capacity must be at least 1", step.map));
                }
                step.rules().map_err(|e| format!("server {id:?}, map {}: {e}", step.map))?;
                if step.budget == Some(0) || server.budget == 0 {
                    return Err(format!("server {id:?}: a budget of 0 sends players nothing"));
                }
            }
        }
        Ok(())
    }
}

fn valid_name(name: &str, longest: usize) -> bool {
    !name.is_empty()
        && name.len() <= longest
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Config, String> {
        Config::parse(text, Path::new("/etc/halo"))
    }

    #[test]
    fn the_example_is_a_valid_configuration() {
        let config = parse(EXAMPLE).unwrap();
        assert_eq!(config.servers.len(), 1);
        let server = &config.servers[0];
        assert_eq!(server.rotation.len(), 2);
        assert_eq!(server.rotation[0].capacity, None, "the map's own");
        assert_eq!(server.rotation[1].capacity, Some(200));
        assert_eq!(server.rotation[1].budget, Some(60000));
        // relative paths are the file's folder's
        assert_eq!(config.root.module, Path::new("/etc/halo/halo_root_module.wasm"));
        assert_eq!(config.matches.maps_dir, Path::new("/etc/halo/maps"));
    }

    #[test]
    fn matches_alternate_between_two_udp_ports() {
        let server = parse(EXAMPLE).unwrap().servers.remove(0);
        assert_eq!(server.gateway_bind(1).port(), 7777);
        assert_eq!(server.gateway_bind(2).port(), 7778);
        assert_eq!(server.gateway_bind(3).port(), 7777);
        assert_eq!(server.gateway_advertised(2), "play.example.org:7778");
    }

    #[test]
    fn mistakes_are_named() {
        let bad = |from: &str, to: &str| parse(&EXAMPLE.replace(from, to)).unwrap_err();
        assert!(bad("id = \"lounge\"", "id = \"The Lounge\"").contains("server id"));
        assert!(bad("map = \"bloodgulch\"", "map = \"../etc/passwd\"").contains("map"));
        assert!(bad("capacity = 200", "capacity = 0").contains("capacity"));
        assert!(bad("game_type = \"slayer\"", "game_type = \"ctf\"").contains("only slayer and team_slayer"));
        assert!(bad("# wave_seconds = 5 ", "wave_seconds = 0 ").contains("wave_seconds"));
        assert!(bad("send_threads = 4", "send_threadz = 4").contains("send_threadz"));
        assert!(bad("start = true", "start = false\nurl2 = 1").contains("url2"));
        assert!(bad("data_dir = \"spacetimedb-data\"", "").contains("data_dir"));
    }

    #[test]
    fn servers_may_not_share_ports_or_names() {
        let two = format!(
            "{EXAMPLE}\n[[server]]\nid = \"lounge\"\nbind = \"0.0.0.0:7777\"\n[[server.rotation]]\nmap = \"x\"\n"
        );
        assert!(parse(&two).unwrap_err().contains("two servers"));
        let overlap =
            two.replace("id = \"lounge\"\nbind = \"0.0.0.0:7777\"", "id = \"other\"\nbind = \"0.0.0.0:7778\"");
        assert!(parse(&overlap).unwrap_err().contains("7778"));
    }
}
