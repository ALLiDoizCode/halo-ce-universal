//! A player on foot: the engine's biped movement (`biped_update_moving` and
//! `biped_update_physics` in `source/units/bipeds.c`) for a player who walks,
//! runs, stops against walls, slides along them, climbs and descends slopes
//! and falls off ledges. Jumping, crouching and the landing pause are not
//! here yet.
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
}

impl Controls {
    pub fn standing(yaw: f32) -> Controls {
        Controls { forward: 0.0, strafe: 0.0, yaw, pitch: 0.0 }
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
    /// in world units a tick; 0 when it did not land.
    pub landing_velocity: f32,
}

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
        }
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
    if throttle_forward * throttle_forward + throttle_strafe * throttle_strafe
        < THROTTLE_DEAD_ZONE * THROTTLE_DEAD_ZONE
    {
        throttle_forward = 0.0;
        throttle_strafe = 0.0;
    }

    // biped_update_moving: the speeds the throttle asks for, a tick's worth
    let forward_speed = if throttle_forward <= 0.0 { m.run_backward_speed } else { m.run_forward_speed };
    let sideways_speed = m.run_sideways_speed;
    let movement_desired = [throttle_forward * forward_speed / ticks, throttle_strafe * sideways_speed / ticks, 0.0];
    let acceleration_maximum = m.run_acceleration / ticks;
    let airborne_acceleration_maximum = m.airborne_acceleration / ticks;

    // the player's facing, flat (biped_update flattens it)
    let (yaw_sine, yaw_cosine) = sin_cos(controls.yaw);
    let forward = [yaw_cosine, yaw_sine];

    // biped_get_physics_pill: the pill stands on the origin, so its base
    // sphere's centre is a radius above
    let radius = m.collision_radius;
    let height = m.collision_height_standing - 2.0 * radius;
    let position = [body.position[0], body.position[1], body.position[2] + radius];

    // biped_update_physics
    let velocity = body.velocity;
    let new_velocity: Vec3;
    let mut facing = [0.0f32; 2];
    if body.airborne {
        let desired = [
            movement_desired[0] * forward[0] - forward[1] * movement_desired[1],
            movement_desired[1] * forward[0] + movement_desired[0] * forward[1],
        ];
        let acceleration = [desired[0] - velocity[0], desired[1] - velocity[1]];
        let mut direction = acceleration;
        let change = if normalize2(&mut direction) > airborne_acceleration_maximum {
            [direction[0] * airborne_acceleration_maximum, direction[1] * airborne_acceleration_maximum]
        } else {
            acceleration
        };
        new_velocity = [change[0] + velocity[0], change[1] + velocity[1], velocity[2] - GRAVITY];
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

        let desired_velocity = scale(&move_direction, speed * (1.0 - 0.0));
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
        new_velocity = add(&change, &velocity);
    }

    let mut moved = move_pill(&map.collision, position, new_velocity, height, radius);
    let mut clipped_position = moved.position;
    let mut clipped_velocity = moved.velocity;
    let mut stick_surface = NONE;

    // nothing hit, but standing on a surface: stay on the walkable surface
    // next to it that the move is about to leave it for
    if moved.contacts.is_empty() && body.support_surface != NONE {
        if let Some(stuck) = stick_to_neighbour(map, body.support_surface, radius, &mut clipped_position, &mut clipped_velocity)
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
                landing_velocity = -dot(&new_velocity, &ground_plane.n);
            }
        }
    }

    body.position = [clipped_position[0], clipped_position[1], clipped_position[2] - radius];
    body.velocity = clipped_velocity;
    body.airborne = airborne;
    body.ground_plane = ground_plane;
    body.support_surface = support_surface;
    body.landing_velocity = landing_velocity;
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
    let point = [
        plane.n[0] * distance + position[0],
        plane.n[1] * distance + position[1],
        plane.n[2] * distance + position[2],
    ];

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
    if !(best_distance_squared <= (width * 2.0) * (width * 2.0) && best_velocity_dot <= 1.6 / TICKS_PER_SECOND as f32)
    {
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
