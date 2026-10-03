//! The on-foot movement values of a map's tag data: what the engine reads from
//! the globals tag's player information (speeds, accelerations) and from the
//! multiplayer player's biped tag (the collision pill, the slope limits and the
//! speed scales on slopes) to move a player. They are read, never retuned.

/// The movement values, in the units of the tags: world units, world units a
/// second, and (for the slope limits) the vertical component of a surface's
/// normal. The names are the tags' own.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Movement {
    // globals tag, player information
    pub run_forward_speed: f32,
    pub run_backward_speed: f32,
    pub run_sideways_speed: f32,
    /// World units a second a second.
    pub run_acceleration: f32,
    pub sneak_forward_speed: f32,
    pub sneak_backward_speed: f32,
    pub sneak_sideways_speed: f32,
    pub sneak_acceleration: f32,
    pub airborne_acceleration: f32,
    // the multiplayer biped tag
    pub collision_radius: f32,
    pub collision_height_standing: f32,
    pub collision_height_crouching: f32,
    /// The steepest surface a player stands on, as the least `n.z` it has
    /// (the tag's maximum slope angle, which the cache holds as a cosine).
    pub minimum_normal_k: f32,
    /// Moving down a slope slows (above `downhill_k0`, full speed; at or below
    /// `downhill_k1`, `downhill_velocity_scale` of it) by the direction's `z`.
    pub downhill_k0: f32,
    pub downhill_k1: f32,
    pub downhill_velocity_scale: f32,
    pub uphill_k0: f32,
    pub uphill_k1: f32,
    pub uphill_velocity_scale: f32,
    /// The upward speed a jump gives, world units a *tick* (the tag's own
    /// unit: the engine adds it to the velocity as it is).
    pub jump_velocity: f32,
    /// How fast the crouch changes (the cache's runtime value, per tick: the
    /// part of the way between standing and crouching a tick covers).
    pub crouch_transition_velocity: f32,
    /// Landing: the longest the soft and the hard landing last, seconds; the
    /// speeds a second at which a landing is soft and at which it is hard,
    /// and the speed at which the hard landing lasts the longest.
    pub maximum_soft_landing_time: f32,
    pub maximum_hard_landing_time: f32,
    pub minimum_soft_landing_velocity: f32,
    pub minimum_hard_landing_velocity: f32,
    pub maximum_hard_landing_velocity: f32,
    // the globals tag, falling damage (the cache's runtime values, world units a tick)
    /// A landing faster than this hurts, and one at `maximum_damage_velocity` hurts the most.
    pub minimum_damage_velocity: f32,
    pub maximum_damage_velocity: f32,
    /// Falling faster than this, hurt or not, is the fall that kills.
    pub maximum_falling_velocity: f32,
}

impl Movement {
    /// How many numbers there are, for [`Movement::to_array`].
    pub const COUNT: usize = 29;

    pub fn to_array(&self) -> [f32; Self::COUNT] {
        [
            self.run_forward_speed,
            self.run_backward_speed,
            self.run_sideways_speed,
            self.run_acceleration,
            self.sneak_forward_speed,
            self.sneak_backward_speed,
            self.sneak_sideways_speed,
            self.sneak_acceleration,
            self.airborne_acceleration,
            self.collision_radius,
            self.collision_height_standing,
            self.collision_height_crouching,
            self.minimum_normal_k,
            self.downhill_k0,
            self.downhill_k1,
            self.downhill_velocity_scale,
            self.uphill_k0,
            self.uphill_k1,
            self.uphill_velocity_scale,
            self.jump_velocity,
            self.crouch_transition_velocity,
            self.maximum_soft_landing_time,
            self.maximum_hard_landing_time,
            self.minimum_soft_landing_velocity,
            self.minimum_hard_landing_velocity,
            self.maximum_hard_landing_velocity,
            self.minimum_damage_velocity,
            self.maximum_damage_velocity,
            self.maximum_falling_velocity,
        ]
    }

    pub fn from_array(a: [f32; Self::COUNT]) -> Movement {
        Movement {
            run_forward_speed: a[0],
            run_backward_speed: a[1],
            run_sideways_speed: a[2],
            run_acceleration: a[3],
            sneak_forward_speed: a[4],
            sneak_backward_speed: a[5],
            sneak_sideways_speed: a[6],
            sneak_acceleration: a[7],
            airborne_acceleration: a[8],
            collision_radius: a[9],
            collision_height_standing: a[10],
            collision_height_crouching: a[11],
            minimum_normal_k: a[12],
            downhill_k0: a[13],
            downhill_k1: a[14],
            downhill_velocity_scale: a[15],
            uphill_k0: a[16],
            uphill_k1: a[17],
            uphill_velocity_scale: a[18],
            jump_velocity: a[19],
            crouch_transition_velocity: a[20],
            maximum_soft_landing_time: a[21],
            maximum_hard_landing_time: a[22],
            minimum_soft_landing_velocity: a[23],
            minimum_hard_landing_velocity: a[24],
            maximum_hard_landing_velocity: a[25],
            minimum_damage_velocity: a[26],
            maximum_damage_velocity: a[27],
            maximum_falling_velocity: a[28],
        }
    }

    /// Whether every number is usable: finite, speeds and sizes not negative,
    /// the pill with some radius. A map that fails this is refused at load.
    pub fn is_sane(&self) -> bool {
        let a = self.to_array();
        a.iter().all(|v| v.is_finite())
            && a[..12].iter().all(|v| *v >= 0.0)
            && a[19..].iter().all(|v| *v >= 0.0)
            && self.collision_radius > 0.0
            && self.collision_height_standing >= 2.0 * self.collision_radius
    }
}
