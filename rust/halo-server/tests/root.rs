//! The root database's module (`rust/halo-root-module`) on a real local
//! SpacetimeDB, through the connections the orchestration and the clients use:
//! who may change the server list and the bans, what registering an identity
//! does, and what a banned identity is told. Needs `HALO_STDB_BIN`; skips
//! itself without it.

use std::time::{Duration, Instant};

use halo_match_driver::root_bindings::{register, DbConnection, Server as ServerRow};
use halo_match_driver::server::{stdb_bin_dir, Server as Stdb};
use halo_server::admin::Admin;
use halo_server::fixtures::modules;
use halo_server::root::Root;
use spacetimedb_sdk::{DbContext, Identity};

fn wait_for(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(30));
    }
}

fn row(id: &str, database: &str) -> ServerRow {
    ServerRow {
        id: id.into(),
        title: "A server".into(),
        map: "bloodgulch".into(),
        game_type: "slayer".into(),
        variant: String::new(),
        database: database.into(),
        gateway: "127.0.0.1:7777".into(),
        players: 0,
        capacity: 100,
        match_number: 1,
        match_seconds: 600,
        match_started_us: 0,
        updated_us: 0,
    }
}

/// `register` as the connection's identity, and the module's answer.
fn register_as(stdb: &Stdb, token: &str) -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let conn = DbConnection::builder()
        .with_uri(stdb.uri())
        .with_database_name("halo-root")
        .with_token(Some(token.to_string()))
        .build()
        .expect("connect");
    conn.run_threaded();
    conn.reducers
        .register_then(move |_, result| {
            let _ = tx.send(result);
        })
        .unwrap();
    let answer = rx.recv_timeout(Duration::from_secs(20)).expect("an answer").map_err(|e| e.to_string())?;
    let _ = conn.disconnect();
    answer
}

#[test]
fn only_the_owner_writes_the_list_and_the_bans_and_a_banned_identity_cannot_register() {
    let Some(bin) = stdb_bin_dir() else {
        eprintln!("HALO_STDB_BIN is not set: skipping, this test needs a SpacetimeDB 2.10.x release");
        return;
    };
    let stdb = Stdb::start(&bin);
    let admin = Admin::new(&stdb.uri()).unwrap();
    admin.publish("halo-root", &std::fs::read(&modules().0).unwrap(), &stdb.owner().token).unwrap();
    let owner = Root::connect(&stdb.uri(), "halo-root", &stdb.owner().token).unwrap();
    let stranger_account = stdb.new_account();
    let stranger = Root::connect(&stdb.uri(), "halo-root", &stranger_account.token).unwrap();

    // the list: the owner writes it, everyone reads it, nobody else writes
    owner.set_server(row("lounge", "hm-lounge-1")).unwrap();
    wait_for("the list at a stranger", || stranger.servers().len() == 1);
    assert_eq!(stranger.servers()[0].database, "hm-lounge-1");
    let refused = stranger.set_server(row("mine", "x")).unwrap_err();
    assert!(refused.contains("owner"), "told: {refused}");
    assert!(stranger.set_players("lounge", 5).unwrap_err().contains("owner"));
    assert!(stranger.remove_server("lounge").unwrap_err().contains("owner"));
    assert!(stranger.clear_servers().unwrap_err().contains("owner"));
    owner.set_players("lounge", 7).unwrap();
    wait_for("the count", || stranger.servers()[0].players == 7);
    // text that would not fit in a list is refused
    let long = "x".repeat(200);
    assert!(owner.set_server(ServerRow { title: long, ..row("lounge", "x") }).unwrap_err().contains("at most"));
    owner.remove_server("lounge").unwrap();
    wait_for("the server to go", || stranger.servers().is_empty());

    // bans: the owner's alone
    let identity = Identity::from_hex(&stranger_account.identity).unwrap();
    assert!(stranger.ban(identity, "I ban myself").unwrap_err().contains("owner"));
    assert!(stranger.unban(identity).unwrap_err().contains("owner"));

    // registering makes an identity known, and a ban turns it away with the reason
    register_as(&stdb, &stranger_account.token).unwrap();
    wait_for("the identity to be known", || owner.known_identities().contains(&identity));
    owner.ban(identity, "abusive chat").unwrap();
    let refused = register_as(&stdb, &stranger_account.token).unwrap_err();
    assert!(refused.contains("banned: abusive chat"), "told: {refused}");
    wait_for("the ban to be seen", || stranger.bans().iter().any(|b| b.identity == identity));
    // a ban's reason can be changed, and it goes when lifted
    owner.ban(identity, "abusive chat, again").unwrap();
    assert!(register_as(&stdb, &stranger_account.token).unwrap_err().contains("abusive chat, again"));
    owner.unban(identity).unwrap();
    register_as(&stdb, &stranger_account.token).unwrap();
    wait_for("the ban to be gone", || owner.bans().is_empty());
}
