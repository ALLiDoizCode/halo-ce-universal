//! Where a hit's damage came from: which of a weapon's damages a report names,
//! and what the tags say of how often it can be dealt, how far it reaches and
//! how much of it a hit can carry.
//!
//! A weapon has several damages (`halo_map::combat::Damage`): what a
//! projectile does where it hits ([`Kind::Impact`]), the explosion it makes
//! when it detonates ([`Kind::Detonation`], [`Kind::SuperDetonation`]), what
//! one stuck to a unit does to it ([`Kind::Attached`]), and the weapon's blow in
//! melee ([`Kind::Melee`]). A client reports a hit by the damage's tag
//! ([`halo_map::combat::Damage::tag_index`], what the engine's own damage
//! names), and [`find`] says which weapon's damage it is.
//!
//! # What a hit can carry
//!
//! The engine deals a damage at a *scale* between 0 and 1 (a melee blow's, up
//! to 1.5): `(1 - scale) * minimum + roll * scale` ([`crate::damage::roll`]).
//! The client's engine knows it (how far the projectile had slowed, how far the
//! target was from the explosion, how fast the blow was struck) and reports it;
//! the server cannot see all of that, but it can bound it from what it does
//! see, and a scale over the bound is brought down to it ([`limit`]). A
//! cheating client gains nothing by a scale that is too low, and one that is too
//! high is held to what the shooter's distance to the impact allows.

use halo_map::combat::{damage_effect_flags, Damage, Projectile, Trigger, Weapon};

use crate::weapon::{trigger_rate, MELEE_SPEEDUP_SHIFT};
use crate::TICKS_PER_SECOND;

/// What a damage is of its weapon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A projectile hits a unit.
    Impact,
    /// A projectile detonates: an explosion, to everything within its radius.
    Detonation,
    /// Projectiles that stuck to one unit detonate together.
    SuperDetonation,
    /// A projectile stuck to a unit detonates, and hurts it.
    Attached,
    /// The weapon's blow.
    Melee,
}

/// One damage of a weapon, and what the weapon's tags say of what deals it.
#[derive(Debug, Clone, Copy)]
pub struct Source<'a> {
    pub weapon: &'a Weapon,
    pub kind: Kind,
    pub damage: &'a Damage,
    /// The trigger that fires the projectile (a melee blow has none).
    pub trigger: Option<&'a Trigger>,
    /// Which of the weapon's triggers.
    pub trigger_index: usize,
    pub projectile: Option<&'a Projectile>,
}

impl Source<'_> {
    /// The damage reaches everything around a point, less the further it is
    /// from it (an explosion), and not what it touches only.
    pub fn is_area(&self) -> bool {
        self.damage.cutoff_radius > 0.0 && self.kind != Kind::Melee
    }
}

/// The weapon's damage that has this tag, if it has one.
pub fn find(weapon: &Weapon, damage_tag: u16) -> Option<Source<'_>> {
    for (trigger_index, trigger) in weapon.triggers.iter().enumerate() {
        let Some(projectile) = trigger.projectile.as_ref() else { continue };
        let source = |kind, damage| Source {
            weapon,
            kind,
            damage,
            trigger: Some(trigger),
            trigger_index,
            projectile: Some(projectile),
        };
        let listed = projectile
            .impact_damage
            .iter()
            .map(|d| (Kind::Impact, d))
            .chain(projectile.detonation_damage.iter().map(|d| (Kind::Detonation, d)))
            .chain(projectile.super_detonation_damage.iter().map(|d| (Kind::SuperDetonation, d)))
            .chain(projectile.attached_damage.iter().map(|d| (Kind::Attached, d)));
        for (kind, damage) in listed {
            if damage.tag_index == damage_tag {
                return Some(source(kind, damage));
            }
        }
    }
    weapon.melee_damage.as_ref().filter(|d| d.tag_index == damage_tag).map(|damage| Source {
        weapon,
        kind: Kind::Melee,
        damage,
        trigger: None,
        trigger_index: 0,
        projectile: None,
    })
}

/// How many hits a second the weapon's source can deal at the most, for one
/// target: a melee blow as often as the player can swing, and a projectile's
/// impact as often as the trigger fires it, a shot's pellets each (the
/// pellets of a shotgun all land at once). An explosion counts once for the
/// shot that made it, and what it hurts all at once is one explosion: see
/// [`crate::combat`].
pub fn hit_rate(source: &Source<'_>) -> f32 {
    match source.kind {
        Kind::Melee => melee_rate(source.weapon),
        Kind::Impact => {
            let pellets = source.trigger.map_or(1, |t| t.projectiles_per_shot.max(1));
            trigger_rate(source.weapon, source.trigger_index) * f32::from(pellets)
        }
        Kind::Detonation | Kind::SuperDetonation | Kind::Attached => trigger_rate(source.weapon, source.trigger_index),
    }
}

/// How many blows a second a player can strike with the weapon: the first-person
/// melee animation's length, less the quarter the engine takes off it (`biped_update`),
/// is a blow's duration. A weapon with no animation is read as one a second.
pub fn melee_rate(weapon: &Weapon) -> f32 {
    let frames = weapon.melee_frames.max(0);
    let ticks = frames - (frames >> MELEE_SPEEDUP_SHIFT);
    if ticks > 0 {
        TICKS_PER_SECOND as f32 / f32::from(ticks)
    } else {
        1.0
    }
}

/// How far the damage's projectile can be from where it was fired.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reach {
    /// World units from where the shooter was when they fired.
    pub range: f32,
    /// How many ticks it can be in flight at the most, a lob's arc included.
    pub flight_ticks: u64,
}

/// The most world units the projectile of a weapon flies: its range, or where
/// it has slowed to its final velocity (the engine takes a projectile with no
/// range off at that distance), `None` where the tags bound neither.
fn range_of(projectile: &Projectile) -> Option<f32> {
    if projectile.maximum_range > 0.0 {
        Some(projectile.maximum_range)
    } else if projectile.air_damage_range_upper > 0.0 {
        Some(projectile.air_damage_range_upper)
    } else {
        None
    }
}

/// How far the source's projectile reaches and how long it takes, where the
/// tags bound them; a melee blow has no flight (see [`MELEE_REACH`]).
pub fn reach(source: &Source<'_>) -> Option<Reach> {
    let projectile = source.projectile?;
    let range = range_of(projectile)?;
    let slowest = projectile.initial_velocity.min(projectile.final_velocity);
    // (twice as long: a lob's arc)
    let flight = if slowest > 0.0 { 2.0 * range / slowest } else { f32::MAX };
    let flight_ticks = if flight < 1.0e6 { flight as u64 + 1 } else { 1_000_000 };
    Some(Reach { range, flight_ticks })
}

/// World units: how far ahead of the shooter's head a melee blow lands (the
/// engine's rays are 0.8 long and spread 0.1 to each side).
pub const MELEE_REACH: f32 = 1.0;

/// The most a projectile's impact can be scaled by after it has flown at least `distance`
/// world units: the scale is how much of its initial speed it has kept
/// (`(speed - final) / (initial - final)`, `projectile_collision`), so this flies the
/// projectile tick by tick as the engine does (`projectile_update`) and says what
/// the speed it had at the start of the tick it passed `distance` in gives.
///
/// The engine slows a projectile with a constant deceleration, that of coming down from
/// the initial velocity to the final one over the distance between the damage ranges of its
/// tag, but starts it when a timer has run (`projectile_calculate_deceleration`: the timer
/// gains `lower range / initial velocity` a tick, which is a tick count that does not
/// measure the lower range: a shotgun's pellets, 4.7 a tick, fly 4 ticks, 17 units, at full
/// speed, and then lose it all in one; a plasma pistol's bolt starts to slow at once). That
/// is what the numbers of the tags do in the engine, and what this follows.
pub fn impact_scale(projectile: &Projectile, distance: f32) -> f32 {
    let (v0, vf) = (projectile.initial_velocity, projectile.final_velocity);
    if v0 == vf {
        return 1.0;
    }
    let scale_of = |speed: f32| ((speed - vf) / (v0 - vf)).clamp(0.0, 1.0);
    let lower = projectile.air_damage_range_lower;
    let span = projectile.air_damage_range_upper - lower;
    let deceleration = if span != 0.0 { (v0 * v0 - vf * vf) / (2.0 * span) } else { 0.0 };
    let (mut timer, delta) = if lower > 0.0 { (0.0, lower / v0) } else { (1.0, 0.0) };
    let (mut speed, mut flown) = (v0, 0.0);
    // (a flight is a few hundred ticks at the most: the rocket's)
    for _ in 0..4096 {
        timer += delta;
        let start = speed;
        let mut average = speed;
        if timer >= 1.0 {
            if speed > vf && deceleration != 0.0 {
                let end = speed - deceleration;
                if end <= vf {
                    let fraction = (speed - vf) / deceleration;
                    speed = vf * 0.99;
                    average = (speed + start) * fraction * 0.5 + (1.0 - fraction) * vf;
                } else {
                    average = start - deceleration * 0.5;
                    speed = end;
                }
            } else if speed < vf && speed > 0.0 {
                speed = vf * 0.99;
            }
        }
        flown += average;
        if flown >= distance {
            return scale_of(start);
        }
        if average <= 0.0 {
            break;
        }
    }
    0.0
}

/// The damage scale of an explosion on a unit whose centre is `distance` from
/// its epicentre (`area_of_effect_cause_damage_to_object`): 1 within the
/// damage effect's falloff radius, less out to its cutoff radius, and none
/// beyond; always 1 for a damage effect that does not scale by distance.
pub fn splash_scale(damage: &Damage, distance: f32) -> f32 {
    if damage.effect_flags & damage_effect_flags::DONT_SCALE_DAMAGE_BY_DISTANCE != 0 {
        return if distance <= damage.cutoff_radius { 1.0 } else { 0.0 };
    }
    let delta = damage.cutoff_radius - damage.falloff_radius;
    if delta > 0.0 {
        (1.0 - (distance - damage.falloff_radius) / delta).clamp(0.0, 1.0)
    } else {
        1.0
    }
}

/// What a melee blow's scale can be at the most: its speed against a run's
/// (1), and 1.5 for one struck after a long fall (`unit_cause_player_melee_damage`).
pub const MELEE_SCALE_AIRBORNE: f32 = 1.5;

/// The scale a melee blow is struck at (`unit_cause_player_melee_damage`): how fast the
/// player moves ahead, `velocity` in world units a tick and `forward` the way they face, against
/// the speed they run at; 1.5 after more than 15 ticks in the air.
pub fn melee_scale(velocity: [f32; 3], forward: [f32; 3], run_forward_speed: f32, airborne_ticks: i16) -> f32 {
    let mut scale = 1.0;
    if run_forward_speed > 0.0 {
        let ahead = velocity[0] * forward[0] + velocity[1] * forward[1] + velocity[2] * forward[2];
        scale = (ahead * TICKS_PER_SECOND as f32 / run_forward_speed).clamp(0.0, 1.0);
    }
    if airborne_ticks > 15 {
        scale = MELEE_SCALE_AIRBORNE;
    }
    scale
}

/// How far from a unit's origin the engine's measure of where it is (the
/// centre of its bounding sphere) can be: an explosion's distance to the
/// unit is at least the distance to its origin less this.
pub const CENTRE_OFFSET: f32 = 0.7;

/// What a hit's scale can be at the most, from what the server sees.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScaleBounds {
    /// The least distance, in world units, the projectile can have flown (the
    /// shooter's distance to the impact, less what the server cannot see of
    /// where they fired from), where it is known.
    pub flown: Option<f32>,
    /// The distance from the epicentre to the target's origin.
    pub epicentre_to_target: f32,
    /// The shooter was in the air.
    pub airborne: bool,
}

/// The scale to deal a hit at: what the client says, brought down to the most
/// the source allows.
pub fn limit(source: &Source<'_>, reported: f32, bounds: &ScaleBounds) -> f32 {
    let most = match source.kind {
        Kind::Melee => {
            if bounds.airborne {
                MELEE_SCALE_AIRBORNE
            } else {
                1.0
            }
        }
        Kind::Impact => match (source.projectile, bounds.flown) {
            (Some(p), Some(flown)) => impact_scale(p, flown),
            _ => 1.0,
        },
        Kind::Detonation | Kind::SuperDetonation => {
            splash_scale(source.damage, (bounds.epicentre_to_target - CENTRE_OFFSET).max(0.0))
        }
        Kind::Attached => 1.0,
    };
    reported.clamp(0.0, most)
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;
    use crate::fixtures::{pistol, pistol_damage};

    fn rocket_like() -> Weapon {
        let mut weapon = pistol();
        weapon.tag_index = 1100;
        let mut blast = pistol_damage();
        blast.tag_index = 1135;
        blast.falloff_radius = 0.5;
        blast.cutoff_radius = 2.0;
        let projectile = weapon.triggers[0].projectile.as_mut().unwrap();
        projectile.impact_damage = None;
        projectile.detonation_damage = Vec::from([blast]);
        projectile.maximum_range = 128.0;
        projectile.initial_velocity = 0.4;
        projectile.final_velocity = 0.333;
        weapon.triggers[0].initial_rate_of_fire = 0.5;
        weapon.triggers[0].final_rate_of_fire = 0.5;
        weapon
    }

    #[test]
    fn a_damage_is_found_by_its_tag_among_the_weapons() {
        let mut weapon = pistol();
        let mut melee = pistol_damage();
        melee.tag_index = 489;
        weapon.melee_damage = Some(melee);
        assert_eq!(find(&weapon, 517).map(|s| s.kind), Some(Kind::Impact));
        assert_eq!(find(&weapon, 489).map(|s| s.kind), Some(Kind::Melee));
        assert!(find(&weapon, 518).is_none());
        let rocket = rocket_like();
        assert_eq!(find(&rocket, 1135).map(|s| (s.kind, s.is_area())), Some((Kind::Detonation, true)));
    }

    #[test]
    fn a_melee_blow_is_as_frequent_as_the_swing_and_a_pellet_as_the_shotgun_fires() {
        let mut weapon = pistol();
        weapon.melee_frames = 40;
        // the engine takes a quarter off: a blow takes 30 ticks
        assert_eq!(melee_rate(&weapon), 1.0);
        weapon.melee_frames = 20;
        assert_eq!(melee_rate(&weapon), 2.0);
        weapon.melee_frames = 0;
        assert_eq!(melee_rate(&weapon), 1.0);
        let mut shotgun = pistol();
        shotgun.triggers[0].initial_rate_of_fire = 1.0;
        shotgun.triggers[0].final_rate_of_fire = 1.0;
        shotgun.triggers[0].projectiles_per_shot = 15;
        shotgun.triggers[0].projectile.as_mut().unwrap().impact_damage.as_mut().unwrap().tag_index = 962;
        let source = find(&shotgun, 962).unwrap();
        assert_eq!(hit_rate(&source), 15.0);
    }

    #[test]
    fn a_projectile_reaches_as_far_as_its_range_and_a_slow_one_takes_long() {
        let weapon = pistol();
        let source = find(&weapon, 517).unwrap();
        // 40 units at 10 a tick, twice over for a lob, and one more
        assert_eq!(reach(&source), Some(Reach { range: 40.0, flight_ticks: 9 }));
        let rocket = rocket_like();
        let source = find(&rocket, 1135).unwrap();
        let r = reach(&source).unwrap();
        assert_eq!(r.range, 128.0);
        assert!(r.flight_ticks > 700 && r.flight_ticks < 800, "{r:?}");
        // no range, but it slows to a stop in the damage range
        let mut rifle = weapon;
        let p = rifle.triggers[0].projectile.as_mut().unwrap();
        p.impact_damage.as_mut().unwrap().tag_index = 5;
        p.maximum_range = 0.0;
        p.air_damage_range_upper = 50.0;
        assert_eq!(reach(&find(&rifle, 5).unwrap()).unwrap().range, 50.0);
        let p = rifle.triggers[0].projectile.as_mut().unwrap();
        p.air_damage_range_upper = 0.0;
        assert_eq!(reach(&find(&rifle, 5).unwrap()), None);
    }

    #[test]
    fn a_projectiles_damage_falls_as_the_engine_slows_it() {
        let mut p = pistol().triggers[0].projectile.clone().unwrap();
        // (a bullet that does not slow down does all its damage at any range)
        assert_eq!(impact_scale(&p, 500.0), 1.0);
        // the shotgun's pellets: 4.667 a tick slowing to 3.333 between 1.5 and 3 units: the
        // engine's timer starts the slowing on the fourth tick
        p.initial_velocity = 4.666_667;
        p.final_velocity = 3.333_333_5;
        p.air_damage_range_lower = 1.5;
        p.air_damage_range_upper = 3.0;
        assert_eq!(impact_scale(&p, 0.0), 1.0);
        assert_eq!(impact_scale(&p, 3.0), 1.0, "a target at the end of the barrel takes all of it");
        assert_eq!(impact_scale(&p, 14.0), 1.0, "three ticks at full speed");
        assert_eq!(impact_scale(&p, 16.0), 1.0, "(the fourth tick starts at full speed, and loses it by its end)");
        assert_eq!(impact_scale(&p, 20.0), 0.0);
        // the plasma pistol's bolt: 0.833 a tick slowing to 0.5, from the first tick
        p.initial_velocity = 0.833_333_4;
        p.final_velocity = 0.5;
        p.air_damage_range_lower = 20.0;
        p.air_damage_range_upper = 50.0;
        assert_eq!(impact_scale(&p, 0.5), 1.0);
        let near = impact_scale(&p, 5.0);
        let middle = impact_scale(&p, 20.0);
        let far = impact_scale(&p, 29.0);
        assert!(1.0 > near && near > middle && middle > far && far > 0.0, "{near} {middle} {far}");
        assert!((near - 0.87).abs() < 0.02 && (middle - 0.40).abs() < 0.02, "{near} {middle}");
        assert_eq!(impact_scale(&p, 80.0), 0.0);
    }

    #[test]
    fn an_explosions_damage_is_full_to_the_falloff_and_nothing_past_the_cutoff() {
        let mut d = pistol_damage();
        d.falloff_radius = 0.5;
        d.cutoff_radius = 2.0;
        assert_eq!(splash_scale(&d, 0.0), 1.0);
        assert_eq!(splash_scale(&d, 0.5), 1.0);
        assert_eq!(splash_scale(&d, 1.25), 0.5);
        assert_eq!(splash_scale(&d, 2.0), 0.0);
        assert_eq!(splash_scale(&d, 9.0), 0.0);
        d.effect_flags = damage_effect_flags::DONT_SCALE_DAMAGE_BY_DISTANCE;
        assert_eq!(splash_scale(&d, 1.25), 1.0);
        assert_eq!(splash_scale(&d, 2.5), 0.0);
        d.effect_flags = 0;
        d.falloff_radius = 2.0;
        assert_eq!(splash_scale(&d, 1.9), 1.0, "no ramp to scale by");
    }

    #[test]
    fn a_scale_is_brought_down_to_what_the_shooters_distance_and_the_blasts_allow() {
        let rocket = rocket_like();
        let blast = find(&rocket, 1135).unwrap();
        let near = ScaleBounds { flown: None, epicentre_to_target: 0.5, airborne: false };
        assert_eq!(limit(&blast, 1.0, &near), 1.0);
        let far = ScaleBounds { epicentre_to_target: 1.7 + CENTRE_OFFSET, ..near };
        // a unit 1.7 from the blast has at most (2 - 1.7) / 1.5 of it
        assert!((limit(&blast, 1.0, &far) - 0.2).abs() < 1.0e-6);
        assert_eq!(limit(&blast, 0.1, &far), 0.1, "less is fine");
        assert_eq!(limit(&blast, -4.0, &far), 0.0);
        let out = ScaleBounds { epicentre_to_target: 5.0, ..near };
        assert_eq!(limit(&blast, 1.0, &out), 0.0);

        let mut weapon = pistol();
        weapon.triggers[0].projectile.as_mut().unwrap().impact_damage.as_mut().unwrap().tag_index = 7;
        let slow = {
            let p = weapon.triggers[0].projectile.as_mut().unwrap();
            p.initial_velocity = 0.8;
            p.final_velocity = 0.5;
            p.air_damage_range_lower = 20.0;
            p.air_damage_range_upper = 50.0;
            find(&weapon, 7).unwrap()
        };
        let flown = |d| ScaleBounds { flown: Some(d), epicentre_to_target: 0.0, airborne: false };
        assert_eq!(limit(&slow, 1.0, &flown(0.5)), 1.0);
        assert!(limit(&slow, 1.0, &flown(10.0)) < 1.0);
        assert_eq!(limit(&slow, 1.0, &flown(60.0)), 0.0);
        assert_eq!(limit(&slow, 1.0, &ScaleBounds { flown: None, ..flown(0.0) }), 1.0, "no bound known");

        let mut melee = pistol_damage();
        melee.tag_index = 8;
        weapon.melee_damage = Some(melee);
        let blow = find(&weapon, 8).unwrap();
        assert_eq!(limit(&blow, 5.0, &flown(0.0)), 1.0);
        assert_eq!(limit(&blow, 5.0, &ScaleBounds { airborne: true, ..flown(0.0) }), MELEE_SCALE_AIRBORNE);
    }
}
