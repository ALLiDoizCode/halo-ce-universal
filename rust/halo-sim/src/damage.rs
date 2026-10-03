//! Health and shields: what a hit does to a player, and how their shields
//! come back. A port of the engine's own (`source/objects/damage.c`:
//! `object_damage_shield`, `object_damage_body`, the shield part of
//! `object_damage_update`), for one unit that is no vehicle's rider, with the
//! numbers of the map's tags ([`Resistance`], [`Damage`]). It takes the same
//! operations in the same order as the engine, so that the results agree to
//! the last bit (the comparison harness holds it to that).
//!
//! # What the numbers are
//!
//! A player's shield and health are fractions of the full amount the tags
//! give them: [`Vitals::shield`] 1 is a full shield (an overshield goes up to
//! 3), [`Vitals::body`] 1 is full health, and a body below 0 is dead.
//!
//! # A hit
//!
//! [`Vitals::hit`] takes the damage of one hit (a number between the damage
//! tag's lower and upper bound, which [`roll`] draws), and the part of the body
//! it was on. The shield takes its share first (the part the shield leaks
//! to the body aside), what is left of it goes on to the body, and a hit on the
//! head that reaches the body kills at once for a damage that can cause
//! headshots (the pistol), and costs twice as much for one that can in
//! multiplayer. A shield that a hit brings down is stunned, and does not
//! recharge until the stun time has run out.
//!
//! # Time
//!
//! [`Vitals::tick`] is one tick of the shield's recharge. A tick is a call of
//! it and then the tick's hits, as the engine updates an object and then the
//! damage of the tick lands. The server does not tick every player's shield
//! every tick: it keeps the vitals as of a tick and [`Vitals::advance`]s them
//! to the tick a hit lands on (or a client looks at them), which is the same
//! thing.
//!
//! # Not here yet
//!
//! Shield failure (a shield that fails more as it empties) is linear only;
//! the tags of the multiplayer player have none. Regions of the body, damage
//! to a vehicle's rider, and explosions' falloff are for the tickets of those
//! things.

use halo_map::combat::{damage_flags, Damage, DamageMaterial, Resistance, MATERIAL_HEAD};

use crate::rng::Rng;
use crate::TICKS_PER_SECOND;

/// [`Vitals::flags`]: the shield is down (a hit took it to nothing, and it
/// has not begun to come back).
pub const SHIELD_DEPLETED: u8 = 1;
/// [`Vitals::flags`]: the body's vitality is below nothing: the player is dead.
pub const DEAD: u8 = 2;
/// [`Vitals::flags`]: the shield is being overcharged (the overshield powerup).
pub const SHIELD_OVER_CHARGING: u8 = 4;
/// [`Vitals::flags`]: the shield is coming back (a tick of it has recharged).
pub const SHIELD_CHARGING: u8 = 8;

/// `damage_resistance.flags` bits, and the damage side effect, that the rules read.
const ALWAYS_SHIELDS_FRIENDLY_DAMAGE: u32 = 1 << 2;
const ONLY_HURT_BY_EXPLOSIVES: u32 = 1 << 5;
const SIDE_EFFECT_EMP: i16 = 3;

/// An overshield's most, and how fast it comes on and goes down again, a tick's
/// worth (the engine's own constants).
const OVERSHIELD_MAXIMUM: f32 = 3.0;
const OVERSHIELD_CHARGE_RATE: f32 = 0.033_333_335;
const OVERSHIELD_DECAY_RATE: f32 = 0.000_740_740_74;

/// A player's health and shields.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vitals {
    pub shield: f32,
    pub body: f32,
    /// Ticks the shield will not recharge for (it was hit).
    pub shield_stun_ticks: i16,
    /// [`SHIELD_DEPLETED`], [`DEAD`], [`SHIELD_OVER_CHARGING`] and [`SHIELD_CHARGING`].
    pub flags: u8,
}

/// What a hit did: for the logs, the events and the tests.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hurt {
    /// What the shield took of the hit, in the units of the damage tag (the
    /// engine's `shield_damage`).
    pub shield_damage: f32,
    /// What was left of the hit for the body, times the body part's damage
    /// multiplier (the engine's `body_damage`).
    pub body_damage: f32,
    /// The hit brought the shield to nothing.
    pub shield_depleted: bool,
    /// The hit killed the player.
    pub killed: bool,
    /// A hit to the head killed outright.
    pub killed_instantly: bool,
}

/// The material a hit that names no part of the body, in a body with no
/// indirect part either, counts as (the engine's zeroed one): it does nothing.
const NO_MATERIAL: DamageMaterial = DamageMaterial {
    flags: 0,
    material_type: 0,
    shield_leak_fraction: 0.0,
    shield_damage_multiplier: 0.0,
    body_damage_multiplier: 0.0,
};

/// The damage of one hit: a number between the tag's lower and upper bound,
/// as the engine draws it, and the scale of the hit (1 for a projectile's
/// impact) between the tag's minimum and it.
pub fn roll(damage: &Damage, scale: f32, multiplier: f32, rng: &mut Rng) -> f32 {
    // (the engine's real_random_range: lower + (upper - lower) * a number in [0, 1); one that
    // has nothing to choose between does not draw)
    let random = if damage.lower == damage.upper {
        damage.lower
    } else {
        damage.lower + (damage.upper - damage.lower) * rng.next_f32()
    };
    ((1.0 - scale) * damage.minimum + random * scale) * multiplier
}

impl Vitals {
    /// A player at full health and shields (`object_initialize_vitality`).
    pub fn full(res: &Resistance) -> Vitals {
        Vitals {
            shield: if res.maximum_shield_vitality > 0.0 { 1.0 } else { 0.0 },
            body: if res.maximum_body_vitality > 0.0 { 1.0 } else { 0.0 },
            shield_stun_ticks: 0,
            flags: 0,
        }
    }

    pub fn is_dead(&self) -> bool {
        self.flags & DEAD != 0
    }

    /// One tick of the shield's recharge (the shield part of the engine's
    /// `object_damage_update`).
    pub fn tick(&mut self, res: &Resistance) {
        self.flags &= !SHIELD_CHARGING;
        if res.maximum_shield_vitality <= 0.0 || self.is_dead() {
            return;
        }
        if self.flags & SHIELD_OVER_CHARGING != 0 {
            let shield = self.shield + OVERSHIELD_CHARGE_RATE;
            self.shield = shield;
            if shield >= OVERSHIELD_MAXIMUM {
                self.shield = OVERSHIELD_MAXIMUM;
                self.flags &= !SHIELD_OVER_CHARGING;
            } else {
                self.flags |= SHIELD_CHARGING;
            }
        } else if self.shield > 1.0 {
            // an overshield wears off to a full shield
            let overcharge = self.shield - 1.0;
            if OVERSHIELD_DECAY_RATE > overcharge {
                self.shield = 1.0;
            } else {
                self.shield -= OVERSHIELD_DECAY_RATE;
            }
        } else if self.shield < 1.0 {
            if self.shield_stun_ticks == 0 {
                let recharge = res.shield_recharge_velocity;
                self.flags &= !SHIELD_DEPLETED;
                self.flags |= SHIELD_CHARGING;
                let shield = recharge + self.shield;
                self.shield = shield;
                if shield > 1.0 {
                    self.flags &= !SHIELD_CHARGING;
                    self.shield = 1.0;
                }
            } else {
                self.shield_stun_ticks -= 1;
            }
        }
    }

    /// Whether [`Vitals::tick`] would change anything: a full shield with no
    /// stun left to count down does not.
    pub fn is_settled(&self) -> bool {
        self.flags & (SHIELD_CHARGING | SHIELD_OVER_CHARGING) == 0 && self.shield_stun_ticks == 0 && self.shield == 1.0
            || self.is_dead()
    }

    /// `ticks` ticks of [`Vitals::tick`] (it stops early when nothing more
    /// would change).
    pub fn advance(&mut self, res: &Resistance, ticks: u64) {
        for _ in 0..ticks {
            if self.is_settled() {
                // (a full shield's tick clears the charging flag, as the first of these did)
                self.flags &= !SHIELD_CHARGING;
                return;
            }
            self.tick(res);
        }
    }

    /// A hit of `total_damage` (see [`roll`]) on the part of the body the
    /// material index `material` names (-1 for none), by `damage`; `friendly`
    /// is a hit by a player on the same team (the body takes less of it).
    /// Returns what it did, or `None` for a hit that did nothing (no damage).
    /// A player who is dead takes it as the engine's corpse does (the body
    /// goes on below nothing); it is the caller's to refuse a hit on the dead.
    pub fn hit(
        &mut self,
        res: &Resistance,
        damage: &Damage,
        material: i16,
        friendly: bool,
        total_damage: f32,
    ) -> Option<Hurt> {
        if total_damage.is_nan() || total_damage <= 0.0 {
            return None;
        }
        let material = match usize::try_from(material).ok().and_then(|m| res.materials.get(m)) {
            Some(m) => *m,
            None => match usize::try_from(res.indirect_damage_material_index).ok().and_then(|m| res.materials.get(m)) {
                Some(m) => *m,
                None => NO_MATERIAL,
            },
        };
        let mut total = total_damage;
        let mut hurt = Hurt {
            shield_damage: 0.0,
            body_damage: 0.0,
            shield_depleted: false,
            killed: false,
            killed_instantly: false,
        };
        if damage.flags & damage_flags::SKIPS_SHIELDS == 0 && res.maximum_shield_vitality > 0.0 {
            self.hit_shield(res, &material, damage, friendly, &mut total, &mut hurt);
        }
        if damage.flags & damage_flags::ONLY_HURTS_SHIELDS == 0 {
            // (a body only hurt by explosives takes nothing of a hit that detonates none)
            if res.flags & ONLY_HURT_BY_EXPLOSIVES != 0 && damage.flags & damage_flags::DETONATES_EXPLOSIVES == 0 {
                total = 0.0;
            }
            self.hit_body(res, &material, damage, friendly, total, &mut hurt);
        }
        Some(hurt)
    }

    /// `object_damage_shield`.
    fn hit_shield(
        &mut self,
        res: &Resistance,
        material: &DamageMaterial,
        damage: &Damage,
        friendly: bool,
        total_damage: &mut f32,
        hurt: &mut Hurt,
    ) {
        let mut total = *total_damage;
        let mut shield_damage = total;
        if self.shield > 0.0 {
            let maximum = res.maximum_shield_vitality;
            let inverse_maximum = if maximum > 0.0 { 1.0 / maximum } else { 0.0 };
            if !friendly || res.flags & ALWAYS_SHIELDS_FRIENDLY_DAMAGE == 0 {
                shield_damage = (1.0 - material.shield_leak_fraction) * total;
                if self.shield <= res.shield_failure_threshold && res.shield_failure_threshold > 0.0 {
                    // (linear: the only failure function the tags of the multiplayer player use)
                    let failure = (self.shield / res.shield_failure_threshold).clamp(0.0, 1.0);
                    shield_damage *= (1.0 - res.maximum_shield_failure) * failure + res.maximum_shield_failure;
                }
            }
            if self.flags & SHIELD_OVER_CHARGING != 0 {
                shield_damage = total;
                total = 0.0;
            } else {
                if shield_damage < 0.0 {
                    shield_damage = 0.0;
                }
                total -= shield_damage;
                let mut actual = material.shield_damage_multiplier * shield_damage;
                actual *= damage.material_modifiers
                    [(res.shield_material_type as usize).min(damage.material_modifiers.len() - 1)];
                let normalized = actual * inverse_maximum;
                if normalized > self.shield || damage.side_effect == SIDE_EFFECT_EMP {
                    let excess = actual - maximum * self.shield;
                    if excess > 0.0 {
                        total += excess;
                    }
                    self.shield = 0.0;
                    if self.flags & SHIELD_DEPLETED == 0 {
                        self.flags |= SHIELD_DEPLETED;
                        hurt.shield_depleted = true;
                    }
                } else {
                    self.shield -= normalized;
                }
            }
        } else {
            shield_damage = 0.0;
            self.shield = 0.0;
        }
        if shield_damage >= res.minimum_shield_stun_damage || self.shield == 0.0 {
            self.shield_stun_ticks = (res.shield_stun_time * TICKS_PER_SECOND as f32) as i16;
        }
        hurt.shield_damage = shield_damage;
        *total_damage = total;
    }

    /// `object_damage_body`.
    fn hit_body(
        &mut self,
        res: &Resistance,
        material: &DamageMaterial,
        damage: &Damage,
        friendly: bool,
        total_damage: f32,
        hurt: &mut Hurt,
    ) {
        let damage_amount = material.body_damage_multiplier * total_damage;
        let maximum = res.maximum_body_vitality;
        let inverse_maximum = if maximum > 0.0 { 1.0 / maximum } else { 0.0 };
        let mut actual = damage_amount;
        if friendly {
            actual = (1.0 - res.friendly_damage_resistance) * damage_amount;
        }
        actual *= inverse_maximum;
        actual *= damage.material_modifiers[(material.material_type as usize).min(damage.material_modifiers.len() - 1)];

        if damage_amount > 0.0 && material.flags & MATERIAL_HEAD != 0 {
            if damage.flags & damage_flags::CAN_CAUSE_HEADSHOTS != 0 {
                // (in multiplayer every player is hurt this way)
                self.body = 0.0;
                hurt.killed_instantly = true;
            } else if damage.flags & damage_flags::CAN_CAUSE_MULTIPLAYER_HEADSHOTS != 0 {
                actual *= 2.0;
            }
        }
        self.body -= actual;
        hurt.body_damage = damage_amount;

        let body = maximum * self.body;
        // (a body destroyed outright is dead too; the player dies once)
        let destroyed = res.body_destroyed_threshold < 0.0 && body < res.body_destroyed_threshold;
        if (destroyed || body < 0.0) && self.flags & DEAD == 0 {
            self.flags |= DEAD;
            hurt.killed = true;
        }
    }

    /// Overcharge the shield (the overshield powerup): `object_double_charge_shield`.
    /// `false` if the shield was beyond full already.
    pub fn overcharge(&mut self) -> bool {
        let charged = self.shield <= 1.0;
        if charged {
            self.flags |= SHIELD_OVER_CHARGING;
            if self.shield == 0.0 {
                self.shield = 0.01;
            }
            self.shield_stun_ticks = 0;
        }
        charged
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{combat_fixture, pistol_damage};

    fn body_hit() -> i16 {
        1
    }

    #[test]
    fn a_full_player_has_full_shields_and_health() {
        let c = combat_fixture();
        assert_eq!(Vitals::full(&c.resistance), Vitals { shield: 1.0, body: 1.0, shield_stun_ticks: 0, flags: 0 });
        let nothing = Resistance::default();
        assert_eq!(Vitals::full(&nothing), Vitals { shield: 0.0, body: 0.0, shield_stun_ticks: 0, flags: 0 });
    }

    #[test]
    fn a_hit_takes_the_shield_first_and_stuns_it() {
        let c = combat_fixture();
        let mut v = Vitals::full(&c.resistance);
        let hurt = v.hit(&c.resistance, &pistol_damage(), body_hit(), false, 25.0).unwrap();
        assert_eq!(v.body, 1.0, "the shield took it all");
        assert!((v.shield - (1.0 - 25.0 / 75.0)).abs() < 1e-6);
        assert_eq!(v.shield_stun_ticks, 180, "6 seconds of stun");
        assert!(!hurt.shield_depleted && !hurt.killed);
        assert_eq!(hurt.shield_damage, 25.0, "the shield took the whole hit");
    }

    #[test]
    fn what_a_depleted_shield_cannot_take_goes_to_the_body() {
        let c = combat_fixture();
        let mut v = Vitals::full(&c.resistance);
        for _ in 0..3 {
            v.hit(&c.resistance, &pistol_damage(), body_hit(), false, 25.0).unwrap();
        }
        assert_eq!(v.shield, 0.0);
        assert!(v.flags & SHIELD_DEPLETED != 0);
        // (the shield's float error leaves a sliver of the third hit for the body)
        assert!(v.body < 1.0 && v.body > 0.99, "{}", v.body);
        let hurt = v.hit(&c.resistance, &pistol_damage(), body_hit(), false, 25.0).unwrap();
        assert!(!hurt.shield_depleted);
        assert!((v.body - 0.5).abs() < 1e-5, "a body hit costs 25 * 1.5 of 75: {}", v.body);
    }

    #[test]
    fn a_player_whose_health_goes_below_nothing_is_dead_and_stays_so() {
        let c = combat_fixture();
        let mut v = Vitals::full(&c.resistance);
        let mut killed = false;
        for _ in 0..5 {
            killed |= v.hit(&c.resistance, &pistol_damage(), body_hit(), false, 25.0).unwrap().killed;
        }
        assert!(killed && v.is_dead());
        // (a corpse takes damage as the engine's does, and is not killed again)
        let hurt = v.hit(&c.resistance, &pistol_damage(), body_hit(), false, 25.0).unwrap();
        assert!(!hurt.killed && v.is_dead());
        assert!(v.hit(&c.resistance, &pistol_damage(), body_hit(), false, 0.0).is_none(), "no damage is no hit");
    }

    #[test]
    fn a_hit_to_the_head_that_reaches_the_body_kills_at_once() {
        let c = combat_fixture();
        let mut v = Vitals::full(&c.resistance);
        // with the shield up, a headshot only hurts the shield
        let hurt = v.hit(&c.resistance, &pistol_damage(), 0, false, 25.0).unwrap();
        assert!(!hurt.killed && v.body == 1.0);
        v.hit(&c.resistance, &pistol_damage(), 0, false, 25.0).unwrap();
        let hurt = v.hit(&c.resistance, &pistol_damage(), 0, false, 25.0).unwrap();
        assert!(
            hurt.killed && hurt.killed_instantly,
            "the third hit empties the shield and its remainder reaches the head"
        );
        assert!(v.is_dead());
    }

    #[test]
    fn a_hit_with_no_body_part_does_nothing_unless_the_body_has_an_indirect_part() {
        let mut c = combat_fixture();
        c.resistance.indirect_damage_material_index = -1;
        let mut v = Vitals::full(&c.resistance);
        v.hit(&c.resistance, &pistol_damage(), -1, false, 25.0).unwrap();
        assert_eq!(v.shield, 1.0);
        c.resistance.indirect_damage_material_index = 1;
        v.hit(&c.resistance, &pistol_damage(), -1, false, 25.0).unwrap();
        assert!(v.shield < 1.0);
    }

    #[test]
    fn a_shield_recharges_after_its_stun_and_not_before() {
        let c = combat_fixture();
        let r = &c.resistance;
        let mut v = Vitals::full(r);
        v.hit(r, &pistol_damage(), body_hit(), false, 25.0).unwrap();
        let hurt_shield = v.shield;
        // (180 ticks of stun, which the first tick after the hit begins to count)
        v.advance(r, 180);
        assert_eq!((v.shield, v.shield_stun_ticks), (hurt_shield, 0));
        v.advance(r, 1);
        assert!(v.shield > hurt_shield, "the recharge begins");
        v.advance(r, 1000);
        assert_eq!(v.shield, 1.0);
        assert!(v.is_settled());
    }

    #[test]
    fn a_dead_players_shield_does_not_recharge() {
        let c = combat_fixture();
        let r = &c.resistance;
        let mut v = Vitals::full(r);
        for _ in 0..5 {
            v.hit(r, &pistol_damage(), body_hit(), false, 25.0);
        }
        let before = v;
        v.advance(r, 600);
        assert_eq!(v.shield, before.shield);
    }

    #[test]
    fn an_overshield_wears_off_to_a_full_shield() {
        let c = combat_fixture();
        let r = &c.resistance;
        let mut v = Vitals::full(r);
        assert!(v.overcharge());
        v.advance(r, 60);
        assert!(v.shield > 2.9, "it comes on at a third of a shield in ten ticks: {}", v.shield);
        v.advance(r, 5000);
        assert_eq!(v.shield, 1.0);
    }

    #[test]
    fn a_friendly_hit_costs_the_body_less() {
        let mut c = combat_fixture();
        c.resistance.friendly_damage_resistance = 0.5;
        let r = &c.resistance;
        let mut v = Vitals { shield: 0.0, ..Vitals::full(r) };
        v.hit(r, &pistol_damage(), body_hit(), true, 25.0).unwrap();
        assert!((v.body - 0.75).abs() < 1e-5, "{}", v.body);
    }

    #[test]
    fn the_damage_of_a_hit_is_drawn_between_the_bounds() {
        let mut d = pistol_damage();
        d.lower = 10.0;
        d.upper = 20.0;
        let mut rng = Rng::seeded(7);
        for _ in 0..100 {
            let n = roll(&d, 1.0, 1.0, &mut rng);
            assert!((10.0..20.0).contains(&n), "{n}");
        }
        d.lower = 25.0;
        d.upper = 25.0;
        assert_eq!(roll(&d, 1.0, 1.0, &mut rng), 25.0, "nothing to choose between");
        assert_eq!(roll(&d, 0.5, 1.0, &mut rng), 0.5 * d.minimum + 12.5);
    }
}
