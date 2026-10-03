//! A player on foot: the engine's biped movement (`biped_update_moving` and
//! `biped_update_physics` in `source/units/bipeds.c`) for a player who walks,
//! runs, stops against walls, slides along them, climbs and descends slopes,
//! falls off ledges, jumps, lands (a hard landing holds the player still for a
//! moment) and crouches.
//!
//! Every number is the map's tags' ([`halo_map::Movement`]); nothing is
//! retuned. A tick moves the player's [`Body`] by what their [`Controls`] ask
//! for, through the map's collision BSP (see [`crate::pill`]).
//!
//! Each client runs this for its own player (so that its movement shows at
//! once) and reports where it got to; the server only checks the report (see
//! [`crate::step`]).

use halo_map::collision::{Plane3d, SURFACE_CLIMBABLE};

use crate::map::MapData;
use crate::math::{
    add, along, cross, dot, magnitude, magnitude_squared, normalize, normalize2, scale, sin_cos, sub, Vec3, EPSILON,
};
use crate::pill::{move_pill, Contact, NONE};
use crate::TICKS_PER_SECOND;

/// The engine's gravity, `global_gravity`, in world units a tick a tick.
pub const GRAVITY: f32 = 0.003_565_179_2;

/// What the player asks for in one tick: the controls the engine hands the
/// player's unit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Controls {
    /// Throttle ahead (negative: back), -1 to 1.
    pub forward: f32,
    /// Throttle to the left (negative: right), -1 to 1.
    pub strafe: f32,
    /// Where the player faces, radians, 0 along +x and turning towards +y.
    pub yaw: f32,
    /// Where the player aims, radians, up positive.
    pub pitch: f32,
    /// The jump button is held.
    pub jump: bool,
    /// The crouch button is held.
    pub crouch: bool,
}

impl Controls {
    pub fn standing(yaw: f32) -> Controls {
        Controls { forward: 0.0, strafe: 0.0, yaw, pitch: 0.0, jump: false, crouch: false }
    }
}

/// Where a player is and how they are moving: what a tick of walking carries
/// to the next.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Body {
    /// The unit's origin, near the feet, in world units.
    pub position: Vec3,
    /// World units a *tick* (the engine's unit; the trace and the wire have
    /// it a second).
    pub velocity: Vec3,
    /// Not standing on anything.
    pub airborne: bool,
    /// The plane the player stands on as of the last tick, or the engine's
    /// `depths_of_hell` when there was none.
    pub ground_plane: Plane3d,
    /// The collision BSP's surface the player stands on, or -1.
    pub support_surface: i32,
    /// How fast the last tick drove the player into the ground it came to,
    /// in world units a tick; 0 when it did not land. (What the falling
    /// damage reads: [`halo_map::Movement::minimum_damage_velocity`].)
    pub landing_velocity: f32,
    /// How far down into the crouch the player is, 0 (standing) to 1: it
    /// moves by [`halo_map::Movement::crouch_transition_velocity`] a tick
    /// towards where the player's stance is.
    pub crouch: f32,
    /// The player's stance, the crouch the animation holds from the end of
    /// the last tick: the crouch button was held, or there was no room to
    /// stand. (The `crouch` follows it a tick later.)
    pub crouching: bool,
    /// How far down the pill the last tick moved was: the `crouch` before it.
    pub pill_crouch: f32,
    /// Ticks on the ground since the last jump, at most 127: a jump needs
    /// more than 5.
    pub jump_timer: u8,
    /// How the last landing hit: [`NO_LANDING`], [`SOFT_LANDING`] or
    /// [`HARD_LANDING`]; a hard landing holds the player still.
    pub landing: i8,
    /// Ticks into the landing, and how many it lasts.
    pub landing_counter: i8,
    pub landing_time: i8,
}

/// `Body::landing` when the player has not just landed.
pub const NO_LANDING: i8 = -1;
pub const SOFT_LANDING: i8 = 0;
pub const HARD_LANDING: i8 = 1;

/// What the engine's `depths_of_hell` is: a plane far below the map, which a
/// player who is on nothing stands on as far as the movement rules go.
const DEPTHS_OF_HELL: Plane3d = Plane3d { n: [0.0, 0.0, 1.0], d: -256.0 };

impl Body {
    /// A player placed at `position`, at rest, not yet having touched
    /// anything (the first tick finds the ground, or falls).
    pub fn at(position: Vec3) -> Body {
        Body {
            position,
            velocity: [0.0; 3],
            airborne: false,
            ground_plane: DEPTHS_OF_HELL,
            support_surface: NONE,
            landing_velocity: 0.0,
            crouch: 0.0,
            crouching: false,
            pill_crouch: 0.0,
            jump_timer: 127,
            landing: NO_LANDING,
            landing_counter: 0,
            landing_time: 0,
        }
    }

    /// Whether the player is crouched as far as the others and the server are
    /// concerned (what a client reports as [`crate::FLAG_CROUCHED`]): in the
    /// crouch stance, or still on the way up from it, with a pill shorter than
    /// a standing player's (the one the last tick moved had the crouch the
    /// tick began with).
    pub fn crouched(&self) -> bool {
        self.crouching || self.crouch > 0.0 || self.pill_crouch > 0.0
    }

    /// The velocity in world units a second.
    pub fn velocity_per_second(&self) -> Vec3 {
        scale(&self.velocity, TICKS_PER_SECOND as f32)
    }
}

pub use crate::pill::Footing;

/// How a standing player's origin at `position` sits in the map: how deep in
/// what the player has to stay out of (0 for a position the movement itself
/// could have left them in), and whether they are standing on something.
/// What the server's check of a reported position is made of.
pub fn footing(map: &MapData, position: Vec3) -> Footing {
    let m = &map.movement;
    let radius = m.collision_radius;
    crate::pill::footing(
        &map.collision,
        [position[0], position[1], position[2] + radius],
        m.collision_height_standing - 2.0 * radius,
        radius,
        crate::GROUND_TOLERANCE,
        m.minimum_normal_k,
    )
}

/// A player put at `position`, who stands there until they are on the ground:
/// the position they have once they have settled (or `position` itself if
/// they have nothing to land on within a second's fall). Where a player who
/// is placed in the map should start from, so that nothing of what follows is
/// the fall of a spawn that was a little above the ground.
pub fn settled(map: &MapData, position: Vec3) -> Vec3 {
    let mut body = Body::at(position);
    for _ in 0..TICKS_PER_SECOND {
        walk(map, &mut body, &Controls::standing(0.0));
        if !body.airborne {
            return body.position;
        }
    }
    position
}

/// How far the throttle must be pushed for the player to move at all.
const THROTTLE_DEAD_ZONE: f32 = 0.1;

/// A tick of walking: the player's [`Body`] after `controls` for 1/30 second.
pub fn walk(map: &MapData, body: &mut Body, controls: &Controls) {
    let m = &map.movement;
    let ticks = TICKS_PER_SECOND as f32;

    // biped_update: a throttle too small to mean it is none
    let (mut throttle_forward, mut throttle_strafe) = (controls.forward, controls.strafe);
    if throttle_forward * throttle_forward + throttle_strafe * throttle_strafe < THROTTLE_DEAD_ZONE * THROTTLE_DEAD_ZONE
    {
        throttle_forward = 0.0;
        throttle_strafe = 0.0;
    }

    // biped_update_moving: the speeds the throttle asks for, a tick's worth. A
    // player held by a hard landing asks for nothing (and pushes against what
    // moves them with all they have: the movement penalty)
    let crouch = body.crouch;
    let uncrouch = 1.0 - crouch;
    let held = body.landing == HARD_LANDING;
    let (movement_desired, acceleration_maximum, airborne_acceleration_maximum, movement_penalty) = if held {
        ([0.0; 3], HELD_ACCELERATION / ticks, 0.0, 1.0)
    } else {
        // (standing and crouching speeds blend by how far down the player is)
        let (run, sneak) = if throttle_forward <= 0.0 {
            (m.run_backward_speed, m.sneak_backward_speed)
        } else {
            (m.run_forward_speed, m.sneak_forward_speed)
        };
        let forward_speed = run * uncrouch + sneak * crouch;
        let sideways_speed = m.run_sideways_speed * uncrouch + m.sneak_sideways_speed * crouch;
        let acceleration = m.run_acceleration * uncrouch + m.sneak_acceleration * crouch;
        (
            [throttle_forward * forward_speed / ticks, throttle_strafe * sideways_speed / ticks, 0.0],
            acceleration / ticks,
            m.airborne_acceleration / ticks,
            0.0,
        )
    };

    // the crouch moves towards the stance, a step a tick (the pill below is
    // the one the player had at the start of the tick)
    let transition = m.crouch_transition_velocity;
    let crouch_delta;
    let new_crouch;
    if body.crouching {
        let to_go = 1.0 - body.crouch;
        if to_go <= transition {
            new_crouch = 1.0;
            crouch_delta = to_go;
        } else {
            new_crouch = body.crouch + transition;
            crouch_delta = transition;
        }
    } else {
        let to_go = -body.crouch;
        if to_go >= -transition {
            new_crouch = 0.0;
            crouch_delta = to_go;
        } else {
            new_crouch = body.crouch - transition;
            crouch_delta = -transition;
        }
    }
    // (in the air, the legs coming up or going down move the pill's base)
    let crouch_velocity = if body.airborne && crouch_delta.abs() > 0.01 {
        (m.collision_height_standing - m.collision_height_crouching) * crouch_delta
    } else {
        0.0
    };

    // the player's facing, flat (biped_update flattens it)
    let (yaw_sine, yaw_cosine) = sin_cos(controls.yaw);
    let forward = [yaw_cosine, yaw_sine];

    // biped_get_physics_pill: the pill stands on the origin, so its base
    // sphere's centre is a radius above; its height is the standing one
    // shortened by how far down the player is
    let radius = m.collision_radius;
    let height = m.collision_height_standing + (m.collision_height_crouching - m.collision_height_standing) * crouch
        - 2.0 * radius;
    let position = [body.position[0], body.position[1], body.position[2] + radius];

    // biped_update_physics
    let velocity = body.velocity;
    let mut facing = [0.0f32; 2];
    let new_velocity: Vec3 = if body.airborne {
        let desired = [
            (movement_desired[0] * forward[0] - forward[1] * movement_desired[1]) * (1.0 - movement_penalty),
            (movement_desired[1] * forward[0] + movement_desired[0] * forward[1]) * (1.0 - movement_penalty),
        ];
        let acceleration = [desired[0] - velocity[0], desired[1] - velocity[1]];
        let mut direction = acceleration;
        let change = if normalize2(&mut direction) > airborne_acceleration_maximum {
            [direction[0] * airborne_acceleration_maximum, direction[1] * airborne_acceleration_maximum]
        } else {
            acceleration
        };
        [change[0] + velocity[0], change[1] + velocity[1], velocity[2] - GRAVITY]
    } else {
        let mut speed = magnitude(&movement_desired);
        let ground_normal = body.ground_plane.n;
        let mut move_direction: Vec3;
        if ground_normal[2] > EPSILON {
            facing = [
                movement_desired[0] * forward[0] - forward[1] * movement_desired[1],
                movement_desired[1] * forward[0] + movement_desired[0] * forward[1],
            ];
            move_direction = [
                facing[0],
                facing[1],
                movement_desired[2] - (facing[1] * ground_normal[1] + facing[0] * ground_normal[0]) / ground_normal[2],
            ];
            normalize(&mut move_direction);
        } else {
            // a wall: along it, by where the player aims
            let (pitch_sine, pitch_cosine) = sin_cos(controls.pitch);
            let aiming = [pitch_cosine * yaw_cosine, pitch_cosine * yaw_sine, pitch_sine];
            let mut left = cross(&[0.0, 0.0, 1.0], &aiming);
            normalize(&mut left);
            let on_plane = |v: &Vec3| along(v, &ground_normal, -dot(v, &ground_normal));
            let along_forward = on_plane(&aiming);
            let along_left = on_plane(&left);
            facing = [
                movement_desired[0] * forward[0] - forward[1] * movement_desired[1],
                movement_desired[1] * forward[0] + movement_desired[0] * forward[1],
            ];
            move_direction = [
                along_forward[0] * movement_desired[0] + along_left[0] * movement_desired[1],
                along_forward[1] * movement_desired[0] + along_left[1] * movement_desired[1],
                along_forward[2] * movement_desired[0] + along_left[2] * movement_desired[1] + movement_desired[2],
            ];
            move_direction[2] *= 5.0;
            normalize(&mut move_direction);
        }

        // slower up a steep slope, faster down one
        let k = move_direction[2];
        if k <= m.downhill_k1 {
            speed *= m.downhill_velocity_scale;
        } else if k < m.downhill_k0 {
            speed *= (k - m.downhill_k0) * (m.downhill_velocity_scale - 1.0) / (m.downhill_k1 - m.downhill_k0) + 1.0;
        } else if k >= m.uphill_k1 {
            speed *= m.uphill_velocity_scale;
        } else if k > m.uphill_k0 {
            speed *= (k - m.uphill_k0) * (m.uphill_velocity_scale - 1.0) / (m.uphill_k1 - m.uphill_k0) + 1.0;
        }

        let desired_velocity = scale(&move_direction, speed * (1.0 - movement_penalty));
        let acceleration = sub(&desired_velocity, &velocity);
        let mut direction = acceleration;
        let mut change = if normalize(&mut direction) > acceleration_maximum {
            scale(&direction, acceleration_maximum)
        } else {
            acceleration
        };
        // pressed into the ground a little, so that it stays found
        change[0] -= ground_normal[0] * (1.0 / 128.0);
        change[1] -= ground_normal[1] * (1.0 / 128.0);
        change[2] -= ground_normal[2] * (1.0 / 128.0);
        add(&change, &velocity)
    };

    let mut velocity_asked = new_velocity;
    velocity_asked[2] += crouch_velocity;
    let mut moved = move_pill(&map.collision, position, velocity_asked, height, radius);
    let mut clipped_position = moved.position;
    let mut clipped_velocity = moved.velocity;
    let mut stick_surface = NONE;

    // nothing hit, but standing on a surface: stay on the walkable surface
    // next to it that the move is about to leave it for
    if moved.contacts.is_empty() && body.support_surface != NONE {
        if let Some(stuck) =
            stick_to_neighbour(map, body.support_surface, radius, &mut clipped_position, &mut clipped_velocity)
        {
            moved.contacts = alloc::vec![stuck];
            stick_surface = stuck.surface;
        }
    }

    // the direction the player pushes, flat and unit length, or as it was
    let facing_squared = facing[1] * facing[1] + facing[0] * facing[0];
    if facing_squared > EPSILON * EPSILON {
        let inverse = 1.0 / crate::math::sqrt(facing_squared);
        facing = [facing[0] * inverse, facing[1] * inverse];
    }

    // which of what was hit holds the player up
    let mut best: Option<usize> = None;
    let mut best_walkable = false;
    let mut best_stuck = false;
    let mut best_normal_k = -f32::MAX;
    let mut best_velocity_dot = -f32::MAX;
    for (index, contact) in moved.contacts.iter().enumerate() {
        let walkable = contact.flags & SURFACE_CLIMBABLE != 0;
        let stuck = stick_surface != NONE && best.is_some_and(|b| moved.contacts[b].surface == stick_surface);
        let velocity_dot = -dot(&contact.plane.n, &new_velocity);
        let better = if walkable {
            if contact.plane.n[0] * facing[0] + contact.plane.n[1] * facing[1] > 0.5 {
                false
            } else if !best_walkable || stuck {
                true
            } else if best_stuck {
                false
            } else {
                velocity_dot > best_velocity_dot
            }
        } else {
            !best_walkable && contact.plane.n[2] > best_normal_k
        };
        if better {
            best_velocity_dot = velocity_dot;
            best_walkable = walkable;
            best = Some(index);
            best_stuck = stuck;
            best_normal_k = contact.plane.n[2];
        }
    }

    let mut airborne = true;
    let mut ground_plane = DEPTHS_OF_HELL;
    let mut support_surface = NONE;
    let mut landing_velocity = 0.0;
    if let Some(best) = best {
        let contact: &Contact = &moved.contacts[best];
        // too steep to stand on, unless the map says it can be walked
        let supported = best_walkable || best_stuck || best_normal_k >= m.minimum_normal_k;
        if supported {
            airborne = false;
            ground_plane = contact.plane;
            support_surface = contact.surface;
            if support_surface == NONE || support_surface != stick_surface {
                landing_velocity = -dot(&velocity_asked, &ground_plane.n);
            }
        }
    }

    clipped_velocity[2] -= crouch_velocity;

    body.position = [clipped_position[0], clipped_position[1], clipped_position[2] - radius];
    body.velocity = clipped_velocity;
    body.airborne = airborne;
    body.ground_plane = ground_plane;
    body.support_surface = support_surface;
    body.landing_velocity = landing_velocity;
    body.pill_crouch = crouch;
    body.crouch = new_crouch;

    // a player who stands up where there is no room is held in the crouch
    let mut crouching = controls.crouch;
    if body.crouch != 0.0 && !controls.crouch {
        let standing = m.collision_height_standing - 2.0 * radius;
        let at = [body.position[0], body.position[1], body.position[2] + radius];
        if crate::pill::footing(&map.collision, at, standing, radius, 0.0, m.minimum_normal_k).penetration
            > NO_ROOM_TO_STAND
        {
            crouching = true;
        }
    }
    let cannot_stand = crouching && !controls.crouch;

    // biped_start_landing: a landing hard enough holds the player a while
    if body.landing_velocity > 0.0 {
        start_landing(m, body);
    }

    // biped_update_jumping: a player on the ground who has landed a few ticks
    // ago (and is not held by a hard landing) leaps if the button is down
    if !cannot_stand && !body.airborne && body.landing != HARD_LANDING {
        body.jump_timer = body.jump_timer.saturating_add(1).min(127);
        if controls.jump && body.jump_timer > 5 {
            // biped_jump: the velocity up becomes the jump's if it was less
            if body.velocity[2] < m.jump_velocity {
                body.velocity[2] = m.jump_velocity;
            }
            body.airborne = true;
            body.jump_timer = 0;
            body.support_surface = NONE;
        }
    }

    // biped_update_landing: a player on the ground counts off the landing
    if !body.airborne && body.landing != NO_LANDING {
        body.landing_counter = body.landing_counter.saturating_add(1);
        if body.landing_counter >= body.landing_time {
            body.landing = NO_LANDING;
        }
    }

    body.crouching = crouching;
}

/// How far into the map a pill standing up may be for there to be room for
/// the player to stand (a hair of rounding is not no room).
const NO_ROOM_TO_STAND: f32 = 0.001;

/// How fast a player held by a hard landing slows (the engine's default
/// acceleration limit for a biped it gives no speeds), world units a second
/// a second.
const HELD_ACCELERATION: f32 = 0.16;

/// `biped_start_landing`: how long a landing at `body.landing_velocity` holds
/// the player, and whether it is a soft or a hard one.
fn start_landing(m: &halo_map::Movement, body: &mut Body) {
    let ticks = TICKS_PER_SECOND as f32;
    let landing_velocity = body.landing_velocity;
    let minimum_soft = m.minimum_soft_landing_velocity * (1.0 / ticks);
    let minimum_hard = m.minimum_hard_landing_velocity * (1.0 / ticks);
    if landing_velocity < minimum_soft {
        return;
    }
    let (velocity, range, recovery, kind) = if landing_velocity < minimum_hard {
        (landing_velocity - minimum_soft, minimum_hard - minimum_soft, m.maximum_soft_landing_time, SOFT_LANDING)
    } else {
        (
            landing_velocity,
            m.maximum_hard_landing_velocity * (1.0 / ticks) - minimum_hard,
            m.maximum_hard_landing_time,
            HARD_LANDING,
        )
    };
    let recovery = recovery * ticks;
    if range > 0.0 {
        let share = (velocity / range).clamp(0.0, 1.0);
        body.landing = kind;
        body.landing_counter = 0;
        // (the engine truncates it to a char)
        body.landing_time = (recovery * share) as i64 as i8;
    }
}

/// The part of `biped_update_physics` that keeps a walking player on the
/// ground when a move would take them off the edge of the surface they stand
/// on onto a walkable one next to it (a crest, the foot of a slope): the
/// neighbour that the move is heading away from, near enough, becomes the
/// ground, and the player is put on it. Returns the contact that stands in
/// for a hit, with the position and velocity adjusted.
fn stick_to_neighbour(
    map: &MapData,
    support_surface: i32,
    width: f32,
    position: &mut Vec3,
    velocity: &mut Vec3,
) -> Option<Contact> {
    let bsp = &map.collision;
    let surface = bsp.surfaces.get(support_surface as usize)?;
    let plane = bsp.surface_plane(support_surface as usize)?;
    let distance = -(plane.n[0] * position[0] + plane.n[1] * position[1] + plane.n[2] * position[2] - plane.d);
    let point =
        [plane.n[0] * distance + position[0], plane.n[1] * distance + position[1], plane.n[2] * distance + position[2]];

    let mut best: Option<(i32, Plane3d, f32)> = None;
    let mut best_distance_squared = f32::MAX;
    let mut edge_index = surface.first_edge;
    loop {
        let edge = bsp.edges.get(edge_index as usize)?;
        let reverse = (support_surface == edge.surfaces[1]) as usize;
        let neighbour_index = edge.surfaces[1 - reverse];
        if neighbour_index != NONE {
            let neighbour = bsp.surfaces.get(neighbour_index as usize)?;
            if neighbour.flags & SURFACE_CLIMBABLE != 0 {
                let neighbour_plane = bsp.surface_plane(neighbour_index as usize)?;
                let velocity_dot = dot(&neighbour_plane.n, velocity);
                if velocity_dot > 0.0
                    && neighbour_plane.n[0] * position[0]
                        + neighbour_plane.n[1] * position[1]
                        + neighbour_plane.n[2] * position[2]
                        - neighbour_plane.d
                        > width * -0.5
                {
                    let vertex0 = bsp.vertices[edge.vertices[0] as usize].point;
                    let vertex1 = bsp.vertices[edge.vertices[1] as usize].point;
                    let edge_vector = sub(&vertex1, &vertex0);
                    let t = ((point[0] - vertex0[0]) * edge_vector[0]
                        + (point[1] - vertex0[1]) * edge_vector[1]
                        + (point[2] - vertex0[2]) * edge_vector[2])
                        / magnitude_squared(&edge_vector);
                    let closest = if t < 0.0 {
                        vertex0
                    } else if t > 1.0 {
                        vertex1
                    } else {
                        along(&vertex0, &edge_vector, t)
                    };
                    let distance_squared = magnitude_squared(&sub(&point, &closest));
                    if distance_squared < best_distance_squared {
                        best_distance_squared = distance_squared;
                        best = Some((neighbour_index, neighbour_plane, velocity_dot));
                    }
                }
            }
        }
        edge_index = edge.edges[reverse];
        if edge_index == surface.first_edge {
            break;
        }
    }

    let (best_index, best_plane, best_velocity_dot) = best?;
    if !(best_distance_squared <= (width * 2.0) * (width * 2.0) && best_velocity_dot <= 1.6 / TICKS_PER_SECOND as f32) {
        return None;
    }
    let distance = best_plane.n[0] * position[0] + best_plane.n[1] * position[1] + best_plane.n[2] * position[2]
        - (best_plane.d + width);
    if distance.abs() > width * 0.5 {
        return None;
    }
    let new_velocity_dot = dot(&best_plane.n, velocity);
    *position = along(position, &best_plane.n, -distance);
    let one_tick = 1.0 / TICKS_PER_SECOND as f32;
    if new_velocity_dot > -one_tick {
        *velocity = along(velocity, &best_plane.n, -(new_velocity_dot + one_tick));
    }
    Some(Contact {
        t: 0.0,
        point: along(position, &best_plane.n, -width),
        plane: best_plane,
        surface: best_index,
        flags: 0,
    })
}
