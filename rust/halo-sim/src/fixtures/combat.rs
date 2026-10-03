//! Fixtures for the fighting: the pistol and the player's body, as the tags of
//! the maps have them.

use alloc::vec::Vec;

use halo_map::combat::{
    damage_flags, Combat, Damage, DamageMaterial, Magazine, Projectile, Resistance, Trigger, Weapon, MATERIAL_HEAD,
    MATERIAL_TYPES,
};

/// The tag index of [`pistol`].
pub const PISTOL: u16 = 476;
/// The tag index of the damage effect of [`pistol`]'s bullet.
pub const PISTOL_DAMAGE: u16 = 517;

/// What a pistol's bullet deals, as the tags of the maps have it: 25 a hit
/// (always), a head that can kill outright, and a flesh that takes half as
/// much again as a shield does.
pub fn pistol_damage() -> Damage {
    let mut material_modifiers = [0.0; MATERIAL_TYPES];
    material_modifiers[21] = 1.5;
    material_modifiers[22] = 1.0;
    Damage {
        tag_index: PISTOL_DAMAGE,
        side_effect: 0,
        category: 2,
        flags: damage_flags::CAN_CAUSE_HEADSHOTS,
        falloff_radius: 0.0,
        cutoff_radius: 0.0,
        cutoff_scale: 0.0,
        effect_flags: 0,
        core_radius: 0.0,
        minimum: 25.0,
        lower: 25.0,
        upper: 25.0,
        material_modifiers,
    }
}

/// The multiplayer pistol, as the tags of the maps have it: 3.5 rounds a
/// second, a magazine of 12 and 60 rounds to start with.
pub fn pistol() -> Weapon {
    Weapon {
        tag_index: PISTOL,
        name: "weapons\\pistol\\pistol.weap".into(),
        flags: 0,
        weapon_type: 0,
        secondary_trigger_mode: 0,
        heat_recovery_threshold: 0.0,
        heat_overheated_threshold: 0.0,
        heat_detonation_threshold: 0.0,
        heat_loss_per_second: 0.0,
        age_rate_of_fire_penalty: 0.0,
        age_heat_recovery_penalty: 0.0,
        age_misfire_start: 0.0,
        age_misfire_chance: 0.0,
        overheated_explosion_fraction: 0.0,
        reload_frames: 67,
        recoil_frames: 5,
        melee_frames: 0,
        melee_key_frame: 0,
        shotgun_enter_frames: 0,
        magazines: Vec::from([Magazine {
            flags: 0,
            rounds_recharged_per_second: 0,
            rounds_total_initial: 60,
            rounds_total_maximum: 120,
            rounds_loaded_maximum: 12,
            reload_time: 2.17,
            rounds_reloaded: 12,
            chamber_time: 0.0,
        }]),
        triggers: Vec::from([Trigger {
            flags: 0,
            initial_rate_of_fire: 3.5,
            final_rate_of_fire: 3.5,
            rate_of_fire_acceleration: 1.0,
            rate_of_fire_deceleration: 1.0,
            magazine_index: 0,
            rounds_per_shot: 1,
            minimum_rounds_loaded_per_shot: 0,
            charging_time: 0.0,
            charged_time: 0.0,
            overcharged_action: 0,
            spew_time: 0.0,
            overloading_time: 0.0,
            projectiles_per_shot: 1,
            heat_generated_per_round: 0.0,
            age_generated_per_round: 0.0,
            projectile: Some(Projectile {
                flags: 0,
                detonation_timer_starts: 0,
                timer_lower_bound: 0.0,
                timer_upper_bound: 0.0,
                minimum_velocity: 0.0,
                maximum_range: 40.0,
                air_gravity_scale: 0.0,
                air_damage_range_lower: 0.0,
                air_damage_range_upper: 100.0,
                initial_velocity: 10.0,
                final_velocity: 10.0,
                impact_damage: Some(pistol_damage()),
                detonation_damage: Vec::new(),
                super_detonation_damage: Vec::new(),
                attached_damage: None,
            }),
        }]),
        melee_damage: None,
    }
}

/// The tag indices of [`shotgun`], its pellet's damage effect and its melee blow's.
pub const SHOTGUN: u16 = 918;
pub const SHOTGUN_DAMAGE: u16 = 962;
pub const SHOTGUN_MELEE: u16 = 936;
/// The tag indices of [`rocket_launcher`], its explosion's damage effect and its melee blow's.
pub const ROCKET_LAUNCHER: u16 = 1100;
pub const ROCKET_BLAST: u16 = 1135;
pub const ROCKET_MELEE: u16 = 1113;
/// The tag indices of [`sniper_rifle`], its bullet's damage effect and its melee blow's.
pub const SNIPER_RIFLE: u16 = 1172;
pub const SNIPER_RIFLE_DAMAGE: u16 = 1219;
/// The tag indices of [`plasma_pistol`], its bolt's damage effect, its overcharged bolt's and its melee blow's.
pub const PLASMA_PISTOL: u16 = 1281;
pub const PLASMA_PISTOL_DAMAGE: u16 = 1317;
pub const PLASMA_PISTOL_CHARGED_DAMAGE: u16 = 1349;
/// The tag indices of [`plasma_rifle`], its bolt's damage effect and its melee blow's.
pub const PLASMA_RIFLE: u16 = 1005;
pub const PLASMA_RIFLE_DAMAGE: u16 = 1046;

/// A damage of a weapon of the maps: a flesh and a shield that each take it as it is.
fn damage(tag_index: u16, minimum: f32, lower: f32, upper: f32) -> Damage {
    let mut material_modifiers = [0.0; MATERIAL_TYPES];
    material_modifiers[21] = 1.0;
    material_modifiers[22] = 1.0;
    Damage {
        tag_index,
        side_effect: 0,
        category: 0,
        flags: 0,
        falloff_radius: 0.0,
        cutoff_radius: 0.0,
        cutoff_scale: 0.0,
        effect_flags: 0,
        core_radius: 0.0,
        minimum,
        lower,
        upper,
        material_modifiers,
    }
}

/// What every weapon of the maps does with its melee blow: 40 to 60, in a reach of half a unit.
fn melee(tag_index: u16) -> Damage {
    Damage { falloff_radius: 0.5, cutoff_radius: 0.5, flags: 0x5, ..damage(tag_index, 40.0, 50.0, 60.0) }
}

/// A weapon of the fixtures, with the tags' values that the rules read and the pistol's others.
#[allow(clippy::too_many_arguments)]
fn weapon_like_the_pistol(
    tag_index: u16,
    name: &str,
    trigger: Trigger,
    magazine: Magazine,
    melee_damage: Damage,
    frames: (i16, i16, i16),
) -> Weapon {
    Weapon {
        tag_index,
        name: name.into(),
        melee_damage: Some(melee_damage),
        reload_frames: frames.0,
        melee_frames: frames.1,
        melee_key_frame: frames.2,
        recoil_frames: 0,
        magazines: Vec::from([magazine]),
        triggers: Vec::from([trigger]),
        ..pistol()
    }
}

fn projectile(
    initial_velocity: f32,
    final_velocity: f32,
    range: f32,
    air_damage_range: (f32, f32),
    impact_damage: Option<Damage>,
    detonation_damage: Vec<Damage>,
) -> Projectile {
    Projectile {
        flags: 0,
        detonation_timer_starts: 0,
        timer_lower_bound: 0.0,
        timer_upper_bound: 0.0,
        minimum_velocity: 0.0,
        maximum_range: range,
        air_gravity_scale: 0.0,
        air_damage_range_lower: air_damage_range.0,
        air_damage_range_upper: air_damage_range.1,
        initial_velocity,
        final_velocity,
        impact_damage,
        detonation_damage,
        super_detonation_damage: Vec::new(),
        attached_damage: None,
    }
}

/// The multiplayer shotgun, as the tags of the maps have it: fifteen pellets a shot, one shot
/// a second, a pellet that deals 18 to 25 and loses all but 8 of it past about 17 units, twelve
/// shells loaded at a time, one at a time.
pub fn shotgun() -> Weapon {
    let trigger = Trigger {
        initial_rate_of_fire: 1.0,
        final_rate_of_fire: 1.0,
        projectiles_per_shot: 15,
        projectile: Some(projectile(
            4.666_667,
            3.333_333_5,
            40.0,
            (1.5, 3.0),
            Some(damage(SHOTGUN_DAMAGE, 8.0, 18.0, 25.0)),
            Vec::new(),
        )),
        ..pistol().triggers.remove(0)
    };
    let magazine = Magazine {
        flags: 2,
        rounds_total_initial: 24,
        rounds_total_maximum: 60,
        rounds_loaded_maximum: 12,
        reload_time: 0.4,
        rounds_reloaded: 1,
        ..pistol().magazines.remove(0)
    };
    Weapon {
        weapon_type: 1,
        shotgun_enter_frames: 15,
        ..weapon_like_the_pistol(
            SHOTGUN,
            "weapons\\shotgun\\shotgun.weap",
            trigger,
            magazine,
            melee(SHOTGUN_MELEE),
            (12, 36, 4),
        )
    }
}

/// The multiplayer rocket launcher, as the tags of the maps have it: a rocket every two
/// seconds that flies 0.4 units a tick for up to 128 units (a rocket takes 13 seconds to
/// fly its range) and makes an explosion of 300 to 330 within 2 units (all of it to half a unit), two
/// rockets loaded at a time.
pub fn rocket_launcher() -> Weapon {
    let mut blast = damage(ROCKET_BLAST, 80.0, 300.0, 330.0);
    blast.flags = 0x20;
    blast.falloff_radius = 0.5;
    blast.cutoff_radius = 2.0;
    blast.core_radius = 0.6;
    let trigger = Trigger {
        flags: 0x8,
        initial_rate_of_fire: 0.5,
        final_rate_of_fire: 0.5,
        projectile: Some(projectile(0.4, 0.333_333_34, 128.0, (0.0, 100.0), None, Vec::from([blast]))),
        ..pistol().triggers.remove(0)
    };
    let magazine = Magazine {
        rounds_total_initial: 4,
        rounds_total_maximum: 8,
        rounds_loaded_maximum: 2,
        reload_time: 5.0,
        rounds_reloaded: 2,
        ..pistol().magazines.remove(0)
    };
    weapon_like_the_pistol(
        ROCKET_LAUNCHER,
        "weapons\\rocket launcher\\rocket launcher.weap",
        trigger,
        magazine,
        melee(ROCKET_MELEE),
        (125, 52, 4),
    )
}

/// The multiplayer plasma pistol, as the tags of the maps have it: a first trigger that charges for 0.6 s and
/// fires a bolt of 16 to 20 when the button is let go before the charge is full, and a second that fires an
/// overcharged bolt of 70 when it is let go after; each shot makes heat (a sixth of the gauge, and all of it)
/// and ages the battery.
pub fn plasma_pistol() -> Weapon {
    let mut bolt = damage(PLASMA_PISTOL_DAMAGE, 10.0, 16.0, 20.0);
    bolt.flags = 0x4;
    let mut charged = damage(PLASMA_PISTOL_CHARGED_DAMAGE, 70.0, 70.0, 70.0);
    charged.flags = 0x4;
    let first = Trigger {
        initial_rate_of_fire: 0.0,
        final_rate_of_fire: 0.0,
        rate_of_fire_acceleration: 1.0,
        rate_of_fire_deceleration: 1.0,
        rounds_per_shot: 0,
        charging_time: 0.6,
        charged_time: 45.0,
        heat_generated_per_round: 0.16,
        age_generated_per_round: 0.002,
        projectile: Some(projectile(0.833_333_4, 0.5, 50.0, (20.0, 50.0), Some(bolt), Vec::new())),
        ..pistol().triggers.remove(0)
    };
    let second = Trigger {
        flags: 0x4,
        charging_time: 0.0,
        charged_time: 0.0,
        overcharged_action: 1,
        heat_generated_per_round: 1.0,
        age_generated_per_round: 0.11,
        projectile: Some(projectile(0.5, 0.3, 40.0, (5.0, 20.0), Some(charged), Vec::new())),
        ..first.clone()
    };
    let magazine = Magazine {
        rounds_total_initial: 0,
        rounds_total_maximum: 0,
        rounds_loaded_maximum: 0,
        reload_time: 0.0,
        rounds_reloaded: 0,
        ..pistol().magazines.remove(0)
    };
    let mut weapon = Weapon {
        weapon_type: 3,
        flags: 0x800,
        heat_recovery_threshold: 0.25,
        heat_overheated_threshold: 1.0,
        heat_detonation_threshold: 1.0,
        heat_loss_per_second: 0.65,
        age_misfire_start: 0.9,
        age_misfire_chance: 0.5,
        ..weapon_like_the_pistol(
            PLASMA_PISTOL,
            "weapons\\plasma pistol\\plasma pistol.weap",
            first,
            magazine,
            melee(1303),
            (9, 33, 4),
        )
    };
    weapon.triggers.push(second);
    weapon
}

/// The multiplayer sniper rifle, as the tags of the maps have it: a shot twice a second, one a press of
/// the button (the trigger is latched), a bullet of 101 that flies 33 units a tick, four rounds loaded.
pub fn sniper_rifle() -> Weapon {
    let mut bullet = damage(SNIPER_RIFLE_DAMAGE, 101.0, 101.0, 101.0);
    bullet.flags = damage_flags::CAN_CAUSE_HEADSHOTS;
    let trigger = Trigger {
        flags: 0x408,
        initial_rate_of_fire: 2.0,
        final_rate_of_fire: 2.0,
        projectile: Some(projectile(33.333_336, 33.333_336, 1000.0, (1000.0, 1000.0), Some(bullet), Vec::new())),
        ..pistol().triggers.remove(0)
    };
    let magazine = Magazine {
        rounds_total_initial: 12,
        rounds_total_maximum: 24,
        rounds_loaded_maximum: 4,
        reload_time: 0.0,
        rounds_reloaded: 4,
        ..pistol().magazines.remove(0)
    };
    Weapon {
        flags: 0x4020,
        recoil_frames: 5,
        ..weapon_like_the_pistol(
            SNIPER_RIFLE,
            "weapons\\sniper rifle\\sniper rifle.weap",
            trigger,
            magazine,
            melee(1189),
            (94, 36, 6),
        )
    }
}

/// The multiplayer plasma rifle, as the tags of the maps have it: a bolt that deals 12 to 14 and
/// slows from 1.7 units a tick to 0.8, a rate of fire that comes up from 7 a second to 10, 8% of the
/// heat gauge for each shot, a battery that is spent a half of one percent at a time and that
/// misfires from nine tenths spent.
pub fn plasma_rifle() -> Weapon {
    let trigger = Trigger {
        initial_rate_of_fire: 7.0,
        final_rate_of_fire: 10.0,
        rate_of_fire_acceleration: 0.018_518_52,
        rate_of_fire_deceleration: 0.133_333_34,
        rounds_per_shot: 0,
        heat_generated_per_round: 0.08,
        age_generated_per_round: 0.005,
        projectile: Some(projectile(
            1.666_666_7,
            0.833_333_4,
            0.0,
            (20.0, 50.0),
            Some(damage(PLASMA_RIFLE_DAMAGE, 10.0, 12.0, 14.0)),
            Vec::new(),
        )),
        ..pistol().triggers.remove(0)
    };
    let magazine = Magazine {
        rounds_total_initial: 0,
        rounds_total_maximum: 0,
        rounds_loaded_maximum: 0,
        reload_time: 0.0,
        rounds_reloaded: 0,
        ..pistol().magazines.remove(0)
    };
    Weapon {
        weapon_type: 4,
        flags: 0x800,
        heat_recovery_threshold: 0.25,
        heat_overheated_threshold: 1.0,
        heat_detonation_threshold: 1.0,
        heat_loss_per_second: 0.3,
        age_rate_of_fire_penalty: 0.2,
        age_heat_recovery_penalty: 0.2,
        age_misfire_start: 0.9,
        age_misfire_chance: 0.5,
        ..weapon_like_the_pistol(
            PLASMA_RIFLE,
            "weapons\\plasma rifle\\plasma rifle.weap",
            trigger,
            magazine,
            melee(1029),
            (0, 36, 6),
        )
    }
}

/// What the multiplayer player's body is, as the tags of the maps have it: 75
/// health and a 75 shield that is stunned for 6 seconds by a hit and then
/// recharges in 4; a head, a body and two limbs.
pub fn resistance() -> Resistance {
    let material = |flags, body_damage_multiplier| DamageMaterial {
        flags,
        material_type: 21,
        shield_leak_fraction: 0.0,
        shield_damage_multiplier: 1.0,
        body_damage_multiplier,
    };
    Resistance {
        flags: 7,
        indirect_damage_material_index: 1,
        maximum_body_vitality: 75.0,
        friendly_damage_resistance: 0.0,
        body_destroyed_threshold: 0.0,
        maximum_shield_vitality: 75.0,
        shield_material_type: 22,
        shield_failure_function: 0,
        shield_failure_threshold: 0.0,
        maximum_shield_failure: 0.0,
        minimum_shield_stun_damage: 0.0,
        shield_stun_time: 6.0,
        shield_recharge_time: 4.0,
        shield_recharge_velocity: 0.008_333_334,
        materials: Vec::from([material(MATERIAL_HEAD, 1.0), material(0, 1.0), material(0, 0.8), material(0, 1.0)]),
    }
}

/// The combat values of the fixture maps: the pistol, and the player's body.
pub fn combat_fixture() -> Combat {
    Combat { weapons: Vec::from([pistol()]), resistance: resistance(), starting_equipment: Vec::new() }
}
