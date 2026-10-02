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
}

impl Movement {
    /// How many numbers there are, for [`Movement::to_array`].
    pub const COUNT: usize = 19;

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
        }
    }

    /// Whether every number is usable: finite, speeds and sizes not negative,
    /// the pill with some radius. A map that fails this is refused at load.
    pub fn is_sane(&self) -> bool {
        let a = self.to_array();
        a.iter().all(|v| v.is_finite())
            && a[..12].iter().all(|v| *v >= 0.0)
            && self.collision_radius > 0.0
            && self.collision_height_standing >= 2.0 * self.collision_radius
    }
}
