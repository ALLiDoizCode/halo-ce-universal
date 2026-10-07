//! What a game variant gives a player who spawns: the weapons they carry and
//! the grenades of each kind, by the engine's rule (`source/game/game_engine.c`:
//! `universal_variant.weapon_set`, the generic-starting-equipment and
//! infinite-grenades flags of `universal_variant.flags`, and the map's
//! `scenario_starting_equipment`).
//!
//! - A variant with **generic** starting equipment gives the weapon of its
//!   weapon set (the pistol for the sets that name none) and two grenades of
//!   each kind.
//! - A variant that is not generic gives the map's own starting equipment for
//!   the game type: the weapons its item collections pick (by weight), and
//!   grenades by its flags (none, plasma instead of frag, or two frags). A map
//!   with no starting equipment for the game type gives the generic kit.
//! - The weapon set then narrows the grenades: plasma weapons give plasma
//!   grenades only, human weapons frags only, and "no grenades" none. This is
//!   the kind remap `game_engine_remap_equipment` applies to grenades lying on
//!   the map, taken as the spawn's rule too: the engine's own spawn handler
//!   (`_handle_custom_starting_equipment`) has no body in this source.
//! - Infinite grenades give the same grenades at the spawn; a throw does not
//!   cost one ([`crate::items::Kit::throw_grenade`]).

use alloc::vec::Vec;

use crate::combat::{starting_weapon, NO_WEAPON};
use crate::map::MapData;
use crate::rng::Rng;

/// Grenades of each kind a generic variant starts a player with.
pub const GENERIC_GRENADES: u8 = 2;

/// The engine's game type number of Slayer and Team Slayer
/// (`game_engine_slayer`), which `scenario_starting_equipment` names by.
pub const GAME_TYPE_SLAYER: i16 = 2;

/// The grenade kinds, as the engine indexes them (`unit.grenade_counts`).
pub const FRAG: usize = 0;
pub const PLASMA: usize = 1;

// `scenario_starting_equipment.flags`
const NO_GRENADES: u32 = 1;
const PLASMA_GRENADES: u32 = 2;

// `game_type` values of a starting equipment that name more than one game type
// (the engine's `_game_engine_all`, `_all_non_team` and `_all_normal`)
const ALL_GAMES: i16 = 12;
const ALL_NON_TEAM: i16 = 13;
const ALL_NORMAL: i16 = 14;
// the engine's game type numbers
const CAPTURE_THE_FLAG: i16 = 1;
const RACE: i16 = 5;

/// `universal_variant.weapon_set` (`enum game_engine_weapons`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WeaponSet {
    #[default]
    Normal,
    Pistols,
    AssaultRifles,
    PlasmaWeapons,
    Sniping,
    NoSniping,
    RocketLaunchers,
    Shotguns,
    ShortRange,
    Human,
    NoGrenades,
}

impl WeaponSet {
    /// The names a server's configuration uses, as they are in the engine's
    /// enumeration, one by one.
    pub const NAMES: [(&'static str, WeaponSet); 11] = [
        ("normal", WeaponSet::Normal),
        ("pistols", WeaponSet::Pistols),
        ("assault_rifles", WeaponSet::AssaultRifles),
        ("plasma", WeaponSet::PlasmaWeapons),
        ("sniping", WeaponSet::Sniping),
        ("no_sniping", WeaponSet::NoSniping),
        ("rockets", WeaponSet::RocketLaunchers),
        ("shotguns", WeaponSet::Shotguns),
        ("short_range", WeaponSet::ShortRange),
        ("human", WeaponSet::Human),
        ("no_grenades", WeaponSet::NoGrenades),
    ];

    pub fn from_name(name: &str) -> Option<WeaponSet> {
        WeaponSet::NAMES.iter().find(|(n, _)| *n == name).map(|(_, set)| *set)
    }

    /// The number a weapon set travels as (its place in the engine's enumeration).
    pub fn code(self) -> u8 {
        WeaponSet::NAMES.iter().position(|(_, set)| *set == self).unwrap_or(0) as u8
    }

    pub fn from_code(code: u8) -> Option<WeaponSet> {
        WeaponSet::NAMES.get(code as usize).map(|(_, set)| *set)
    }

    pub fn name(self) -> &'static str {
        WeaponSet::NAMES.iter().find(|(_, set)| *set == self).map_or("", |(n, _)| n)
    }

    /// The weapons (tag names) a player of a generic variant starts with.
    fn weapons(self) -> &'static [&'static str] {
        match self {
            WeaponSet::Pistols => &["weapons\\pistol\\pistol.weap"],
            WeaponSet::AssaultRifles => &["weapons\\assault rifle\\assault rifle.weap"],
            WeaponSet::PlasmaWeapons => {
                &["weapons\\plasma pistol\\plasma pistol.weap", "weapons\\plasma rifle\\plasma rifle.weap"]
            }
            WeaponSet::Sniping => &["weapons\\sniper rifle\\sniper rifle.weap"],
            WeaponSet::RocketLaunchers => &["weapons\\rocket launcher\\rocket launcher.weap"],
            WeaponSet::Shotguns => &["weapons\\shotgun\\shotgun.weap"],
            WeaponSet::ShortRange => &["weapons\\shotgun\\shotgun.weap", "weapons\\needler\\needler.weap"],
            WeaponSet::Normal | WeaponSet::NoSniping | WeaponSet::Human | WeaponSet::NoGrenades => &[],
        }
    }
}

/// Where a player's starting equipment comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StartingEquipment {
    /// The weapon set's weapon and two grenades of each kind.
    #[default]
    Generic,
    /// The map's own, for the game type.
    Map,
}

/// The settings of a variant that bear on what a player spawns with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Variant {
    pub weapon_set: WeaponSet,
    pub equipment: StartingEquipment,
    pub infinite_grenades: bool,
}

/// What a player spawns with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnKit {
    /// Tag indices of the weapons carried (`NO_WEAPON` for none).
    pub weapons: [u16; 2],
    /// Grenades by kind ([`FRAG`], [`PLASMA`]).
    pub grenades: [u8; 2],
}

impl Variant {
    /// The weapons and grenades of a player who spawns in a game of `game_type`
    /// (the engine's number, [`GAME_TYPE_SLAYER`] for Slayer) on `map`.
    pub fn spawn_kit(&self, map: &MapData, game_type: i16, rng: &mut Rng) -> SpawnKit {
        let mapped = match self.equipment {
            StartingEquipment::Map => map_equipment(map, game_type, rng),
            StartingEquipment::Generic => None,
        };
        let (weapons, mut grenades) = mapped.unwrap_or_else(|| (self.generic_weapons(map), [GENERIC_GRENADES; 2]));
        let all = grenades[FRAG].max(grenades[PLASMA]);
        grenades = match self.weapon_set {
            WeaponSet::PlasmaWeapons => [0, all],
            WeaponSet::Human => [all, 0],
            WeaponSet::NoGrenades => [0, 0],
            _ => grenades,
        };
        SpawnKit { weapons, grenades }
    }

    fn generic_weapons(&self, map: &MapData) -> [u16; 2] {
        let mut weapons = [NO_WEAPON; 2];
        let named = self
            .weapon_set
            .weapons()
            .iter()
            .filter_map(|name| map.combat.weapons.iter().find(|w| w.name == *name).map(|w| w.tag_index));
        for (slot, tag) in weapons.iter_mut().zip(named) {
            *slot = tag;
        }
        if weapons[0] == NO_WEAPON {
            weapons[0] = starting_weapon(map).unwrap_or(NO_WEAPON);
        }
        weapons
    }
}

fn names_game_type(listed: &[i16; 4], game_type: i16) -> bool {
    listed.iter().any(|&t| {
        t == game_type
            || t == ALL_GAMES
            || (t == ALL_NON_TEAM && game_type != CAPTURE_THE_FLAG)
            || (t == ALL_NORMAL && game_type != CAPTURE_THE_FLAG && game_type != RACE)
    })
}

/// The map's starting equipment for the game type, if it has any: the weapons
/// (at most two) and the grenades.
fn map_equipment(map: &MapData, game_type: i16, rng: &mut Rng) -> Option<([u16; 2], [u8; 2])> {
    let entry = map.combat.starting_equipment.iter().find(|e| names_game_type(&e.game_types, game_type))?;
    let mut weapons = [NO_WEAPON; 2];
    let mut next = 0;
    for collection in &entry.collections {
        if next == weapons.len() {
            break;
        }
        // an item that is not a weapon (an equipment) is not a starting weapon
        let permutations: Vec<&(f32, u16)> =
            collection.iter().filter(|(_, tag)| map.combat.weapon(*tag).is_some()).collect();
        if let Some(tag) = pick(&permutations, rng) {
            weapons[next] = tag;
            next += 1;
        }
    }
    if weapons[0] == NO_WEAPON {
        return None;
    }
    let grenades = if entry.flags & NO_GRENADES != 0 {
        [0, 0]
    } else if entry.flags & PLASMA_GRENADES != 0 {
        [0, GENERIC_GRENADES]
    } else {
        [GENERIC_GRENADES, 0]
    };
    Some((weapons, grenades))
}

/// One of the permutations, by weight.
fn pick(permutations: &[&(f32, u16)], rng: &mut Rng) -> Option<u16> {
    if permutations.is_empty() {
        return None;
    }
    let total: f32 = permutations.iter().map(|(w, _)| w.max(0.0)).sum();
    let mut at = rng.next_f32() * total;
    for (weight, tag) in permutations {
        at -= weight.max(0.0);
        if at < 0.0 {
            return Some(*tag);
        }
    }
    permutations.last().map(|(_, tag)| *tag)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::flat_floor_map;
    use crate::fixtures::{combat_fixture, plasma_pistol, plasma_rifle, rocket_launcher};
    use halo_map::combat::StartingEquipment as MapStart;

    fn map_with(start: Vec<MapStart>) -> MapData {
        let mut map = flat_floor_map();
        map.combat = combat_fixture();
        map.combat.weapons.extend([plasma_pistol(), plasma_rifle(), rocket_launcher()]);
        map.combat.starting_equipment = start;
        map
    }

    fn tag(map: &MapData, name: &str) -> u16 {
        map.combat.weapons.iter().find(|w| w.name == name).unwrap().tag_index
    }

    fn kit(variant: Variant, map: &MapData) -> SpawnKit {
        variant.spawn_kit(map, GAME_TYPE_SLAYER, &mut Rng::seeded(7))
    }

    #[test]
    fn the_default_variant_starts_a_player_with_the_pistol() {
        let map = map_with(Vec::new());
        let k = kit(Variant::default(), &map);
        assert_eq!(k.weapons, [starting_weapon(&map).unwrap(), NO_WEAPON]);
        assert_eq!(k.grenades, [2, 2]);
    }

    #[test]
    fn a_weapon_set_gives_its_weapon_and_a_set_without_one_the_pistol() {
        let map = map_with(Vec::new());
        let rockets = Variant { weapon_set: WeaponSet::RocketLaunchers, ..Variant::default() };
        assert_eq!(kit(rockets, &map).weapons[0], tag(&map, "weapons\\rocket launcher\\rocket launcher.weap"));
        let plasma = Variant { weapon_set: WeaponSet::PlasmaWeapons, ..Variant::default() };
        assert_eq!(
            kit(plasma, &map).weapons,
            [
                tag(&map, "weapons\\plasma pistol\\plasma pistol.weap"),
                tag(&map, "weapons\\plasma rifle\\plasma rifle.weap")
            ]
        );
        // a set the map has no weapon for falls back to the pistol
        let snipers = Variant { weapon_set: WeaponSet::Sniping, ..Variant::default() };
        assert_eq!(kit(snipers, &map).weapons[0], starting_weapon(&map).unwrap());
        let human = Variant { weapon_set: WeaponSet::Human, ..Variant::default() };
        assert_eq!(kit(human, &map).weapons[0], starting_weapon(&map).unwrap());
    }

    #[test]
    fn the_weapon_set_narrows_the_grenades() {
        let map = map_with(Vec::new());
        let with = |weapon_set| kit(Variant { weapon_set, ..Variant::default() }, &map).grenades;
        assert_eq!(with(WeaponSet::PlasmaWeapons), [0, 2]);
        assert_eq!(with(WeaponSet::Human), [2, 0]);
        assert_eq!(with(WeaponSet::NoGrenades), [0, 0]);
        assert_eq!(with(WeaponSet::Normal), [2, 2]);
    }

    fn start(flags: u32, game_types: [i16; 4], weapon: u16) -> MapStart {
        MapStart { flags, game_types, collections: Vec::from([Vec::from([(1.0, weapon)])]) }
    }

    #[test]
    fn the_maps_own_equipment_is_used_when_the_variant_is_not_generic() {
        let mut map = map_with(Vec::new());
        let rocket = tag(&map, "weapons\\rocket launcher\\rocket launcher.weap");
        map.combat.starting_equipment = Vec::from([
            start(0, [1, -1, -1, -1], tag(&map, "weapons\\pistol\\pistol.weap")),
            start(0, [GAME_TYPE_SLAYER, -1, -1, -1], rocket),
        ]);
        let own = Variant { equipment: StartingEquipment::Map, ..Variant::default() };
        let k = kit(own, &map);
        assert_eq!(k.weapons, [rocket, NO_WEAPON]);
        assert_eq!(k.grenades, [2, 0]);
        // not for a game type it does not name, nor when the variant is generic
        let other = own.spawn_kit(&map, 4, &mut Rng::seeded(1));
        assert_eq!(other.weapons[0], starting_weapon(&map).unwrap());
        assert_eq!(kit(Variant::default(), &map).weapons[0], starting_weapon(&map).unwrap());
        // "all games" names every one
        map.combat.starting_equipment[1].game_types = [ALL_GAMES, -1, -1, -1];
        assert_eq!(own.spawn_kit(&map, 4, &mut Rng::seeded(1)).weapons[0], rocket);
    }

    #[test]
    fn the_maps_equipment_flags_choose_the_grenades() {
        let mut map = map_with(Vec::new());
        let pistol = tag(&map, "weapons\\pistol\\pistol.weap");
        let own = Variant { equipment: StartingEquipment::Map, ..Variant::default() };
        let with = |map: &mut MapData, flags| {
            map.combat.starting_equipment = Vec::from([start(flags, [GAME_TYPE_SLAYER, -1, -1, -1], pistol)]);
            kit(own, map).grenades
        };
        assert_eq!(with(&mut map, NO_GRENADES), [0, 0]);
        assert_eq!(with(&mut map, PLASMA_GRENADES), [0, 2]);
        assert_eq!(with(&mut map, 0), [2, 0]);
        // and the weapon set still narrows them
        map.combat.starting_equipment = Vec::from([start(0, [GAME_TYPE_SLAYER, -1, -1, -1], pistol)]);
        let plasma_only = Variant { weapon_set: WeaponSet::PlasmaWeapons, ..own };
        assert_eq!(kit(plasma_only, &map).grenades, [0, 2]);
    }

    #[test]
    fn a_collection_is_picked_from_by_weight() {
        let mut map = map_with(Vec::new());
        let (pistol, rocket) =
            (tag(&map, "weapons\\pistol\\pistol.weap"), tag(&map, "weapons\\rocket launcher\\rocket launcher.weap"));
        map.combat.starting_equipment = Vec::from([MapStart {
            flags: 0,
            game_types: [GAME_TYPE_SLAYER, -1, -1, -1],
            collections: Vec::from([Vec::from([(0.0, pistol), (3.0, rocket)])]),
        }]);
        let own = Variant { equipment: StartingEquipment::Map, ..Variant::default() };
        for seed in 0..20 {
            assert_eq!(own.spawn_kit(&map, GAME_TYPE_SLAYER, &mut Rng::seeded(seed)).weapons[0], rocket);
        }
    }

    /// A spawn as the module does it: the variant's kit into the fighter and the player's kit.
    fn spawn(variant: Variant, map: &MapData) -> (crate::combat::Fighter, crate::items::Kit) {
        use crate::combat::{CombatStore, MemoryCombat, Trails};
        use crate::items::{ItemStore, MemoryItems};
        let (mut combat, mut items) = (MemoryCombat::new(), MemoryItems::new());
        let spawn_kit = variant.spawn_kit(map, GAME_TYPE_SLAYER, &mut Rng::seeded(3));
        crate::combat::spawn_with(&mut combat, &mut Trails::new(), map, 4, 0, spawn_kit.weapons);
        let fighter = combat.fighter(4).unwrap();
        crate::pickups::on_spawn(&mut items, map, 4, &fighter.loadout);
        crate::pickups::give_grenades(&mut items, 4, spawn_kit.grenades);
        (fighter, items.kit(4))
    }

    #[test]
    fn a_player_spawns_with_the_variants_weapons_rounds_and_grenades() {
        let map = map_with(Vec::new());
        let rocket = tag(&map, "weapons\\rocket launcher\\rocket launcher.weap");
        let (fighter, kit) = spawn(Variant { weapon_set: WeaponSet::RocketLaunchers, ..Variant::default() }, &map);
        assert_eq!(fighter.loadout.weapons, [rocket, NO_WEAPON]);
        assert_eq!(kit.ammo[0].loaded, crate::items::initial_rounds(&map, rocket).0);
        assert_eq!(kit.grenades, [2, 2]);
        let (_, kit) = spawn(Variant { weapon_set: WeaponSet::NoGrenades, ..Variant::default() }, &map);
        assert_eq!(kit.grenades, [0, 0]);
    }

    #[test]
    fn a_throw_costs_a_grenade_unless_they_are_infinite() {
        let map = map_with(Vec::new());
        let (_, mut kit) = spawn(Variant::default(), &map);
        assert!(kit.throw_grenade(FRAG, false));
        assert_eq!(kit.grenades, [1, 2]);
        assert!(kit.throw_grenade(FRAG, false));
        assert!(!kit.throw_grenade(FRAG, false), "none left");
        assert_eq!(kit.grenades, [0, 2]);

        let (_, mut kit) = spawn(Variant { infinite_grenades: true, ..Variant::default() }, &map);
        for _ in 0..10 {
            assert!(kit.throw_grenade(PLASMA, true));
        }
        assert_eq!(kit.grenades, [2, 2]);
        // infinite grenades do not make grenades of a kind the variant gives none of
        let (_, mut kit) =
            spawn(Variant { weapon_set: WeaponSet::Human, infinite_grenades: true, ..Variant::default() }, &map);
        assert!(!kit.throw_grenade(PLASMA, true));
    }

    #[test]
    fn weapon_set_names_round_trip() {
        for (name, set) in WeaponSet::NAMES {
            assert_eq!(WeaponSet::from_name(name), Some(set));
            assert_eq!(set.name(), name);
            assert_eq!(WeaponSet::from_code(set.code()), Some(set));
        }
        assert_eq!(WeaponSet::from_name("bazookas"), None);
        assert_eq!(WeaponSet::from_code(11), None);
    }
}
