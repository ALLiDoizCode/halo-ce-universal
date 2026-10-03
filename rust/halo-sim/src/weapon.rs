//! A weapon in a player's hands: its ammunition, its heat, and when its
//! trigger can fire. A port of the engine's own (`source/items/weapons.c`:
//! `weapon_update`, `weapon_trigger_can_fire_again`, `weapon_trigger_fire`,
//! the magazine's reload and chamber), driven by the weapon's tag
//! ([`Weapon`]), so that a weapon is added to the game by its data. The
//! comparison harness holds it to the engine's own rate of fire.
//!
//! One tick is [`Hands::update`]: whether the trigger is held in, and whether
//! a shot was fired. The shot is the client's (it fires the weapon, and
//! decides what it hit); the server uses the model for the rate a weapon can
//! fire at ([`rate_of_fire`]) and tests and bots use it to fire at the real
//! rate.
//!
//! # What a trigger does
//!
//! A trigger fires on the tick it is held and can fire again: the ticks since
//! its last shot, counting that one, reach `30 / rate of fire`, where the rate
//! goes from the tag's initial rate to its final one as the trigger is held
//! (the cache's `rate_of_fire_acceleration` is the part of the way a tick
//! covers, and the deceleration the part it gives back). A latched trigger
//! (the pistol's is not) must be let go between shots. A trigger that has no
//! ammunition to fire locks until it is let go, and starts a reload.
//!
//! # Not here yet
//!
//! Charging, overloading and spewing triggers (the plasma pistol's), the
//! weapon's age, its recoil and ready animations, and the shotgun's
//! round-by-round reload are for the weapons' ticket. A reload takes the
//! frames of the weapon's first-person reload animation, as the engine times
//! it ([`Weapon::reload_frames`]), not the magazine's `reload_time`.

use halo_map::combat::{trigger_flags, Magazine, Trigger, Weapon};

use crate::TICKS_PER_SECOND;

/// The most a trigger counts idle ticks to (the engine's is a `char`).
const MAX_IDLE_TICKS: i16 = 127;

/// `weapon_magazine_definition.flags`: the magazine is chambered again after every shot.
const MAGAZINE_MUST_BE_CHAMBERED_EVERY_SHOT: u32 = 1 << 1;
/// `weapon_magazine_definition.flags`: a reload begins from nothing in the magazine.
const MAGAZINE_WASTES_ROUNDS_WHEN_RELOADED: u32 = 1 << 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MagazineState {
    Idle,
    /// Ticks to go.
    Reloading(i16),
    Unchambered,
    /// Ticks to go.
    Chambering(i16),
}

/// What [`Hands::update`] says happened this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Shot {
    /// The trigger fired.
    pub fired: bool,
    /// How many projectiles the shot made (the trigger's `projectiles_per_shot`).
    pub projectiles: u16,
    /// The weapon began to reload.
    pub reloading: bool,
}

/// A weapon, as someone holds it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hands {
    pub rounds_loaded: i16,
    pub rounds_total: i16,
    pub heat: f32,
    pub overheated: bool,
    magazine: MagazineState,
    /// Ticks since the (first) trigger last fired, up to 127.
    idle_ticks: i16,
    /// How far the trigger has come to its final rate of fire, 0 to 1.
    rate_of_fire: f32,
    /// The trigger has been let go since its last shot.
    released: bool,
    /// The trigger found nothing to fire and waits to be let go.
    locked: bool,
    /// Ticks left of the weapon's recoil animation after its last shot, which it is not idle for.
    recoil: i16,
}

/// The rate of fire a trigger has when it has come `fraction` of the way to its final rate: rounds a second.
pub fn rate_of_fire(trigger: &Trigger, fraction: f32) -> f32 {
    (trigger.final_rate_of_fire - trigger.initial_rate_of_fire) * fraction + trigger.initial_rate_of_fire
}

/// The fastest a weapon's trigger can fire, in shots a second (the greater of the rates; a trigger
/// of no rate of fire is read as one a second).
pub fn fastest_rate(weapon: &Weapon) -> f32 {
    let rate = weapon.triggers.first().map_or(0.0, |t| t.initial_rate_of_fire.max(t.final_rate_of_fire));
    if rate > 0.0001 {
        rate
    } else {
        1.0
    }
}

impl Hands {
    /// A new weapon, with the ammunition the tag starts it with (a full magazine).
    pub fn new(weapon: &Weapon) -> Hands {
        let (loaded, total) = match weapon.magazines.first() {
            Some(m) => (
                m.rounds_loaded_maximum.min(m.rounds_total_initial),
                (m.rounds_total_initial - m.rounds_loaded_maximum).max(0),
            ),
            None => (0, 0),
        };
        Hands {
            rounds_loaded: loaded,
            rounds_total: total,
            heat: 0.0,
            overheated: false,
            magazine: MagazineState::Idle,
            idle_ticks: MAX_IDLE_TICKS,
            rate_of_fire: 0.0,
            released: true,
            locked: false,
            recoil: 0,
        }
    }

    fn magazine_of<'a>(weapon: &'a Weapon, trigger: &Trigger) -> Option<&'a Magazine> {
        usize::try_from(trigger.magazine_index).ok().and_then(|i| weapon.magazines.get(i))
    }

    /// One tick (`weapon_update`): whether the trigger is held, and what the
    /// weapon did.
    pub fn update(&mut self, weapon: &Weapon, trigger_held: bool) -> Shot {
        let mut shot = Shot::default();
        let Some(trigger) = weapon.triggers.first() else { return shot };

        if self.recoil > 0 {
            self.recoil -= 1;
        }
        self.update_heat(weapon);

        // the magazine
        let magazine = Self::magazine_of(weapon, trigger);
        if let Some(m) = magazine {
            self.update_magazine(m);
        }

        // the trigger
        if !trigger_held {
            self.released = true;
        }
        if self.locked {
            // nothing to fire: it waits for the trigger to be let go
            if !trigger_held {
                self.locked = false;
            }
        } else {
            // an empty magazine reloads
            if let Some(m) = magazine {
                let empty = (self.rounds_loaded < trigger.rounds_per_shot
                    && trigger.flags & trigger_flags::CAN_FIRE_WITH_PARTIAL_AMMUNITION == 0)
                    || self.rounds_loaded < trigger.minimum_rounds_loaded_per_shot
                    || self.rounds_loaded == 0;
                if empty && self.start_reload(weapon, m) {
                    shot.reloading = true;
                }
            }
            if trigger_held && self.can_fire_again(trigger, trigger_held) {
                self.begin_firing(weapon, trigger, magazine, &mut shot);
            } else if self.idle_ticks < MAX_IDLE_TICKS {
                self.idle_ticks += 1;
            }
        }

        // the rate of fire comes up while the trigger is held, and goes down when it is let go
        if trigger_held {
            self.rate_of_fire = (self.rate_of_fire + trigger.rate_of_fire_acceleration).min(1.0);
        } else {
            self.rate_of_fire = (self.rate_of_fire - trigger.rate_of_fire_deceleration).max(0.0);
        }
        shot
    }

    fn update_heat(&mut self, weapon: &Weapon) {
        if self.heat > 0.0 {
            if self.heat >= weapon.heat_overheated_threshold && !self.overheated {
                self.overheated = true;
            }
            let loss = weapon.heat_loss_per_second * (1.0 / TICKS_PER_SECOND as f32);
            self.heat -= loss;
            if self.heat < 0.0 {
                self.heat = 0.0;
            }
            if self.overheated && self.heat < weapon.heat_recovery_threshold {
                self.overheated = false;
            }
        }
    }

    fn update_magazine(&mut self, m: &Magazine) {
        if m.rounds_recharged_per_second > 0 && self.rounds_loaded < m.rounds_loaded_maximum {
            // (the rounds a magazine that recharges itself gains, a tick's worth)
            let whole = m.rounds_recharged_per_second / TICKS_PER_SECOND as i16;
            self.rounds_loaded = (self.rounds_loaded + whole).min(m.rounds_loaded_maximum);
        }
        self.magazine = match self.magazine {
            MagazineState::Reloading(timer) => {
                let timer = if timer > 0 { timer - 1 } else { timer };
                if timer - 1 <= 0 {
                    self.finish_reload(m);
                    MagazineState::Unchambered
                } else {
                    MagazineState::Reloading(timer)
                }
            }
            MagazineState::Unchambered if self.recoil == 0 => {
                MagazineState::Chambering((m.chamber_time * TICKS_PER_SECOND as f32) as i16)
            }
            MagazineState::Unchambered => MagazineState::Unchambered,
            MagazineState::Chambering(timer) => {
                let timer = if timer > 0 { timer - 1 } else { timer };
                if timer == 0 {
                    MagazineState::Idle
                } else {
                    MagazineState::Chambering(timer)
                }
            }
            MagazineState::Idle => MagazineState::Idle,
        };
    }

    fn start_reload(&mut self, weapon: &Weapon, m: &Magazine) -> bool {
        if self.recoil > 0 || !matches!(self.magazine, MagazineState::Idle | MagazineState::Unchambered) {
            return false;
        }
        if self.rounds_total > 0 && self.rounds_loaded < m.rounds_loaded_maximum {
            self.magazine = MagazineState::Reloading(weapon.reload_frames);
            return true;
        }
        false
    }

    fn finish_reload(&mut self, m: &Magazine) {
        if m.flags & MAGAZINE_WASTES_ROUNDS_WHEN_RELOADED != 0 {
            self.rounds_loaded = 0;
        }
        let to_load = m.rounds_reloaded.min(self.rounds_total);
        let loaded = (self.rounds_loaded + to_load).min(m.rounds_loaded_maximum);
        self.rounds_total = self.rounds_total - loaded + self.rounds_loaded;
        self.rounds_loaded = loaded;
    }

    /// `weapon_trigger_can_fire_again`.
    fn can_fire_again(&self, trigger: &Trigger, trigger_held: bool) -> bool {
        // (an analog trigger's squeeze is all of it, held)
        let fraction = if trigger.flags & trigger_flags::ANALOG_RATE_OF_FIRE != 0 {
            if trigger_held {
                1.0
            } else {
                0.0
            }
        } else {
            self.rate_of_fire
        };
        let rate = rate_of_fire(trigger, fraction);
        let required_ticks = if rate > 0.0001 { TICKS_PER_SECOND as f32 / rate } else { 0.0 };
        if trigger.flags & trigger_flags::LATCHED != 0 && !self.released {
            return false;
        }
        self.idle_ticks as f32 + 1.0 >= required_ticks
    }

    /// `weapon_trigger_begin_firing` and `weapon_trigger_fire`.
    fn begin_firing(&mut self, weapon: &Weapon, trigger: &Trigger, magazine: Option<&Magazine>, shot: &mut Shot) {
        if self.magazine != MagazineState::Idle && magazine.is_some() {
            return;
        }
        if self.overheated {
            return;
        }
        let mut fired = false;
        if let Some(m) = magazine {
            let enough = self.rounds_loaded >= trigger.rounds_per_shot
                || trigger.flags & trigger_flags::CAN_FIRE_WITH_PARTIAL_AMMUNITION != 0;
            if enough && (self.rounds_loaded >= trigger.minimum_rounds_loaded_per_shot || !self.released) {
                self.rounds_loaded -= trigger.rounds_per_shot;
                if self.rounds_loaded <= 0 {
                    self.rounds_loaded = 0;
                } else if m.flags & MAGAZINE_MUST_BE_CHAMBERED_EVERY_SHOT != 0 {
                    self.magazine = MagazineState::Unchambered;
                }
                fired = true;
            }
        } else {
            fired = true;
        }
        if fired {
            self.heat += trigger.heat_generated_per_round;
            if self.heat > 1.0 {
                self.heat = 1.0;
            }
            shot.fired = true;
            shot.projectiles = trigger.projectiles_per_shot.max(0) as u16;
            // recovering: the trigger counts its idle ticks again
            self.idle_ticks = 0;
            self.recoil = (weapon.recoil_frames - 1).max(0);
        } else {
            self.locked = true;
        }
        self.released = false;
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;
    use crate::fixtures::pistol;

    /// The ticks the trigger fires on, holding it from tick `from` for `ticks` ticks.
    fn firing_ticks(weapon: &Weapon, from: u32, until: u32) -> Vec<u32> {
        let mut hands = Hands::new(weapon);
        let mut fired = Vec::new();
        for tick in 0..until {
            if hands.update(weapon, tick >= from).fired {
                fired.push(tick);
            }
        }
        fired
    }

    #[test]
    fn a_weapon_starts_with_a_full_magazine_and_the_rest_in_reserve() {
        let hands = Hands::new(&pistol());
        assert_eq!((hands.rounds_loaded, hands.rounds_total), (12, 48));
    }

    #[test]
    fn a_held_trigger_fires_at_the_weapons_rate() {
        // 3.5 shots a second is a shot every 8.57 ticks: the trigger fires when 9 ticks have gone since the last
        let fired = firing_ticks(&pistol(), 60, 140);
        assert_eq!(fired, [60, 69, 78, 87, 96, 105, 114, 123, 132]);
    }

    #[test]
    fn each_shot_uses_a_round_and_an_empty_magazine_reloads() {
        let weapon = pistol();
        let mut hands = Hands::new(&weapon);
        let mut shots = 0;
        let mut reloading_at = None;
        for tick in 0..600 {
            let s = hands.update(&weapon, true);
            shots += s.fired as u32;
            if s.reloading && reloading_at.is_none() {
                reloading_at = Some(tick);
            }
        }
        assert!(shots > 12, "the weapon fires again once it has reloaded: {shots}");
        assert!(reloading_at.is_some());
        assert_eq!(hands.rounds_loaded + hands.rounds_total + shots as i16, 60, "no round is lost: {hands:?}");
    }

    #[test]
    fn a_weapon_with_no_ammunition_left_does_not_fire() {
        let weapon = pistol();
        let mut hands = Hands::new(&weapon);
        hands.rounds_loaded = 0;
        hands.rounds_total = 0;
        for _ in 0..200 {
            assert!(!hands.update(&weapon, true).fired);
        }
    }

    #[test]
    fn a_latched_trigger_fires_once_for_each_press() {
        let mut weapon = pistol();
        weapon.triggers[0].flags |= trigger_flags::LATCHED;
        let mut hands = Hands::new(&weapon);
        let held: Vec<bool> = (0..60).map(|_| hands.update(&weapon, true).fired).collect();
        assert_eq!(held.iter().filter(|f| **f).count(), 1);
        hands.update(&weapon, false);
        assert!(hands.update(&weapon, true).fired, "pressed again");
    }

    #[test]
    fn a_trigger_comes_up_to_its_final_rate_as_it_is_held() {
        let mut weapon = pistol();
        weapon.triggers[0].initial_rate_of_fire = 3.0;
        weapon.triggers[0].final_rate_of_fire = 10.0;
        weapon.triggers[0].rate_of_fire_acceleration = 0.05;
        let fired = firing_ticks(&weapon, 0, 120);
        let gaps: Vec<u32> = fired.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(gaps.first() > gaps.last(), "the shots come faster: {gaps:?}");
    }

    #[test]
    fn a_weapon_that_heats_up_stops_until_it_has_cooled() {
        let mut weapon = pistol();
        weapon.triggers[0].heat_generated_per_round = 0.3;
        weapon.heat_overheated_threshold = 1.0;
        weapon.heat_recovery_threshold = 0.25;
        weapon.heat_loss_per_second = 0.5;
        weapon.magazines.clear();
        weapon.triggers[0].magazine_index = -1;
        let mut hands = Hands::new(&weapon);
        let mut shots = 0;
        let mut overheated = false;
        for _ in 0..90 {
            shots += hands.update(&weapon, true).fired as u32;
            overheated |= hands.overheated;
        }
        assert!(overheated, "{hands:?}");
        assert!(shots > 3 && shots < 90 / 9 + 3, "{shots}");
    }

    #[test]
    fn the_fastest_rate_is_the_greater_of_a_triggers_two() {
        let mut weapon = pistol();
        assert_eq!(fastest_rate(&weapon), 3.5);
        weapon.triggers[0].final_rate_of_fire = 7.0;
        assert_eq!(fastest_rate(&weapon), 7.0);
        weapon.triggers.clear();
        assert_eq!(fastest_rate(&weapon), 1.0);
    }
}
