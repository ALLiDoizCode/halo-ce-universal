//! Fixtures for the items: a second weapon beside the pistol, the powerups of
//! the maps, and a flat floor with the placements a match makes them at.

use alloc::vec::Vec;

use halo_map::items::{powerup, ItemDef, Items, Placement, Reach, PLACEMENT_CREATED_AT_REST};

use super::combat::{pistol, PISTOL};
use crate::map::MapData;

/// The tag index of [`rifle`], and of the powerups, as the tags of Blood Gulch have them.
pub const RIFLE: u16 = 237;
pub const CAMOUFLAGE: u16 = 1055;
pub const OVERSHIELD: u16 = 1067;
pub const HEALTH_PACK: u16 = 1074;
pub const FRAG_GRENADE: u16 = 1082;

/// A weapon that is not the pistol, with a bigger magazine and more rounds.
pub fn rifle() -> halo_map::combat::Weapon {
    let mut weapon = pistol();
    weapon.tag_index = RIFLE;
    weapon.name = "weapons\\assault rifle\\assault rifle.weap".into();
    let magazine = &mut weapon.magazines[0];
    magazine.rounds_total_initial = 96;
    magazine.rounds_total_maximum = 288;
    magazine.rounds_loaded_maximum = 32;
    weapon
}

fn weapon_def(tag_index: u16, name: &str, bounding_radius: f32) -> ItemDef {
    ItemDef {
        tag_index,
        is_weapon: true,
        name: name.into(),
        bounding_radius,
        bounding_offset: [0.0; 3],
        flags: 0,
        powerup_type: 0,
        grenade_type: 0,
        powerup_time: 0.0,
    }
}

fn equipment_def(tag_index: u16, name: &str, powerup_type: i16, powerup_time: f32) -> ItemDef {
    ItemDef {
        tag_index,
        is_weapon: false,
        name: name.into(),
        bounding_radius: 0.1,
        bounding_offset: [0.0; 3],
        flags: 0,
        powerup_type,
        grenade_type: 0,
        powerup_time,
    }
}

/// What the items of the fixture map are, as the tags of Blood Gulch have them: the pistol and the
/// rifle, a shield for 60 seconds, camouflage for 45 and a health pack, a grenade. A player reaches
/// 0.42 around them, as the multiplayer biped does.
pub fn item_defs() -> Vec<ItemDef> {
    Vec::from([
        weapon_def(RIFLE, "weapons\\assault rifle\\assault rifle.weap", 0.6),
        weapon_def(PISTOL, "weapons\\pistol\\pistol.weap", 0.1),
        equipment_def(CAMOUFLAGE, "powerups\\active camouflage.eqip", powerup::ACTIVE_CAMOUFLAGE, 45.0),
        equipment_def(OVERSHIELD, "powerups\\over shield.eqip", powerup::OVERSHIELD, 60.0),
        equipment_def(HEALTH_PACK, "powerups\\health pack.eqip", powerup::HEALTH, 1.0),
        equipment_def(FRAG_GRENADE, "weapons\\frag grenade\\frag grenade.eqip", powerup::GRENADE, 0.0),
    ])
}

/// A placement of one item at `(x, y)`, just above the floor, for any game, that
/// comes every `seconds` (0: the collection's own, `collection_seconds`).
pub fn placement(tag: u16, x: f32, y: f32, seconds: i16, collection_seconds: i16) -> Placement {
    Placement {
        flags: 0,
        game_types: [halo_map::game_type::ALL, 0, 0, 0],
        spawn_time: seconds,
        position: [x, y, 0.2],
        facing: 0.0,
        collection_spawn_time: collection_seconds,
        permutations: Vec::from([(100.0, tag)]),
    }
}

/// The items of the fixture map: a rifle every 10 seconds, an overshield and
/// camouflage on their collections' times (60 and 45 seconds), a health pack
/// every 30, a capture-the-flag-only rifle, a grenade, and a pistol made at
/// rest in the air.
pub fn items_fixture() -> Items {
    let mut at_rest = placement(PISTOL, -10.0, 0.0, 15, 0);
    at_rest.flags = PLACEMENT_CREATED_AT_REST;
    at_rest.position[2] = 2.0;
    let mut ctf_only = placement(RIFLE, 50.0, 0.0, 10, 0);
    ctf_only.game_types = [halo_map::game_type::CTF, 0, 0, 0];
    Items {
        defs: item_defs(),
        placements: Vec::from([
            placement(RIFLE, 10.0, 0.0, 10, 0),
            placement(OVERSHIELD, 20.0, 0.0, 0, 60),
            placement(CAMOUFLAGE, 30.0, 0.0, 0, 45),
            placement(HEALTH_PACK, 40.0, 0.0, 0, 0),
            ctf_only,
            placement(FRAG_GRENADE, 5.0, 5.0, 0, 0),
            at_rest,
        ]),
        player: Reach { bounding_radius: 0.42, bounding_offset: [0.0; 3] },
    }
}

/// The flat floor with the pistol and the rifle and [`items_fixture`]'s items.
pub fn items_map() -> MapData {
    let mut map = super::flat_floor_map();
    map.combat.weapons.push(rifle());
    map.items = items_fixture();
    map
}

/// `map` with these placements and no others.
pub fn with_placements(mut map: MapData, placements: &[Placement]) -> MapData {
    map.items.placements = placements.to_vec();
    map
}
