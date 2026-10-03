//! What the map's tags say of fighting: the weapons (their magazines,
//! triggers and projectiles, down to the damage a projectile deals when it
//! hits), the multiplayer player's resistance (its health and shields, how
//! the shields recharge, how each part of the body takes damage) and the
//! starting equipment a game type gives. As with [`crate::Movement`], they
//! are read from the map and never retuned.
//!
//! The names are the tags' own (`source/items/weapon_definitions.h`,
//! `source/items/projectile_definitions.h`, `source/objects/damage_effect_definitions.h`,
//! `source/objects/damage_resistances.h`). Every weapon of the map is read,
//! not only the one a player starts with, so that a weapon is added to the
//! game by its data and not by code.
//!
//! [`Combat::to_bytes`] and [`Combat::from_bytes`] are the byte form a server
//! keeps with the map's other data (see `halo_sim::MapData::to_bytes`).

use crate::error::{malformed, Result};

/// The engine's `MAXIMUM_NUMBER_OF_MATERIAL_TYPES`: how many entries a
/// damage's material modifiers have.
pub const MATERIAL_TYPES: usize = 40;

/// `struct damage_definition` of a damage effect tag: what one hit deals.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Damage {
    /// `_damage_side_effect_*`.
    pub side_effect: i16,
    /// `_damage_category_*`.
    pub category: i16,
    pub flags: u32,
    /// What a hit with a damage scale of 0 deals (the scale is 1 for a hit
    /// from a projectile's impact).
    pub minimum: f32,
    /// A hit deals a number between these (uniformly), times its scale.
    pub lower: f32,
    pub upper: f32,
    /// How much the damage counts against each kind of material: by the
    /// material type of the shield or the body part that takes it.
    pub material_modifiers: [f32; MATERIAL_TYPES],
}

/// `damage_definition.flags` bits the damage rules read.
pub mod damage_flags {
    /// The damage does not hurt the player who caused it.
    pub const DOES_NOT_HURT_OWNER: u32 = 1 << 0;
    /// Hits that reach a head part kill outright (campaign headshots).
    pub const CAN_CAUSE_HEADSHOTS: u32 = 1 << 1;
    /// The damage does not hurt the owner's friends.
    pub const DOES_NOT_HURT_FRIENDS: u32 = 1 << 3;
    /// The damage detonates explosives.
    pub const DETONATES_EXPLOSIVES: u32 = 1 << 5;
    /// The damage hurts only shields.
    pub const ONLY_HURTS_SHIELDS: u32 = 1 << 6;
    /// The damage passes through shields.
    pub const SKIPS_SHIELDS: u32 = 1 << 9;
    /// In multiplayer a head part takes twice the damage.
    pub const CAN_CAUSE_MULTIPLAYER_HEADSHOTS: u32 = 1 << 11;
}

/// A projectile tag, as far as a hit on a player goes.
#[derive(Debug, Clone, PartialEq)]
pub struct Projectile {
    pub flags: u32,
    pub detonation_timer_starts: i16,
    pub timer_lower_bound: f32,
    pub timer_upper_bound: f32,
    pub minimum_velocity: f32,
    /// World units; 0 where the tag has none.
    pub maximum_range: f32,
    pub air_gravity_scale: f32,
    /// World units a *tick* (the engine's unit for velocities), at the start
    /// and the end of the flight.
    pub initial_velocity: f32,
    pub final_velocity: f32,
    /// The damage of its impact on what it hits, if the tag has one.
    pub impact_damage: Option<Damage>,
}

/// `struct weapon_magazine_definition`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Magazine {
    pub flags: u32,
    pub rounds_recharged_per_second: i16,
    pub rounds_total_initial: i16,
    pub rounds_total_maximum: i16,
    pub rounds_loaded_maximum: i16,
    pub reload_time: f32,
    pub rounds_reloaded: i16,
    pub chamber_time: f32,
}

/// `struct weapon_trigger_definition`.
#[derive(Debug, Clone, PartialEq)]
pub struct Trigger {
    pub flags: u32,
    /// Rounds a second, with the trigger just pressed and held fully.
    pub initial_rate_of_fire: f32,
    pub final_rate_of_fire: f32,
    /// How much of the way from the initial rate to the final one a tick of
    /// holding the trigger covers, and a tick of letting go gives back: the
    /// cache's runtime values (the tags have the times).
    pub rate_of_fire_acceleration: f32,
    pub rate_of_fire_deceleration: f32,
    /// `NONE` (-1) for a trigger that uses no ammunition.
    pub magazine_index: i16,
    pub rounds_per_shot: i16,
    pub minimum_rounds_loaded_per_shot: i16,
    pub charging_time: f32,
    pub charged_time: f32,
    pub spew_time: f32,
    pub overloading_time: f32,
    pub projectiles_per_shot: i16,
    pub heat_generated_per_round: f32,
    pub age_generated_per_round: f32,
    pub projectile: Option<Projectile>,
}

/// `weapon_trigger_definition.flags` bits the weapon rules read.
pub mod trigger_flags {
    pub const CAN_FIRE_WITH_PARTIAL_AMMUNITION: u32 = 1 << 2;
    /// A latched trigger fires once for each press.
    pub const LATCHED: u32 = 1 << 3;
    pub const TOGGLES: u32 = 1 << 4;
    pub const ANALOG_RATE_OF_FIRE: u32 = 1 << 9;
}

/// A weapon tag.
#[derive(Debug, Clone, PartialEq)]
pub struct Weapon {
    /// The weapon's index among the map's tags: what a hit report names it by
    /// (the client's engine and the server have the same map).
    pub tag_index: u16,
    /// As `name.weapon`.
    pub name: String,
    pub flags: u32,
    pub weapon_type: i16,
    pub secondary_trigger_mode: i16,
    pub heat_recovery_threshold: f32,
    pub heat_overheated_threshold: f32,
    pub heat_detonation_threshold: f32,
    /// A second's worth, as the tag has it.
    pub heat_loss_per_second: f32,
    pub age_rate_of_fire_penalty: f32,
    /// How many ticks a reload takes: the frames of the weapon's first-person
    /// animation for reloading (the engine times the reload by it, not by the
    /// magazine's `reload_time`); 0 where the weapon has none.
    pub reload_frames: i16,
    /// The frames of the weapon's own recoil animation (the model's, played
    /// after each shot): the weapon starts no reload until it has played;
    /// 0 where it has none.
    pub recoil_frames: i16,
    pub magazines: Vec<Magazine>,
    pub triggers: Vec<Trigger>,
    /// The damage of its melee blow.
    pub melee_damage: Option<Damage>,
}

/// `struct damage_resistance_material`: how one part of a body takes damage.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DamageMaterial {
    pub flags: u32,
    /// The index of the material modifier of a damage that this part takes.
    pub material_type: i16,
    pub shield_leak_fraction: f32,
    pub shield_damage_multiplier: f32,
    pub body_damage_multiplier: f32,
}

/// `damage_resistance_material.flags`: this part is the head.
pub const MATERIAL_HEAD: u32 = 1;

/// `struct damage_resistance` of a unit's collision model: what the unit's
/// health and shields are, and how they take damage and recharge.
#[derive(Debug, Clone, PartialEq)]
pub struct Resistance {
    pub flags: u32,
    /// The part a damage with no part of its own (an explosion's) hits; -1 for none.
    pub indirect_damage_material_index: i16,
    pub maximum_body_vitality: f32,
    /// How much of the damage from a player's own team the body does not take.
    pub friendly_damage_resistance: f32,
    pub body_destroyed_threshold: f32,
    pub maximum_shield_vitality: f32,
    /// The index of the material modifier a damage's shield part counts against.
    pub shield_material_type: i16,
    /// `_transition_function_*`.
    pub shield_failure_function: i16,
    pub shield_failure_threshold: f32,
    pub maximum_shield_failure: f32,
    /// Damage below this does not stun the shield (the recharge waits).
    pub minimum_shield_stun_damage: f32,
    pub shield_stun_time: f32,
    pub shield_recharge_time: f32,
    /// How much of a full shield a tick of recharging gives back: the cache's runtime value.
    pub shield_recharge_velocity: f32,
    pub materials: Vec<DamageMaterial>,
}

/// A game type's starting equipment (`struct scenario_starting_equipment`):
/// for each of its item collections, the weapons it picks one of by weight.
#[derive(Debug, Clone, PartialEq)]
pub struct StartingEquipment {
    pub flags: u32,
    /// `game_type` values this applies to.
    pub game_types: [i16; 4],
    /// Each collection: `(weight, the weapon's tag index)` for its permutations.
    pub collections: Vec<Vec<(f32, u16)>>,
}

/// Everything the map says of fighting.
#[derive(Debug, Clone, PartialEq)]
pub struct Combat {
    pub weapons: Vec<Weapon>,
    /// The multiplayer player's unit's.
    pub resistance: Resistance,
    pub starting_equipment: Vec<StartingEquipment>,
}

impl Combat {
    /// The weapon with this tag index.
    pub fn weapon(&self, tag_index: u16) -> Option<&Weapon> {
        self.weapons.iter().find(|w| w.tag_index == tag_index)
    }
}

impl Default for Resistance {
    /// A body with no health and no shields (nothing of it in the tags).
    fn default() -> Resistance {
        Resistance {
            flags: 0,
            indirect_damage_material_index: -1,
            maximum_body_vitality: 0.0,
            friendly_damage_resistance: 0.0,
            body_destroyed_threshold: 0.0,
            maximum_shield_vitality: 0.0,
            shield_material_type: 0,
            shield_failure_function: 0,
            shield_failure_threshold: 0.0,
            maximum_shield_failure: 0.0,
            minimum_shield_stun_damage: 0.0,
            shield_stun_time: 0.0,
            shield_recharge_time: 0.0,
            shield_recharge_velocity: 0.0,
            materials: Vec::new(),
        }
    }
}

impl Default for Combat {
    /// No weapons, and a player that cannot be hurt.
    fn default() -> Combat {
        Combat { weapons: Vec::new(), resistance: Resistance::default(), starting_equipment: Vec::new() }
    }
}

// ---------- the byte form

const MAGIC: &[u8; 4] = b"HCC1";
/// The most elements any list of the byte form may claim: a hostile count
/// cannot make the decoder allocate more than the bytes could hold.
const MAX_LIST: usize = 4096;

struct Writer(Vec<u8>);

impl Writer {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn i16(&mut self, v: i16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f32(&mut self, v: f32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn count(&mut self, n: usize) {
        self.u16(n as u16);
    }
    fn opt_damage(&mut self, d: &Option<Damage>) {
        match d {
            None => self.u8(0),
            Some(d) => {
                self.u8(1);
                self.i16(d.side_effect);
                self.i16(d.category);
                self.u32(d.flags);
                self.f32(d.minimum);
                self.f32(d.lower);
                self.f32(d.upper);
                d.material_modifiers.iter().for_each(|m| self.f32(*m));
            }
        }
    }
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N]> {
        match self.0.split_at_checked(N) {
            Some((head, rest)) => {
                self.0 = rest;
                Ok(head.try_into().unwrap())
            }
            None => malformed("combat data is cut short"),
        }
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take::<1>()?[0])
    }
    fn i16(&mut self) -> Result<i16> {
        Ok(i16::from_le_bytes(self.take()?))
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take()?))
    }
    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.take()?))
    }
    fn count(&mut self) -> Result<usize> {
        let n = self.u16()? as usize;
        if n > MAX_LIST {
            return malformed(format!("combat data has a list of {n}"));
        }
        Ok(n)
    }
    fn opt_damage(&mut self) -> Result<Option<Damage>> {
        match self.u8()? {
            0 => Ok(None),
            1 => {
                let mut d = Damage {
                    side_effect: self.i16()?,
                    category: self.i16()?,
                    flags: self.u32()?,
                    minimum: self.f32()?,
                    lower: self.f32()?,
                    upper: self.f32()?,
                    material_modifiers: [0.0; MATERIAL_TYPES],
                };
                for m in &mut d.material_modifiers {
                    *m = self.f32()?;
                }
                Ok(Some(d))
            }
            _ => malformed("combat data has a damage that is neither there nor not"),
        }
    }
}

impl Combat {
    /// `"HCC1"`, then the weapons (a `u16` count, each weapon field by field),
    /// the resistance, and the starting equipment; little-endian, floats as
    /// their IEEE-754 bits.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = Writer(Vec::new());
        w.0.extend_from_slice(MAGIC);
        w.count(self.weapons.len());
        for weapon in &self.weapons {
            w.u16(weapon.tag_index);
            w.count(weapon.name.len());
            w.0.extend_from_slice(weapon.name.as_bytes());
            w.u32(weapon.flags);
            w.i16(weapon.weapon_type);
            w.i16(weapon.secondary_trigger_mode);
            for v in [
                weapon.heat_recovery_threshold,
                weapon.heat_overheated_threshold,
                weapon.heat_detonation_threshold,
                weapon.heat_loss_per_second,
                weapon.age_rate_of_fire_penalty,
            ] {
                w.f32(v);
            }
            w.i16(weapon.reload_frames);
            w.i16(weapon.recoil_frames);
            w.count(weapon.magazines.len());
            for m in &weapon.magazines {
                w.u32(m.flags);
                for v in [
                    m.rounds_recharged_per_second,
                    m.rounds_total_initial,
                    m.rounds_total_maximum,
                    m.rounds_loaded_maximum,
                ] {
                    w.i16(v);
                }
                w.f32(m.reload_time);
                w.i16(m.rounds_reloaded);
                w.f32(m.chamber_time);
            }
            w.count(weapon.triggers.len());
            for t in &weapon.triggers {
                w.u32(t.flags);
                for v in [
                    t.initial_rate_of_fire,
                    t.final_rate_of_fire,
                    t.rate_of_fire_acceleration,
                    t.rate_of_fire_deceleration,
                ] {
                    w.f32(v);
                }
                for v in [t.magazine_index, t.rounds_per_shot, t.minimum_rounds_loaded_per_shot] {
                    w.i16(v);
                }
                for v in [t.charging_time, t.charged_time, t.spew_time, t.overloading_time] {
                    w.f32(v);
                }
                w.i16(t.projectiles_per_shot);
                w.f32(t.heat_generated_per_round);
                w.f32(t.age_generated_per_round);
                match &t.projectile {
                    None => w.u8(0),
                    Some(p) => {
                        w.u8(1);
                        w.u32(p.flags);
                        w.i16(p.detonation_timer_starts);
                        for v in [
                            p.timer_lower_bound,
                            p.timer_upper_bound,
                            p.minimum_velocity,
                            p.maximum_range,
                            p.air_gravity_scale,
                            p.initial_velocity,
                            p.final_velocity,
                        ] {
                            w.f32(v);
                        }
                        w.opt_damage(&p.impact_damage);
                    }
                }
            }
            w.opt_damage(&weapon.melee_damage);
        }
        let r = &self.resistance;
        w.u32(r.flags);
        w.i16(r.indirect_damage_material_index);
        for v in [
            r.maximum_body_vitality,
            r.friendly_damage_resistance,
            r.body_destroyed_threshold,
            r.maximum_shield_vitality,
        ] {
            w.f32(v);
        }
        w.i16(r.shield_material_type);
        w.i16(r.shield_failure_function);
        for v in [
            r.shield_failure_threshold,
            r.maximum_shield_failure,
            r.minimum_shield_stun_damage,
            r.shield_stun_time,
            r.shield_recharge_time,
            r.shield_recharge_velocity,
        ] {
            w.f32(v);
        }
        w.count(r.materials.len());
        for m in &r.materials {
            w.u32(m.flags);
            w.i16(m.material_type);
            w.f32(m.shield_leak_fraction);
            w.f32(m.shield_damage_multiplier);
            w.f32(m.body_damage_multiplier);
        }
        w.count(self.starting_equipment.len());
        for s in &self.starting_equipment {
            w.u32(s.flags);
            s.game_types.iter().for_each(|g| w.i16(*g));
            w.count(s.collections.len());
            for c in &s.collections {
                w.count(c.len());
                for (weight, tag) in c {
                    w.f32(*weight);
                    w.u16(*tag);
                }
            }
        }
        w.0
    }

    /// Rebuild from [`Combat::to_bytes`]; bytes that are cut short, are
    /// followed by more, or claim absurd lists are refused.
    pub fn from_bytes(bytes: &[u8]) -> Result<Combat> {
        let mut r = Reader(bytes);
        if &r.take::<4>()? != MAGIC {
            return malformed("combat data does not start with its signature");
        }
        let mut weapons = Vec::new();
        for _ in 0..r.count()? {
            let tag_index = r.u16()?;
            let name_len = r.count()?;
            let Some((name, rest)) = r.0.split_at_checked(name_len) else {
                return malformed("combat data is cut short");
            };
            let name = String::from_utf8_lossy(name).into_owned();
            r.0 = rest;
            let flags = r.u32()?;
            let weapon_type = r.i16()?;
            let secondary_trigger_mode = r.i16()?;
            let heat_recovery_threshold = r.f32()?;
            let heat_overheated_threshold = r.f32()?;
            let heat_detonation_threshold = r.f32()?;
            let heat_loss_per_second = r.f32()?;
            let age_rate_of_fire_penalty = r.f32()?;
            let reload_frames = r.i16()?;
            let recoil_frames = r.i16()?;
            let mut magazines = Vec::new();
            for _ in 0..r.count()? {
                magazines.push(Magazine {
                    flags: r.u32()?,
                    rounds_recharged_per_second: r.i16()?,
                    rounds_total_initial: r.i16()?,
                    rounds_total_maximum: r.i16()?,
                    rounds_loaded_maximum: r.i16()?,
                    reload_time: r.f32()?,
                    rounds_reloaded: r.i16()?,
                    chamber_time: r.f32()?,
                });
            }
            let mut triggers = Vec::new();
            for _ in 0..r.count()? {
                let flags = r.u32()?;
                let initial_rate_of_fire = r.f32()?;
                let final_rate_of_fire = r.f32()?;
                let rate_of_fire_acceleration = r.f32()?;
                let rate_of_fire_deceleration = r.f32()?;
                let magazine_index = r.i16()?;
                let rounds_per_shot = r.i16()?;
                let minimum_rounds_loaded_per_shot = r.i16()?;
                let charging_time = r.f32()?;
                let charged_time = r.f32()?;
                let spew_time = r.f32()?;
                let overloading_time = r.f32()?;
                let projectiles_per_shot = r.i16()?;
                let heat_generated_per_round = r.f32()?;
                let age_generated_per_round = r.f32()?;
                let projectile = match r.u8()? {
                    0 => None,
                    1 => Some(Projectile {
                        flags: r.u32()?,
                        detonation_timer_starts: r.i16()?,
                        timer_lower_bound: r.f32()?,
                        timer_upper_bound: r.f32()?,
                        minimum_velocity: r.f32()?,
                        maximum_range: r.f32()?,
                        air_gravity_scale: r.f32()?,
                        initial_velocity: r.f32()?,
                        final_velocity: r.f32()?,
                        impact_damage: r.opt_damage()?,
                    }),
                    _ => return malformed("combat data has a projectile that is neither there nor not"),
                };
                triggers.push(Trigger {
                    flags,
                    initial_rate_of_fire,
                    final_rate_of_fire,
                    rate_of_fire_acceleration,
                    rate_of_fire_deceleration,
                    magazine_index,
                    rounds_per_shot,
                    minimum_rounds_loaded_per_shot,
                    charging_time,
                    charged_time,
                    spew_time,
                    overloading_time,
                    projectiles_per_shot,
                    heat_generated_per_round,
                    age_generated_per_round,
                    projectile,
                });
            }
            let melee_damage = r.opt_damage()?;
            weapons.push(Weapon {
                tag_index,
                name,
                flags,
                weapon_type,
                secondary_trigger_mode,
                heat_recovery_threshold,
                heat_overheated_threshold,
                heat_detonation_threshold,
                heat_loss_per_second,
                age_rate_of_fire_penalty,
                reload_frames,
                recoil_frames,
                magazines,
                triggers,
                melee_damage,
            });
        }
        let mut resistance = Resistance {
            flags: r.u32()?,
            indirect_damage_material_index: r.i16()?,
            maximum_body_vitality: r.f32()?,
            friendly_damage_resistance: r.f32()?,
            body_destroyed_threshold: r.f32()?,
            maximum_shield_vitality: r.f32()?,
            shield_material_type: r.i16()?,
            shield_failure_function: r.i16()?,
            shield_failure_threshold: r.f32()?,
            maximum_shield_failure: r.f32()?,
            minimum_shield_stun_damage: r.f32()?,
            shield_stun_time: r.f32()?,
            shield_recharge_time: r.f32()?,
            shield_recharge_velocity: r.f32()?,
            materials: Vec::new(),
        };
        for _ in 0..r.count()? {
            resistance.materials.push(DamageMaterial {
                flags: r.u32()?,
                material_type: r.i16()?,
                shield_leak_fraction: r.f32()?,
                shield_damage_multiplier: r.f32()?,
                body_damage_multiplier: r.f32()?,
            });
        }
        let mut starting_equipment = Vec::new();
        for _ in 0..r.count()? {
            let flags = r.u32()?;
            let game_types = [r.i16()?, r.i16()?, r.i16()?, r.i16()?];
            let mut collections = Vec::new();
            for _ in 0..r.count()? {
                let mut permutations = Vec::new();
                for _ in 0..r.count()? {
                    permutations.push((r.f32()?, r.u16()?));
                }
                collections.push(permutations);
            }
            starting_equipment.push(StartingEquipment { flags, game_types, collections });
        }
        if !r.0.is_empty() {
            return malformed("combat data has bytes after its end");
        }
        let combat = Combat { weapons, resistance, starting_equipment };
        combat.check()?;
        Ok(combat)
    }

    /// Whether the numbers can be used: finite, and the lists that the rules
    /// index (a damage's material, a trigger's magazine) in range.
    fn check(&self) -> Result<()> {
        let r = &self.resistance;
        let finite = |vs: &[f32]| vs.iter().all(|v| v.is_finite());
        if !finite(&[
            r.maximum_body_vitality,
            r.friendly_damage_resistance,
            r.body_destroyed_threshold,
            r.maximum_shield_vitality,
            r.shield_failure_threshold,
            r.maximum_shield_failure,
            r.minimum_shield_stun_damage,
            r.shield_stun_time,
            r.shield_recharge_time,
            r.shield_recharge_velocity,
        ]) {
            return malformed("combat data has a resistance that is not a number");
        }
        if r.materials.iter().any(|m| {
            !(0..MATERIAL_TYPES as i16).contains(&m.material_type)
                || !finite(&[m.shield_leak_fraction, m.shield_damage_multiplier, m.body_damage_multiplier])
        }) {
            return malformed("combat data has a body part with a material that is not one");
        }
        if r.maximum_shield_vitality > 0.0 && !(0..MATERIAL_TYPES as i16).contains(&r.shield_material_type) {
            return malformed("combat data has a shield with a material that is not one");
        }
        let damage_ok = |d: &Option<Damage>| {
            d.is_none_or(|d| finite(&[d.minimum, d.lower, d.upper]) && finite(&d.material_modifiers))
        };
        for w in &self.weapons {
            if !damage_ok(&w.melee_damage) {
                return malformed("combat data has a damage that is not a number");
            }
            for t in &w.triggers {
                if t.magazine_index >= 0 && t.magazine_index as usize >= w.magazines.len() {
                    return malformed("combat data has a trigger whose magazine is not one");
                }
                if !finite(&[
                    t.initial_rate_of_fire,
                    t.final_rate_of_fire,
                    t.rate_of_fire_acceleration,
                    t.rate_of_fire_deceleration,
                    t.charging_time,
                    t.charged_time,
                    t.spew_time,
                    t.overloading_time,
                    t.heat_generated_per_round,
                    t.age_generated_per_round,
                ]) {
                    return malformed("combat data has a trigger with a number that is not one");
                }
                if let Some(p) = &t.projectile {
                    if !damage_ok(&p.impact_damage)
                        || !finite(&[
                            p.timer_lower_bound,
                            p.timer_upper_bound,
                            p.maximum_range,
                            p.initial_velocity,
                            p.final_velocity,
                        ])
                    {
                        return malformed("combat data has a projectile with a number that is not one");
                    }
                }
            }
            if !finite(&[
                w.heat_recovery_threshold,
                w.heat_overheated_threshold,
                w.heat_detonation_threshold,
                w.heat_loss_per_second,
                w.age_rate_of_fire_penalty,
            ]) || w.magazines.iter().any(|m| !finite(&[m.reload_time, m.chamber_time]))
            {
                return malformed("combat data has a weapon with a number that is not one");
            }
        }
        for s in &self.starting_equipment {
            if s.collections.iter().flatten().any(|(weight, _)| !weight.is_finite() || *weight < 0.0) {
                return malformed("combat data has a starting weapon with a weight that is not one");
            }
        }
        Ok(())
    }
}
