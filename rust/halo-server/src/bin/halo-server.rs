//! The large-scale server as a program.
//!
//!   halo-server --config server.toml              run the servers until stopped
//!   halo-server --config server.toml servers      the server list as it stands
//!   halo-server --config server.toml bans         who is banned
//!   halo-server --config server.toml ban <identity> [reason...]
//!   halo-server --config server.toml unban <identity>
//!   halo-server --example                         print an example configuration
//!
//! `ban` and `unban` change the root database; the running server carries the
//! change to every match within a moment. The identity is the hex one a
//! player's client shows (the console's `servers` command prints it) and the
//! server's log names.

use std::path::PathBuf;
use std::sync::Arc;

use halo_server::config::{Config, EXAMPLE};
use halo_server::maps::MapFiles;
use halo_server::servers::Log;
use spacetimedb_sdk::Identity;

const USAGE: &str = "halo-server --config <file> [servers | bans | ban <identity> [reason...] | unban <identity>]\n       halo-server --example";

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return;
    }
    if args.iter().any(|a| a == "--example") {
        print!("{EXAMPLE}");
        return;
    }
    let mut config_path = PathBuf::from("server.toml");
    if let Some(at) = args.iter().position(|a| a == "--config") {
        let Some(path) = args.get(at + 1) else { fail("--config needs a file") };
        config_path = PathBuf::from(path);
        args.drain(at..at + 2);
    }
    let config = Config::load(&config_path).unwrap_or_else(|e| fail(&e));

    match args.first().map(String::as_str) {
        None => run(config),
        Some("servers") => servers(&config),
        Some("bans") => bans(&config),
        Some("ban") => {
            let Some(identity) = args.get(1) else { fail("ban needs an identity") };
            let reason = if args.len() > 2 { args[2..].join(" ") } else { "banned by the operator".into() };
            change_ban(&config, identity, Some(&reason));
        }
        Some("unban") => {
            let Some(identity) = args.get(1) else { fail("unban needs an identity") };
            change_ban(&config, identity, None);
        }
        Some(other) => fail(&format!("{other:?} is not a command\n{USAGE}")),
    }
}

fn run(config: Config) {
    let maps = Arc::new(MapFiles { dir: config.matches.maps_dir.clone() });
    let log = Log::new();
    let running = halo_server::start(config, maps, log).unwrap_or_else(|e| fail(&e));
    let running = Arc::new(running);
    {
        let running = running.clone();
        ctrlc::set_handler(move || running.request_stop()).unwrap_or_else(|e| fail(&format!("signal handler: {e}")));
    }
    running.wait();
    let running = Arc::try_unwrap(running).unwrap_or_else(|_| fail("the server is still referenced"));
    if let Err(e) = running.stop() {
        fail(&e);
    }
}

fn owner_root(config: &Config) -> halo_server::root::Root {
    let token = halo_server::owner_token_from(config).unwrap_or_else(|e| fail(&e));
    halo_server::connect_root(config, &token).unwrap_or_else(|e| fail(&e))
}

fn servers(config: &Config) {
    let root = owner_root(config);
    let servers = root.servers();
    if servers.is_empty() {
        println!("no servers are listed");
    }
    for s in servers {
        let database = if s.database.is_empty() { "(between matches)" } else { &s.database };
        println!(
            "{}  {}  {} {}  {}/{}  match {}  {}  {}",
            s.id, s.title, s.map, s.game_type, s.players, s.capacity, s.match_number, s.gateway, database
        );
    }
}

fn bans(config: &Config) {
    let root = owner_root(config);
    let bans = root.bans();
    if bans.is_empty() {
        println!("nobody is banned");
    }
    for b in bans {
        println!("{}  {}", b.identity.to_hex(), b.reason);
    }
}

fn change_ban(config: &Config, identity: &str, reason: Option<&str>) {
    let identity =
        Identity::from_hex(identity).unwrap_or_else(|e| fail(&format!("{identity:?} is not an identity: {e}")));
    let root = owner_root(config);
    let result = match reason {
        Some(reason) => root.ban(identity, reason),
        None => root.unban(identity),
    };
    root.disconnect();
    match result {
        Ok(()) => println!("{} {}", if reason.is_some() { "banned" } else { "unbanned" }, identity.to_hex()),
        Err(e) => fail(&e),
    }
}

fn fail(message: &str) -> ! {
    eprintln!("halo-server: {message}");
    std::process::exit(2);
}
