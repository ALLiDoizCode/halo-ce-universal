//! Fixtures for the fighting: the pistol and the player's body, as the tags of
//! the maps have them.

use alloc::vec::Vec;

use halo_map::combat::{
    damage_flags, Combat, Damage, DamageMaterial, Magazine, Projectile, Resistance, Trigger, Weapon, MATERIAL_HEAD,
    MATERIAL_TYPES,
};

/// The tag index of [`pistol`].
pub const PISTOL: u16 = 476;

/// What a pistol's bullet deals, as the tags of the maps have it: 25 a hit
/// (always), a head that can kill outright, and a flesh that takes half as
/// much again as a shield does.
pub fn pistol_damage() -> Damage {
    let mut material_modifiers = [0.0; MATERIAL_TYPES];
    material_modifiers[21] = 1.5;
    material_modifiers[22] = 1.0;
    Damage {
        side_effect: 0,
        category: 2,
        flags: damage_flags::CAN_CAUSE_HEADSHOTS,
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
        reload_frames: 67,
        recoil_frames: 5,
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
                initial_velocity: 10.0,
                final_velocity: 10.0,
                impact_damage: Some(pistol_damage()),
            }),
        }]),
        melee_damage: None,
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
