//! The game variant in the match module, against a real local SpacetimeDB: a
//! player who spawns carries the weapons and grenades the variant gives, and
//! the default variant is as it was. The rule is tested at the step in
//! `rust/halo-sim/src/variant.rs`; this checks that the module applies it at a
//! spawn and that a bad setting is refused.
//!
//! Needs `HALO_STDB_BIN` (a SpacetimeDB 2.10.x release directory) and skips
//! itself without it; no game data: the map is the fixtures'.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use halo_match_driver::module_bindings::set_variant as _;
use halo_match_driver::server::{build_module, stdb_bin_dir, Server};
use halo_match_driver::{MatchClient, PlayerClient};
use halo_sim::fixtures::{items_map, start_at, with_starts, PISTOL};
use halo_sim::rules::Rules;
use halo_sim::variant::{StartingEquipment, Variant, WeaponSet};

const WAIT: Duration = Duration::from_secs(30);
const STATE_ALIVE: u8 = 0;
const NO_WEAPON: u16 = u16::MAX;

fn wasm() -> &'static PathBuf {
    static WASM: OnceLock<PathBuf> = OnceLock::new();
    WASM.get_or_init(build_module)
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A match with this variant (`None`: none set) and one player spawned in it.
fn spawned(name: &str, variant: Option<Variant>) -> Option<(Server, MatchClient, PlayerClient)> {
    let Some(bin) = stdb_bin_dir() else {
        eprintln!("HALO_STDB_BIN is not set: skipping, this test needs a SpacetimeDB 2.10.x release");
        return None;
    };
    let server = Server::start(&bin);
    server.publish(wasm(), name);
    let owner = server.connect(name);
    owner.load_map(with_starts(items_map(), &[start_at(10.0, 0.0, -1)]).to_bytes()).unwrap();
    owner.set_game(&Rules::slayer()).unwrap();
    if let Some(variant) = variant {
        owner.set_variant(&variant).unwrap();
    }
    owner.start();
    let player = PlayerClient::connect_unsubscribed(&server.uri(), name, &server.new_account().token);
    player.join([1; 32]).unwrap();
    wait_until("the player spawned", || {
        owner.standings().values().filter(|s| s.state == STATE_ALIVE).count() == 1 && owner.fighters().len() == 1
    });
    wait_until("the kit", || !owner.kits().is_empty());
    Some((server, owner, player))
}

#[test]
fn a_player_spawns_with_the_variants_grenades_and_the_default_variants_weapon() {
    let Some((_server, owner, _player)) = spawned("variant-default", None) else { return };
    assert_eq!(owner.fighters()[&0].weapon_0, PISTOL);
    assert_eq!(owner.fighters()[&0].weapon_1, NO_WEAPON);
    let kit = &owner.kits()[&0];
    assert_eq!((kit.frag, kit.plasma), (2, 2));

    let variant = Variant {
        weapon_set: WeaponSet::PlasmaWeapons,
        equipment: StartingEquipment::Generic,
        infinite_grenades: true,
    };
    let Some((_server, owner, _player)) = spawned("variant-plasma", Some(variant)) else { return };
    let kit = &owner.kits()[&0];
    assert_eq!((kit.frag, kit.plasma), (0, 2), "plasma weapons give plasma grenades only");
}

#[test]
fn a_weapon_set_that_does_not_exist_is_refused() {
    let Some((_server, owner, _player)) = spawned("variant-bad", None) else { return };
    let refused =
        halo_match_driver::call_reducer("set_variant", |cb| owner.conn.reducers.set_variant_then(99, false, false, cb));
    assert!(refused.is_err(), "{refused:?}");
}
