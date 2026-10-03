//! A weapon in a player's hands: its ammunition, its heat, its triggers and
//! when they fire. A port of the engine's own (`source/items/weapons.c`:
//! `weapon_update`, `weapon_trigger_can_fire_again`, `weapon_trigger_begin_firing`,
//! `weapon_trigger_fire`, the charge of a trigger and its release, the
//! magazine's reload and chamber), driven by the weapon's tag ([`Weapon`]), so
//! that a weapon is added to the game by its data. The comparison harness
//! holds it to the engine's own, weapon by weapon.
//!
//! One tick is [`Hands::update`]: whether the trigger is held in, and whether
//! a shot was fired. The shot is the client's (it fires the weapon, and
//! decides what it hit); the server uses the model for the rate a weapon can
//! fire at ([`trigger_rate`]) and tests and bots use it to fire at the real
//! rate.
//!
//! # What a trigger does
//!
//! A trigger fires on the tick it is held and can fire again: the ticks since
//! its last shot, counting that one, reach `30 / rate of fire`, where the rate
//! goes from the tag's initial rate to its final one as the trigger is held
//! (the cache's `rate_of_fire_acceleration` is the part of the way a tick
//! covers, and the deceleration the part it gives back). A latched trigger
//! (the sniper rifle's) must be let go between shots. A trigger that has no
//! ammunition to fire locks until it is let go, and starts a reload.
//!
//! # Charging
//!
//! A trigger that has a `charging_time`, on a weapon with a second trigger (the
//! plasma pistol), does not fire when it is pressed: it charges for that long. If
//! the button is let go before the charge is full it fires the first trigger's
//! shot; if the charge is full it waits (the charge is held for `charged_time`),
//! and when the button is let go fires the *second* trigger's shot (the
//! overcharged shot) in its place. Heat that the shots make is not lost while a
//! charge is held.
//!
//! # Heat
//!
//! Each shot makes the trigger's `heat_generated_per_round` of heat (up to 1), a
//! weapon loses `heat_loss_per_second` of it, and a weapon whose heat reaches
//! `heat_overheated_threshold` does not fire until it has cooled below
//! `heat_recovery_threshold`.
//!
//! # Reloading
//!
//! A reload takes the frames of the weapon's first-person reload animation, as
//! the engine times it ([`Weapon::reload_frames`]), not the magazine's
//! `reload_time`. A shotgun loads a round at a time: its first takes the
//! animation of going in ([`Weapon::shotgun_enter_frames`]), and each of the
//! others the reload animation, one after the other until the magazine is
//! full, the weapon is out of rounds, or the player presses the trigger.
//!
//! # Melee
//!
//! [`Melee`] is the player's blow: the first-person melee animation (less a
//! quarter) is its length, and it lands a fixed number of ticks into it.
//!
//! # Age
//!
//! A weapon grows older with every shot (`age_generated_per_round`, up to 1): a
//! plasma weapon's battery. An older weapon fires more slowly
//! (`age_rate_of_fire_penalty`) and cools more slowly (`age_heat_recovery_penalty`);
//! one that cannot fire at its maximum age (`WEAPON_CANNOT_FIRE_AT_MAXIMUM_AGE`) does
//! not at 1; and past `age_misfire_start` a shot may misfire (nothing comes out of the weapon
//! but the shot is spent), with a chance that rises with age: the engine's random numbers
//! decide, and the caller of [`Hands::update_with`] provides them.
//!
//! # Not here
//!
//! The second trigger held as a button of its own (no weapon of the maps has it
//! so), trigger states that spew or overload (none has the times), and a weapon that
//! blows up from the heat (none of the maps' does).

use halo_map::combat::{trigger_flags, Magazine, Trigger, Weapon};

use crate::TICKS_PER_SECOND;

/// The most a trigger counts idle ticks to (the engine's is a `char`).
const MAX_IDLE_TICKS: i16 = 127;

/// `weapon_magazine_definition.flags`: the magazine is chambered again after every shot.
const MAGAZINE_MUST_BE_CHAMBERED_EVERY_SHOT: u32 = 1 << 1;
/// `weapon_magazine_definition.flags`: a reload begins from nothing in the magazine.
const MAGAZINE_WASTES_ROUNDS_WHEN_RELOADED: u32 = 1 << 0;
/// `weapon_definition.weapon_type`: a shotgun, which reloads a round at a time.
const WEAPON_TYPE_SHOTGUN: i16 = 1;
/// `weapon_definition.flags`: the weapon cannot be used to strike a blow.
const WEAPON_PREVENTS_MELEE_ATTACK: u32 = 1 << 9;
/// `weapon_definition.flags`: the weapon does not fire once it is as old as it gets.
const WEAPON_CANNOT_FIRE_AT_MAXIMUM_AGE: u32 = 1 << 11;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MagazineState {
    Idle,
    /// Ticks to go.
    Reloading(i16),
    Unchambered,
    /// Ticks to go.
    Chambering(i16),
}

/// What a trigger is doing (`weapon_trigger_state`; a trigger that spews, overloads or
/// tracks is not here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TriggerMode {
    Idle,
    /// A charge to come, which takes the timer's ticks.
    Charging,
    /// A full charge, held for the timer's ticks more.
    Charged,
    /// Nothing to fire: the trigger waits to be let go.
    Locked,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct TriggerState {
    mode: TriggerMode,
    timer: i16,
    /// Ticks since the trigger last fired, up to 127.
    idle_ticks: i16,
    /// How far the trigger has come to its final rate of fire, 0 to 1.
    rate_of_fire: f32,
    /// The trigger has been let go since its last shot.
    released: bool,
    /// A charging trigger fired when it was pressed (a weapon of one trigger).
    fired_before_charging: bool,
}

impl TriggerState {
    const NEW: TriggerState = TriggerState {
        mode: TriggerMode::Idle,
        timer: 0,
        idle_ticks: MAX_IDLE_TICKS,
        rate_of_fire: 0.0,
        released: true,
        fired_before_charging: false,
    };
}

/// What [`Hands::update`] says happened this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Shot {
    /// A trigger fired.
    pub fired: bool,
    /// Which of the weapon's triggers fired (the overcharged shot is the second's).
    pub trigger: u8,
    /// How many projectiles the shot made (the trigger's `projectiles_per_shot`).
    pub projectiles: u16,
    /// The weapon began to reload.
    pub reloading: bool,
    /// The shot misfired (an old battery): it was spent and made no projectile (`projectiles` is 0).
    pub misfired: bool,
}

/// A weapon, as someone holds it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hands {
    pub rounds_loaded: i16,
    pub rounds_total: i16,
    pub heat: f32,
    pub overheated: bool,
    /// How old the weapon is, 0 to 1.
    pub age: f32,
    magazine: MagazineState,
    /// A fraction of a round a magazine that recharges itself has gained.
    rounds_fractional: i16,
    triggers: [TriggerState; 2],
    /// How far a charge held past full has come (0 when none is held): a weapon does not cool then.
    overcharged: f32,
    /// Ticks left of the weapon's recoil animation after its last shot, which it is not idle for.
    recoil: i16,
}

/// The rate of fire a trigger has when it has come `fraction` of the way to its final rate: rounds a second.
pub fn rate_of_fire(trigger: &Trigger, fraction: f32) -> f32 {
    (trigger.final_rate_of_fire - trigger.initial_rate_of_fire) * fraction + trigger.initial_rate_of_fire
}

/// The seconds of fire a shooter's bucket of hits holds (see [`crate::combat`]),
/// which is also the window the heat a weapon can take is counted over.
pub const BURST_SECONDS: f32 = 3.0;

/// The engine takes this fraction of a first-person melee animation off a
/// player's blow (`biped_update`: the frames shifted right by this).
pub const MELEE_SPEEDUP_SHIFT: i16 = 2;

/// The most shots a second a weapon's trigger can fire, as a player who
/// plays it for all it is worth would:
///
/// - a trigger with a rate of fire fires at the greater of its two rates (a
///   trigger of no rate is read as one a second);
/// - a trigger that charges, on a weapon with a second trigger, fires on the
///   release of the button, so a player can tap it every other tick; the
///   second trigger is the charged shot, fired on the release of a full charge;
/// - a weapon that makes heat is held to what it can take in
///   [`BURST_SECONDS`]: a full heat gauge's worth and what it loses meanwhile,
///   at the heat each shot makes.
pub fn trigger_rate(weapon: &Weapon, index: usize) -> f32 {
    let Some(trigger) = weapon.triggers.get(index) else { return 1.0 };
    let ticks = TICKS_PER_SECOND as f32;
    let charged_by = index.checked_sub(1).and_then(|i| weapon.triggers.get(i)).filter(|t| t.charging_time > 0.0);
    let mut rate = trigger.initial_rate_of_fire.max(trigger.final_rate_of_fire);
    if rate <= 0.0001 {
        rate = 1.0;
    }
    if trigger.charging_time > 0.0 && weapon.triggers.len() > 1 {
        rate = ticks / 2.0;
    } else if let Some(charging) = charged_by {
        // (a tick to press, a tick to let go, and the charge between)
        rate = ticks / ((charging.charging_time * ticks) as i32 as f32 + 1.0);
    } else if trigger.charging_time > 0.0 {
        rate = rate.min(ticks / ((trigger.charging_time * ticks) as i32 as f32 + 1.0));
    }
    if trigger.heat_generated_per_round > 0.0 {
        let shots = (1.0 + weapon.heat_loss_per_second * BURST_SECONDS) / trigger.heat_generated_per_round;
        rate = rate.min(shots / BURST_SECONDS);
    }
    rate
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
            age: 0.0,
            magazine: MagazineState::Idle,
            rounds_fractional: 0,
            triggers: [TriggerState::NEW; 2],
            overcharged: 0.0,
            recoil: 0,
        }
    }

    fn magazine_of<'a>(weapon: &'a Weapon, trigger: &Trigger) -> Option<&'a Magazine> {
        usize::try_from(trigger.magazine_index).ok().and_then(|i| weapon.magazines.get(i))
    }

    /// Whether the weapon is at rest: no trigger charging or locked, and no animation of the
    /// weapon's playing (`weapon_magazine_state_change_ok`), which a magazine needs to start
    /// to reload or to chamber a round.
    fn at_rest(&self) -> bool {
        self.triggers.iter().all(|t| t.mode == TriggerMode::Idle) && self.recoil == 0
    }

    /// The weapon's first trigger is charging or charged (`weapon_overcharged`): a player cannot strike a blow.
    pub fn is_charging(&self) -> bool {
        matches!(self.triggers[0].mode, TriggerMode::Charging | TriggerMode::Charged)
    }

    /// The weapon is reloading: what the others are shown of a player's weapon
    /// ([`crate::FLAG_RELOADING`]).
    pub fn reloading(&self) -> bool {
        matches!(self.magazine, MagazineState::Reloading(_))
    }

    /// The player asked for a reload (the engine's reload control, `weapon_update`'s
    /// `_weapon_needs_to_reload_bit`): it begins if the weapon can, as an empty
    /// magazine's does. Returns whether it began.
    pub fn request_reload(&mut self, weapon: &Weapon) -> bool {
        let Some(trigger) = weapon.triggers.first() else { return false };
        let Some(m) = Self::magazine_of(weapon, trigger) else { return false };
        self.start_reload(weapon, m, true)
    }

    /// One tick (`weapon_update`): whether the trigger is held, and what the
    /// weapon did. A weapon that has aged past `age_misfire_start` here never misfires:
    /// see [`Hands::update_with`].
    pub fn update(&mut self, weapon: &Weapon, trigger_held: bool) -> Shot {
        self.update_with(weapon, trigger_held, &mut || 1.0)
    }

    /// ... with `random` giving the engine's `real_random()`, a number from 0 to 1, each time the
    /// weapon would ask for one (a shot of a weapon past its misfire age asks: a number under the
    /// chance of a misfire is one).
    pub fn update_with(&mut self, weapon: &Weapon, trigger_held: bool, random: &mut dyn FnMut() -> f32) -> Shot {
        let mut shot = Shot::default();
        if weapon.triggers.is_empty() {
            return shot;
        }

        if self.recoil > 0 {
            self.recoil -= 1;
        }
        self.update_heat(weapon);
        self.overcharged = 0.0;

        if let Some(m) = weapon.triggers.first().and_then(|t| Self::magazine_of(weapon, t)) {
            self.update_magazine(weapon, m, trigger_held);
        }

        for index in 0..weapon.triggers.len().min(2) {
            self.update_trigger(weapon, index, index == 0 && trigger_held, random, &mut shot);
        }
        shot
    }

    /// `weapon_stop_reload` (`weapon_reset`): the triggers start over, and a reload more than half
    /// done is finished, one less is given up. The player's melee blow does this to the weapon.
    pub fn reset(&mut self, weapon: &Weapon) {
        for trigger in &mut self.triggers {
            // (the engine's triggers are uninitialised for a tick, and then idle)
            trigger.mode = TriggerMode::Idle;
            trigger.timer = 0;
        }
        if let Some(m) = weapon.triggers.first().and_then(|t| Self::magazine_of(weapon, t)) {
            if let MagazineState::Reloading(timer) = self.magazine {
                if 2 * i32::from(timer) < i32::from(weapon.reload_frames) {
                    self.finish_reload(m);
                }
            }
            self.magazine = MagazineState::Idle;
        }
    }

    fn update_heat(&mut self, weapon: &Weapon) {
        if self.heat > 0.0 {
            if self.heat >= weapon.heat_overheated_threshold && !self.overheated {
                self.overheated = true;
            }
            if self.overcharged == 0.0 {
                let mut loss = weapon.heat_loss_per_second * (1.0 / TICKS_PER_SECOND as f32);
                if weapon.age_heat_recovery_penalty > 0.0 {
                    loss *= 1.0 - self.age * weapon.age_heat_recovery_penalty;
                }
                self.heat -= loss;
                if self.heat < 0.0 {
                    self.heat = 0.0;
                }
            }
            if self.overheated && self.heat < weapon.heat_recovery_threshold {
                self.overheated = false;
            }
        }
    }

    fn update_magazine(&mut self, weapon: &Weapon, m: &Magazine, trigger_held: bool) {
        if m.rounds_recharged_per_second > 0 && self.rounds_loaded < m.rounds_loaded_maximum {
            // (a magazine that recharges itself gains its whole rounds' worth a tick, and the rest as they add up)
            let ticks = TICKS_PER_SECOND as i16;
            self.rounds_loaded += m.rounds_recharged_per_second / ticks;
            self.rounds_fractional += m.rounds_recharged_per_second % ticks;
            if self.rounds_fractional >= ticks {
                self.rounds_loaded += 1;
                self.rounds_fractional -= ticks;
            }
            self.rounds_loaded = self.rounds_loaded.min(m.rounds_loaded_maximum);
        }
        match self.magazine {
            MagazineState::Reloading(timer) => {
                let timer = if timer > 0 { timer - 1 } else { timer };
                if timer - 1 <= 0 {
                    self.finish_reload(m);
                    self.magazine = MagazineState::Unchambered;
                    // a shotgun goes on to the next round, unless the player has the trigger in
                    if self.rounds_total > 0
                        && self.rounds_loaded < m.rounds_loaded_maximum
                        && m.flags & MAGAZINE_WASTES_ROUNDS_WHEN_RELOADED == 0
                        && !trigger_held
                    {
                        self.start_reload(weapon, m, false);
                    }
                } else {
                    self.magazine = MagazineState::Reloading(timer);
                }
            }
            MagazineState::Unchambered => {
                if self.at_rest() {
                    self.magazine = MagazineState::Chambering((m.chamber_time * TICKS_PER_SECOND as f32) as i16);
                }
            }
            MagazineState::Chambering(timer) => {
                let timer = if timer > 0 { timer - 1 } else { timer };
                self.magazine = if timer == 0 { MagazineState::Idle } else { MagazineState::Chambering(timer) };
            }
            MagazineState::Idle => {}
        }
    }

    /// `weapon_magazine_start_reload`: `first` is a reload the weapon starts for itself, not one that follows a round of a shotgun's.
    fn start_reload(&mut self, weapon: &Weapon, m: &Magazine, first: bool) -> bool {
        if !matches!(self.magazine, MagazineState::Idle | MagazineState::Unchambered) || !self.at_rest() {
            return false;
        }
        if self.rounds_total > 0 && self.rounds_loaded < m.rounds_loaded_maximum {
            let frames = if weapon.weapon_type == WEAPON_TYPE_SHOTGUN && first {
                weapon.shotgun_enter_frames
            } else {
                weapon.reload_frames
            };
            self.magazine = MagazineState::Reloading(frames);
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

    /// One trigger's part of a tick (the loop over the triggers of `weapon_update`).
    fn update_trigger(
        &mut self,
        weapon: &Weapon,
        index: usize,
        down: bool,
        random: &mut dyn FnMut() -> f32,
        shot: &mut Shot,
    ) {
        let trigger = &weapon.triggers[index];
        let magazine = Self::magazine_of(weapon, trigger);
        if self.triggers[index].timer > 0 {
            self.triggers[index].timer -= 1;
        }
        if !down {
            self.triggers[index].released = true;
        }
        match self.triggers[index].mode {
            TriggerMode::Idle => {
                // an empty magazine reloads
                if let Some(m) = magazine {
                    let empty = (self.rounds_loaded < trigger.rounds_per_shot
                        && trigger.flags & trigger_flags::CAN_FIRE_WITH_PARTIAL_AMMUNITION == 0)
                        || self.rounds_loaded < trigger.minimum_rounds_loaded_per_shot
                        || self.rounds_loaded == 0;
                    if empty && self.start_reload(weapon, m, true) {
                        shot.reloading = true;
                    }
                }
                if down && self.can_fire_again(weapon, trigger, index, down) {
                    self.begin_firing(weapon, index, false, random, shot);
                } else if self.triggers[index].idle_ticks < MAX_IDLE_TICKS {
                    self.triggers[index].idle_ticks += 1;
                }
            }
            TriggerMode::Charging => {
                if self.triggers[index].timer != 0 {
                    if !down {
                        if index == 0 && weapon.triggers.len() > 1 && !self.triggers[index].fired_before_charging {
                            self.begin_firing(weapon, index, true, random, shot);
                        } else {
                            self.triggers[index].mode = TriggerMode::Idle;
                        }
                    }
                } else {
                    // fully charged
                    self.triggers[index].mode = TriggerMode::Charged;
                    self.triggers[index].timer = (trigger.charged_time * TICKS_PER_SECOND as f32) as i16;
                }
            }
            TriggerMode::Charged => {
                if down {
                    if trigger.charged_time > 0.0 {
                        let left = f32::from(self.triggers[index].timer) * (1.0 / TICKS_PER_SECOND as f32);
                        self.overcharged = 1.0 - left / trigger.charged_time;
                    }
                    if self.triggers[index].timer != 0
                        && magazine.is_some()
                        && self.rounds_loaded < trigger.rounds_per_shot
                        && trigger.flags & trigger_flags::CAN_FIRE_WITH_PARTIAL_AMMUNITION == 0
                    {
                        self.release_charge(weapon, index, random, shot);
                    }
                    // (a charge held to its end does nothing more here: no weapon of the maps overcharges
                    // into an explosion)
                } else {
                    self.release_charge(weapon, index, random, shot);
                }
            }
            TriggerMode::Locked => {
                if !down {
                    self.triggers[index].mode = TriggerMode::Idle;
                }
            }
        }

        // the rate of fire comes up while the trigger is held, and goes down when it is let go
        let state = &mut self.triggers[index];
        if down {
            state.rate_of_fire = (state.rate_of_fire + trigger.rate_of_fire_acceleration).min(1.0);
        } else {
            state.rate_of_fire = (state.rate_of_fire - trigger.rate_of_fire_deceleration).max(0.0);
        }
    }

    /// `weapon_trigger_can_fire_again`.
    fn can_fire_again(&self, weapon: &Weapon, trigger: &Trigger, index: usize, trigger_held: bool) -> bool {
        // (an analog trigger's squeeze is all of it, held)
        let fraction = if trigger.flags & trigger_flags::ANALOG_RATE_OF_FIRE != 0 {
            if trigger_held {
                1.0
            } else {
                0.0
            }
        } else {
            self.triggers[index].rate_of_fire
        };
        let rate = rate_of_fire(trigger, fraction);
        let mut required_ticks = if rate > 0.0001 { TICKS_PER_SECOND as f32 / rate } else { 0.0 };
        if weapon.age_rate_of_fire_penalty > 0.0 {
            required_ticks *= self.age * weapon.age_rate_of_fire_penalty + 1.0;
        }
        if trigger.flags & trigger_flags::LATCHED != 0 && !self.triggers[index].released {
            return false;
        }
        self.triggers[index].idle_ticks as f32 + 1.0 >= required_ticks
    }

    /// `weapon_trigger_begin_firing`; `force` fires a trigger that would charge.
    fn begin_firing(
        &mut self,
        weapon: &Weapon,
        index: usize,
        force: bool,
        random: &mut dyn FnMut() -> f32,
        shot: &mut Shot,
    ) {
        let trigger = &weapon.triggers[index];
        let magazine = Self::magazine_of(weapon, trigger);
        if self.magazine != MagazineState::Idle && magazine.is_some() {
            return;
        }
        if self.overheated {
            return;
        }
        if !force && trigger.charging_time > 0.0 {
            if weapon.flags & WEAPON_CANNOT_FIRE_AT_MAXIMUM_AGE != 0 && self.age >= 1.0 {
                self.fire(weapon, index, random, shot);
                return;
            }
            if weapon.triggers.len() > 1 {
                // (a weapon with a second trigger charges that second trigger's shot, and fires this one on a release)
            } else if self.triggers[index].rate_of_fire > 0.0 {
                self.triggers[index].fired_before_charging = true;
                self.fire(weapon, index, random, shot);
            } else {
                self.triggers[index].fired_before_charging = false;
            }
            self.triggers[index].mode = TriggerMode::Charging;
            self.triggers[index].timer = (trigger.charging_time * TICKS_PER_SECOND as f32) as i16;
        } else {
            self.fire(weapon, index, random, shot);
        }
    }

    /// A charge is let go (`weapon_trigger_release_charge`): the second trigger's shot, on a weapon that has one.
    fn release_charge(&mut self, weapon: &Weapon, index: usize, random: &mut dyn FnMut() -> f32, shot: &mut Shot) {
        if weapon.triggers.len() > 1 {
            self.fire(weapon, 1, random, shot);
        }
        self.recover(index);
        self.triggers[index].rate_of_fire = 0.0;
    }

    /// `weapon_trigger_recover`: the trigger counts its idle ticks again.
    fn recover(&mut self, index: usize) {
        self.triggers[index].idle_ticks = 0;
        self.triggers[index].mode = TriggerMode::Idle;
    }

    /// `weapon_trigger_fire`.
    fn fire(&mut self, weapon: &Weapon, index: usize, random: &mut dyn FnMut() -> f32, shot: &mut Shot) {
        let trigger = &weapon.triggers[index];
        let magazine = Self::magazine_of(weapon, trigger);
        let mut fired = false;
        let worn_out = weapon.flags & WEAPON_CANNOT_FIRE_AT_MAXIMUM_AGE != 0 && self.age >= 1.0;
        if let Some(m) = magazine {
            let enough = self.rounds_loaded >= trigger.rounds_per_shot
                || trigger.flags & trigger_flags::CAN_FIRE_WITH_PARTIAL_AMMUNITION != 0;
            if enough
                && !worn_out
                && (self.rounds_loaded >= trigger.minimum_rounds_loaded_per_shot || !self.triggers[index].released)
            {
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
        // an old weapon may misfire
        let mut misfired = false;
        if weapon.age_misfire_start > 0.0 && weapon.age_misfire_start < 1.0 && self.age > weapon.age_misfire_start {
            let chance =
                ((self.age - weapon.age_misfire_start) * weapon.age_misfire_chance) / (1.0 - weapon.age_misfire_start);
            misfired = random() < chance;
        }
        if fired {
            self.heat += trigger.heat_generated_per_round;
            if self.heat > 1.0 {
                self.heat = 1.0;
            }
            self.age += trigger.age_generated_per_round;
            if self.age > 1.0 {
                self.age = 1.0;
            }
            shot.fired = true;
            shot.misfired = misfired;
            shot.trigger = index as u8;
            shot.projectiles = if misfired { 0 } else { trigger.projectiles_per_shot.max(0) as u16 };
            self.recoil = (weapon.recoil_frames - 1).max(0);
        }
        if !fired {
            self.triggers[index].mode = TriggerMode::Locked;
        } else {
            self.recover(index);
        }
        self.triggers[index].released = false;
    }
}

/// A player's blow in melee: `player_melee_ticks` and `player_melee_attack_tick` of the engine's
/// biped (`biped_update`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Melee {
    ticks: i16,
    attack_tick: i16,
}

impl Melee {
    /// One tick with the melee button `pressed` or not, holding `hands` (which a blow starts
    /// over): whether the blow lands this tick. A weapon that prevents it, and a charge, make
    /// pressing the button do nothing.
    pub fn update(&mut self, weapon: &Weapon, hands: &mut Hands, pressed: bool) -> bool {
        if self.ticks == 0 {
            if pressed && weapon.flags & WEAPON_PREVENTS_MELEE_ATTACK == 0 && !hands.is_charging() {
                hands.reset(weapon);
                self.ticks = weapon.melee_frames;
                self.attack_tick = self.ticks - weapon.melee_key_frame;
                let speedup = self.ticks >> MELEE_SPEEDUP_SHIFT;
                self.ticks -= speedup;
                self.attack_tick -= speedup;
            }
            false
        } else {
            let lands = self.ticks == self.attack_tick;
            self.ticks -= 1;
            lands
        }
    }

    /// A blow is on its way.
    pub fn is_striking(&self) -> bool {
        self.ticks > 0
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
    fn a_reload_asked_for_with_rounds_to_spare_runs_the_animations_frames_and_shows_as_reloading() {
        let weapon = pistol();
        let mut hands = Hands::new(&weapon);
        hands.update(&weapon, true);
        for _ in 0..30 {
            hands.update(&weapon, false);
        }
        assert!(!hands.reloading());
        assert!(hands.request_reload(&weapon), "a magazine that is not full reloads when asked");
        assert!(hands.reloading());
        let mut ticks = 0;
        while hands.reloading() {
            hands.update(&weapon, false);
            ticks += 1;
            assert!(ticks < 300);
        }
        assert_eq!((hands.rounds_loaded, hands.rounds_total), (12, 47), "{hands:?}");
        assert!(!hands.request_reload(&weapon), "a full magazine does not");
    }

    /// The shots a plasma pistol makes (the tick, which trigger) when the button is held on the ticks `held` says.
    fn plasma_shots(held: impl Fn(u32) -> bool, ticks: u32) -> (Vec<(u32, u8)>, Hands) {
        let weapon = crate::fixtures::plasma_pistol();
        let mut hands = Hands::new(&weapon);
        let mut shots = Vec::new();
        for tick in 0..ticks {
            let shot = hands.update(&weapon, held(tick));
            if shot.fired {
                shots.push((tick, shot.trigger));
            }
        }
        (shots, hands)
    }

    #[test]
    fn a_plasma_pistol_fires_the_first_triggers_bolt_when_the_button_is_let_go_early() {
        // a tap: pressed on tick 10, let go on tick 11, which fires it
        let (shots, hands) = plasma_shots(|t| t == 10, 40);
        assert_eq!(shots, [(11, 0)]);
        assert!((hands.age - 0.002).abs() < 1.0e-6, "the battery has aged by a shot's worth: {}", hands.age);
        // ... held for 17 ticks (a charge takes 18) it is the same
        let (shots, _) = plasma_shots(|t| (10..27).contains(&t), 60);
        assert_eq!(shots, [(27, 0)]);
    }

    #[test]
    fn a_plasma_pistol_held_until_the_charge_is_full_fires_the_overcharged_bolt_and_overheats() {
        let (shots, hands) = plasma_shots(|t| (10..60).contains(&t), 62);
        assert_eq!(shots, [(60, 1)], "nothing until the button is let go, and then the second trigger's shot");
        // a shot of all the gauge: the weapon is overheated and has aged by the shot's 0.11
        assert!(hands.overheated, "{hands:?}");
        assert!((hands.age - 0.11).abs() < 1.0e-6);
        // the weapon fires nothing until it has cooled to a quarter (0.65 a second takes 35 ticks of the 0.75)
        let weapon = crate::fixtures::plasma_pistol();
        let mut hands = hands;
        let mut fired_at = None;
        for tick in 0..120 {
            if hands.update(&weapon, tick % 2 == 0).fired {
                fired_at = Some(tick);
                break;
            }
        }
        assert!(matches!(fired_at, Some(t) if (30..45).contains(&t)), "{fired_at:?}");
    }

    #[test]
    fn heat_is_not_lost_while_a_full_charge_is_held() {
        let weapon = crate::fixtures::plasma_pistol();
        let mut hands = Hands::new(&weapon);
        // a bolt, and then a button held: the charge is full 18 ticks into the hold
        hands.update(&weapon, true);
        hands.update(&weapon, false);
        let mut heats = Vec::new();
        for _ in 0..60 {
            hands.update(&weapon, true);
            heats.push(hands.heat);
        }
        assert!(heats[0] > heats[10], "heat is lost while the charge fills: {heats:?}");
        let at_full = heats[19];
        assert!(heats[20..].iter().all(|h| *h == at_full), "and not once it is full: {heats:?}");
    }

    #[test]
    fn a_blow_cannot_be_struck_while_charging_and_it_starts_the_triggers_over() {
        let weapon = crate::fixtures::plasma_pistol();
        let mut hands = Hands::new(&weapon);
        let mut melee = Melee::default();
        hands.update(&weapon, true);
        assert!(hands.is_charging());
        melee.update(&weapon, &mut hands, true);
        assert!(!melee.is_striking(), "a charge is no time to strike a blow");
        // let go: the first shot fires, and now a blow can be struck: it lands after the first-person animation's key frame
        hands.update(&weapon, false);
        hands.update(&weapon, false);
        let mut landed = None;
        for tick in 0..60 {
            if melee.update(&weapon, &mut hands, tick == 0) {
                landed = Some(tick);
            }
            hands.update(&weapon, false);
        }
        // (33 frames less a quarter is a blow of 25 ticks, the key frame 4 less a quarter of 33, so 21 left to go: the
        // blow lands on the fifth tick after the press)
        assert_eq!(landed, Some(5), "{landed:?}");
    }

    #[test]
    fn the_rate_of_a_trigger_is_the_greater_of_its_two_and_what_charging_and_heat_allow() {
        let mut weapon = pistol();
        assert_eq!(trigger_rate(&weapon, 0), 3.5);
        weapon.triggers[0].final_rate_of_fire = 7.0;
        assert_eq!(trigger_rate(&weapon, 0), 7.0);
        weapon.triggers[0].initial_rate_of_fire = 0.0;
        weapon.triggers[0].final_rate_of_fire = 0.0;
        assert_eq!(trigger_rate(&weapon, 0), 1.0, "a trigger of no rate is read as one a second");
        assert_eq!(trigger_rate(&weapon, 5), 1.0, "and a trigger the weapon does not have");

        // the plasma pistol: a first trigger that charges, tapped every other tick, and a second that fires a full charge
        let mut plasma = pistol();
        plasma.magazines.clear();
        plasma.triggers[0].magazine_index = -1;
        plasma.triggers[0].initial_rate_of_fire = 0.0;
        plasma.triggers[0].final_rate_of_fire = 0.0;
        plasma.triggers[0].charging_time = 0.6;
        let second = plasma.triggers[0].clone();
        plasma.triggers.push(Trigger { charging_time: 0.0, ..second });
        assert_eq!(trigger_rate(&plasma, 0), 15.0);
        // 0.6 s is 18 ticks of charge, and a tick either side
        assert_eq!(trigger_rate(&plasma, 1), 30.0 / 19.0);
        // with heat: 1 + 0.65 x 3 seconds' worth at 0.16 a shot
        plasma.heat_loss_per_second = 0.65;
        plasma.triggers[0].heat_generated_per_round = 0.16;
        assert!((trigger_rate(&plasma, 0) - 2.95 / 0.16 / 3.0).abs() < 1.0e-4);
        // (a weapon with one trigger that charges is held to the charge)
        plasma.triggers.pop();
        plasma.triggers[0].heat_generated_per_round = 0.0;
        plasma.triggers[0].final_rate_of_fire = 10.0;
        assert_eq!(trigger_rate(&plasma, 0), 30.0 / 19.0);
    }
}
