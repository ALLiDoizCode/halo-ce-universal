//! Moving a pill (a vertical capsule: a player's collision shape) through the
//! map's collision BSP: a port of the engine's `collision_move_pill`
//! (`source/physics/collisions.c`, `collision_features.c`).
//!
//! The engine does not sweep the pill. It turns the world into features the
//! pill's reference point (the centre of its bottom sphere) cannot enter: each
//! nearby surface becomes a slab (a prism) as thick as the pill's radius in
//! front of it, each vertex a vertical capsule and each edge a pair of
//! prisms and a pair of cylinders (all as wide as the pill's radius, and as
//! tall as the pill is). Moving the point is then a ray test against those
//! features, clipping the motion to the plane of what it hits, up to three
//! times (a floor, a wall and a corner).

use alloc::vec::Vec;

use halo_map::collision::{project, CollisionBsp, Plane3d, MAX_SPHERE_FEATURES, PROJECTION};

use crate::math::{along, cross, dot, magnitude, magnitude_squared, normalize, scale, sqrt, sub, Vec3, EPSILON};

pub(crate) const NONE: i32 = -1;

/// The most contacts one move reports (`collision_move_pill`'s callers pass 16).
pub(crate) const MAX_CONTACTS: usize = 16;

/// The engine's cap on a prism's polygon.
const MAX_PRISM_POINTS: usize = 8;

/// What `collision_get_features_in_sphere` adds to a sphere's radius.
const FEATURE_MARGIN: f32 = 0.0625;

/// Which surface a feature came from, and its flags.
#[derive(Debug, Clone, Copy)]
struct Origin {
    surface: i32,
    flags: u8,
}

#[derive(Debug, Clone, Copy)]
struct Sphere {
    center: Vec3,
    radius: f32,
    origin: Origin,
}

#[derive(Debug, Clone, Copy)]
struct Cylinder {
    base: Vec3,
    height: Vec3,
    width: f32,
    origin: Origin,
}

#[derive(Debug, Clone)]
struct Prism {
    plane: Plane3d,
    /// The slab's thickness in front of the plane.
    height: f32,
    axis: usize,
    sign: bool,
    points: Vec<[f32; 2]>,
    origin: Origin,
}

/// Where a move met a feature: `collision_plane` in the engine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Contact {
    /// How far along the (remaining) move it happened, 0 to 1.
    pub t: f32,
    pub point: Vec3,
    pub plane: Plane3d,
    /// The collision BSP's surface, or [`NONE`] for a crease between two.
    pub surface: i32,
    /// The surface's `SURFACE_*` flags.
    pub flags: u8,
}

/// What a pill move came to.
#[derive(Debug, Clone)]
pub(crate) struct Moved {
    pub position: Vec3,
    pub velocity: Vec3,
    pub contacts: Vec<Contact>,
}

#[derive(Debug, Clone, Default)]
struct Features {
    spheres: Vec<Sphere>,
    cylinders: Vec<Cylinder>,
    prisms: Vec<Prism>,
}

fn plane_distance(plane: &Plane3d, point: &Vec3) -> f32 {
    plane.n[0] * point[0] + plane.n[1] * point[1] + plane.n[2] * point[2] - plane.d
}

fn projection_axis(n: &Vec3) -> usize {
    let (i, j, k) = (n[0].abs(), n[1].abs(), n[2].abs());
    if k >= j && k >= i {
        2
    } else if j >= i {
        1
    } else {
        0
    }
}

impl Features {
    fn is_empty(&self) -> bool {
        self.spheres.is_empty() && self.cylinders.is_empty() && self.prisms.is_empty()
    }

    /// collision_features_from_point
    fn add_point(&mut self, point: Vec3, height: f32, width: f32, origin: Origin) {
        if self.spheres.len() < MAX_SPHERE_FEATURES {
            self.spheres.push(Sphere { center: point, radius: width, origin });
        }
        if height > 0.0 {
            let new_height = point[2] - height;
            if self.spheres.len() < MAX_SPHERE_FEATURES {
                self.spheres.push(Sphere { center: [point[0], point[1], new_height], radius: width, origin });
            }
            if self.cylinders.len() < MAX_SPHERE_FEATURES {
                self.cylinders.push(Cylinder {
                    base: [point[0], point[1], new_height],
                    height: [0.0, 0.0, height],
                    width,
                    origin,
                });
            }
        }
    }

    /// collision_features_from_line
    fn add_line(&mut self, point: Vec3, vector: Vec3, height: f32, width: f32, origin: Origin) {
        if self.cylinders.len() < MAX_SPHERE_FEATURES {
            self.cylinders.push(Cylinder { base: point, height: vector, width, origin });
        }
        if height <= 0.0 {
            return;
        }
        if self.cylinders.len() < MAX_SPHERE_FEATURES {
            self.cylinders.push(Cylinder {
                base: [point[0], point[1], point[2] - height],
                height: vector,
                width,
                origin,
            });
        }
        // perpendicular2d, normalize2d
        let mut n = [-vector[1], vector[0]];
        if crate::math::normalize2(&mut n) == 0.0 {
            return;
        }
        let distance = point[0] * n[0] + point[1] * n[1];
        let end = [point[0] + vector[0], point[1] + vector[1], point[2] + vector[2]];
        let mut points = [point, end, [end[0], end[1], end[2] - height], [point[0], point[1], point[2] - height]];

        // one slab on each side of the edge's vertical wall, the second with
        // the polygon turned over
        for pass in 0..2 {
            let (normal, d) = if pass == 0 { ([n[0], n[1], 0.0], distance) } else { ([-n[0], -n[1], 0.0], -distance) };
            let axis = projection_axis(&normal);
            let sign = normal[axis] > 0.0;
            if self.prisms.len() < MAX_SPHERE_FEATURES {
                self.prisms.push(Prism {
                    plane: Plane3d { n: normal, d },
                    height: width,
                    axis,
                    sign,
                    points: points.iter().map(|p| project(p, axis, sign)).collect(),
                    origin,
                });
            }
            points.swap(1, 3);
        }
    }

    /// collision_features_from_polygon
    fn add_polygon(&mut self, points: &[Vec3], plane: Plane3d, height: f32, width: f32, origin: Origin) {
        if self.prisms.len() >= MAX_SPHERE_FEATURES {
            return;
        }
        let axis = projection_axis(&plane.n);
        let sign = plane.n[axis] > 0.0;
        let mut prism = Prism {
            plane,
            height: width,
            axis,
            sign,
            points: points.iter().map(|p| project(p, axis, sign)).collect(),
            origin,
        };
        if height > 0.0 && plane.n[2] < 0.0 {
            prism.plane.d -= height * prism.plane.n[2];
            if axis != 2 {
                let component = (PROJECTION[axis][sign as usize][1] == 2) as usize;
                for p in &mut prism.points {
                    p[component] -= height;
                }
            }
        }
        self.prisms.push(prism);
    }

    /// collision_features_from_vertex
    fn add_vertex(&mut self, bsp: &CollisionBsp, vertex_index: i32, height: f32, width: f32) {
        let vertex = &bsp.vertices[vertex_index as usize];
        let edge = &bsp.edges[vertex.first_edge as usize];
        let Some(surface) = bsp.surfaces.get(edge.surfaces[0] as usize) else { return };
        self.add_point(vertex.point, height, width, Origin { surface: edge.surfaces[0], flags: surface.flags });
    }

    /// collision_features_from_edge
    fn add_edge(&mut self, bsp: &CollisionBsp, edge_index: i32, height: f32, width: f32) {
        let edge = &bsp.edges[edge_index as usize];
        // (an edge with no surface on one side is not an edge of the closed map the engine expects)
        let (Some(surface0), Some(surface1)) =
            (bsp.surfaces.get(edge.surfaces[0] as usize), bsp.surfaces.get(edge.surfaces[1] as usize))
        else {
            return;
        };
        if surface0.plane == surface1.plane {
            return;
        }
        let vertex0 = bsp.vertices[edge.vertices[0] as usize].point;
        let vertex1 = bsp.vertices[edge.vertices[1] as usize].point;
        let plane0_index = surface0.plane & i32::MAX;
        let plane1_index = surface1.plane & i32::MAX;
        let plane0 = bsp.planes[plane0_index as usize];
        let plane1 = bsp.planes[plane1_index as usize];
        let plane0_negated = surface0.plane < 0;
        let plane1_negated = surface1.plane < 0;
        let vector = sub(&vertex1, &vertex0);

        let valid = if plane0_index == plane1_index {
            true
        } else if plane0_negated == plane1_negated {
            dot(&cross(&plane0.n, &plane1.n), &vector) > -EPSILON
        } else {
            dot(&cross(&plane0.n, &plane1.n), &vector) < EPSILON
        };
        if valid {
            self.add_line(vertex0, vector, height, width, Origin { surface: edge.surfaces[0], flags: surface0.flags });
        }
    }

    /// collision_features_from_surface
    fn add_surface(&mut self, bsp: &CollisionBsp, surface_index: i32, height: f32, width: f32) {
        let Some(points) = bsp.surface_polygon(surface_index as usize, MAX_PRISM_POINTS) else { return };
        let Some(plane) = bsp.surface_plane(surface_index as usize) else { return };
        let flags = bsp.surfaces[surface_index as usize].flags;
        self.add_polygon(&points, plane, height, width, Origin { surface: surface_index, flags });
    }

    /// collision_features_test_vector: the first feature the point meets
    /// moving along `vector`, if it moves into it.
    fn test_vector(&self, point: &Vec3, vector: &Vec3) -> Hit {
        let mut closest_t = f32::MAX;
        let mut closest: Option<(Plane3d, Origin)> = None;
        let mut consider = |t: f32, plane: Plane3d, origin: Origin| {
            if closest_t > t && dot(vector, &plane.n) < -EPSILON {
                closest_t = t;
                closest = Some((plane, origin));
            }
        };
        for s in &self.spheres {
            if let Some((t, plane)) = s.test_vector(point, vector) {
                consider(t, plane, s.origin);
            }
        }
        for c in &self.cylinders {
            if let Some((t, plane)) = c.test_vector(point, vector) {
                consider(t, plane, c.origin);
            }
        }
        for p in &self.prisms {
            if let Some((t, plane)) = p.test_vector(point, vector) {
                consider(t, plane, p.origin);
            }
        }
        match closest {
            Some((plane, origin)) => Hit::Contact(Contact {
                t: closest_t,
                point: along(point, vector, closest_t),
                plane,
                surface: origin.surface,
                flags: origin.flags,
            }),
            None => Hit::Clear([point[0] + vector[0], point[1] + vector[1], point[2] + vector[2]]),
        }
    }
}

enum Hit {
    Contact(Contact),
    /// Nothing in the way: where the whole move ends.
    Clear(Vec3),
}

impl Sphere {
    /// collision_sphere_test_vector
    fn test_vector(&self, point: &Vec3, vector: &Vec3) -> Option<(f32, Plane3d)> {
        let w = sub(&self.center, point);
        let distance = magnitude_squared(&w) - self.radius * self.radius;
        let t;
        if distance <= 0.0 {
            t = 0.0;
        } else {
            let mut projection = dot(&w, vector);
            if projection > 0.0 {
                let vector_squared = magnitude_squared(vector);
                let discriminant = projection * projection - vector_squared * distance;
                if discriminant >= 0.0 {
                    projection -= sqrt(discriminant);
                    if projection <= vector_squared {
                        t = projection / vector_squared;
                    } else {
                        return None;
                    }
                } else {
                    return None;
                }
            } else {
                return None;
            }
        }
        let point_on_vector = scale(vector, t);
        let mut n = sub(&point_on_vector, &w);
        if normalize(&mut n) == 0.0 {
            n = [0.0, 0.0, 1.0];
        }
        Some((t, Plane3d { n, d: dot(&self.center, &n) + self.radius }))
    }
}

impl Cylinder {
    /// collision_cylinder_test_vector
    fn test_vector(&self, point: &Vec3, vector: &Vec3) -> Option<(f32, Plane3d)> {
        let height_squared = magnitude_squared(&self.height);
        let height_vector = dot(&self.height, vector);
        let vector_squared = magnitude_squared(vector);
        let quadratic_a = height_squared * vector_squared - height_vector * height_vector;
        if quadratic_a == 0.0 {
            return None;
        }

        let w = sub(point, &self.base);
        let vector_w = dot(vector, &w);
        let height_w = dot(&self.height, &w);
        let w_squared = magnitude_squared(&w);
        let width_squared = self.width * self.width;
        let quadratic_b = height_w * height_vector - height_squared * vector_w;
        let quadratic_c = (w_squared - width_squared) * height_squared - height_w * height_w;
        let discriminant = quadratic_b * quadratic_b - quadratic_a * quadratic_c;
        if discriminant < 0.0 {
            return None;
        }

        let discriminant = sqrt(discriminant);
        let inverse_quadratic_a = 1.0 / quadratic_a;
        let mut minimum_t = (quadratic_b - discriminant) * inverse_quadratic_a;
        let mut maximum_t = (quadratic_b + discriminant) * inverse_quadratic_a;
        if minimum_t > 1.0 || maximum_t < 0.0 {
            return None;
        }
        if minimum_t < 0.0 {
            minimum_t = 0.0;
        }
        if maximum_t > 1.0 {
            maximum_t = 1.0;
        }
        if height_vector != 0.0 {
            let bottom_t = -height_w / height_vector;
            let top_t = (height_squared - height_w) / height_vector;
            if height_vector > 0.0 {
                if minimum_t < bottom_t {
                    minimum_t = bottom_t;
                }
                maximum_t = maximum_t.min(top_t);
            } else {
                if minimum_t < top_t {
                    minimum_t = top_t;
                }
                maximum_t = maximum_t.min(bottom_t);
            }
            if minimum_t > maximum_t {
                return None;
            }
        } else if height_w < 0.0 || height_w > height_squared {
            return None;
        }

        let point_on_vector = along(&w, vector, minimum_t);
        let height_t = -(dot(&point_on_vector, &self.height) / height_squared);
        let mut n = along(&point_on_vector, &self.height, height_t);
        if normalize(&mut n) == 0.0 {
            n = [1.0, 0.0, 0.0];
        }
        Some((minimum_t, Plane3d { n, d: dot(&self.base, &n) + self.width }))
    }
}

impl Prism {
    /// collision_prism_test_vector
    fn test_vector(&self, point: &Vec3, vector: &Vec3) -> Option<(f32, Plane3d)> {
        let mut t_out = 0.0f32;
        let mut t_in = 1.0f32;
        let d = plane_distance(&self.plane, point);
        let mut vn = self.plane.n[2] * vector[2];
        vn += self.plane.n[1] * vector[1];
        vn += self.plane.n[0] * vector[0];

        if vn != 0.0 {
            let oovn = 1.0 / vn;
            let t0 = -oovn * d;
            let t1 = -oovn * (d - self.height);
            if vn > 0.0 {
                if t_out < t0 {
                    t_out = t0;
                }
                t_in = t_in.min(t1);
            } else {
                if t_out < t1 {
                    t_out = t1;
                }
                t_in = t_in.min(t0);
            }
            if t_out > t_in {
                return None;
            }
        } else if d < 0.0 || d >= self.height {
            return None;
        }

        let p3d = along(point, &self.plane.n, -d);
        let v3d = along(vector, &self.plane.n, -vn);
        let p2d = project(&p3d, self.axis, self.sign);
        let v2d = project(&v3d, self.axis, self.sign);

        let count = self.points.len();
        for i in 0..count {
            let next = if i + 1 >= count { 0 } else { i + 1 };
            let p0 = self.points[i];
            let p1 = self.points[next];
            let w = [p1[0] - p0[0], p1[1] - p0[1]];
            let x = [p2d[0] - p0[0], p2d[1] - p0[1]];
            let vw = v2d[0] * w[1] - v2d[1] * w[0];
            let wx = w[0] * x[1] - w[1] * x[0];
            if vw != 0.0 {
                let t_edge = wx / vw;
                if vw < 0.0 {
                    if t_out < t_edge {
                        t_out = t_edge;
                    }
                } else if t_in > t_edge {
                    t_in = t_edge;
                }
                if t_out > t_in {
                    return None;
                }
            } else if wx < 0.0 {
                return None;
            }
        }
        Some((t_out, Plane3d { n: self.plane.n, d: self.plane.d + self.height }))
    }
}

impl Sphere {
    /// collision_sphere_test_point: how deep `point` is in the sphere (made
    /// `margin` bigger), and the way out.
    fn test_point(&self, point: &Vec3, margin: f32) -> Option<(f32, Plane3d)> {
        let radius = self.radius + margin;
        let w = sub(point, &self.center);
        let distance_squared = magnitude_squared(&w);
        if distance_squared >= radius * radius || distance_squared.is_nan() {
            return None;
        }
        let distance = sqrt(distance_squared);
        let n = if distance > 0.0 { scale(&w, 1.0 / distance) } else { [0.0, 0.0, 1.0] };
        Some((radius - distance, Plane3d { n, d: dot(&self.center, &n) + radius }))
    }
}

impl Cylinder {
    /// collision_cylinder_test_point (the cylinder `margin` wider)
    fn test_point(&self, point: &Vec3, margin: f32) -> Option<(f32, Plane3d)> {
        let width = self.width + margin;
        let w = sub(point, &self.base);
        let height_distance = dot(&w, &self.height);
        if height_distance < 0.0 {
            return None;
        }
        let height_squared = magnitude_squared(&self.height);
        if !(height_distance <= height_squared
            && magnitude_squared(&w) * height_squared - height_distance * height_distance
                < width * width * height_squared)
        {
            return None;
        }
        let mut n =
            if height_squared > 0.0 { sub(&w, &scale(&self.height, height_distance / height_squared)) } else { w };
        let radial_distance = normalize(&mut n);
        if radial_distance == 0.0 {
            n = [0.0, 0.0, 1.0];
        }
        Some((width - radial_distance, Plane3d { n, d: dot(&self.base, &n) + width }))
    }
}

impl Prism {
    /// collision_prism_test_point (the slab `margin` thicker)
    fn test_point(&self, point: &Vec3, margin: f32) -> Option<(f32, Plane3d)> {
        let height = self.height + margin;
        let distance = plane_distance(&self.plane, point);
        if !(distance >= 0.0 && distance < height) {
            return None;
        }
        let on_plane = along(point, &self.plane.n, -distance);
        let p = project(&on_plane, self.axis, self.sign);
        let count = self.points.len();
        for i in 0..count {
            let next = if i + 1 < count { i + 1 } else { 0 };
            let v0 = [self.points[i][0] - p[0], self.points[i][1] - p[1]];
            let v1 = [self.points[next][0] - p[0], self.points[next][1] - p[1]];
            if v0[0] * v1[1] - v0[1] * v1[0] < 0.0 {
                return None;
            }
        }
        Some((height - distance, Plane3d { n: self.plane.n, d: self.plane.d + height }))
    }
}

impl Features {
    /// The features the point is inside, when each is `margin` bigger
    /// (collision_features_test_point asks for the deepest of them): how
    /// deep, the way out, and where from.
    fn inside(&self, point: &Vec3, margin: f32) -> Vec<(f32, Plane3d, Origin)> {
        let mut found = Vec::new();
        for s in &self.spheres {
            found.extend(s.test_point(point, margin).map(|(d, p)| (d, p, s.origin)));
        }
        for c in &self.cylinders {
            found.extend(c.test_point(point, margin).map(|(d, p)| (d, p, c.origin)));
        }
        for p in &self.prisms {
            found.extend(p.test_point(point, margin).map(|(d, pl)| (d, pl, p.origin)));
        }
        found
    }
}

/// How a pill stands at a position, as far as validating a report goes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Footing {
    /// How far the pill's base sphere is inside what it should stay out of:
    /// 0 for a pill the engine's movement could have put there.
    pub penetration: f32,
    /// Something is under the pill, within `drop`, that is not an overhang
    /// (a wall counts: the engine keeps a player on what they moved along).
    pub supported: bool,
    /// Something a player stands on is under the pill, within `drop`: ground
    /// the tags' slope limit allows, or a climbable surface.
    pub standing: bool,
}

/// Judge a pill whose base sphere's centre is at `base`: how deep it is in
/// the map, and whether it stands on something (touching a surface that is
/// not an overhang, or within `drop` of one); and whether it is on ground
/// a player stands on, with `minimum_normal_k` the least vertical part of its
/// normal that is (the tags').
pub(crate) fn footing(
    bsp: &CollisionBsp,
    base: Vec3,
    height: f32,
    radius: f32,
    drop: f32,
    minimum_normal_k: f32,
) -> Footing {
    let center = [base[0], base[1], base[2] + height * 0.5];
    let features = features_in_sphere(bsp, center, height * 0.5 + radius + drop, height, radius);
    // Anything that is not an overhang holds a player up as far as validating
    // goes, not only a slope within the tags' limit: the engine keeps a player
    // on the surface they moved along for a tick or two after the ground
    // gets steeper than that, and on a climbable one (a ladder's) however
    // steep it is. A report that touches nothing at all is in the air.
    let walkable =
        |plane: &Plane3d, flags: u8| flags & halo_map::collision::SURFACE_CLIMBABLE != 0 || plane.n[2] >= 0.0;
    // (a pill the engine put somewhere touches what it rests on, and is a hair
    // inside it as often as a hair outside)
    let penetration = features.inside(&base, 0.0).iter().fold(0.0f32, |deepest, (depth, _, _)| deepest.max(*depth));
    // standing on something is being within `drop` of it
    let near = features.inside(&base, drop);
    let supported = near.iter().any(|(_, plane, origin)| walkable(plane, origin.flags));
    let standing = near.iter().any(|(_, plane, origin)| {
        origin.flags & halo_map::collision::SURFACE_CLIMBABLE != 0 || plane.n[2] >= minimum_normal_k
    });
    Footing { penetration, supported, standing }
}

/// collision_get_features_in_sphere, for the structure BSP alone: what the
/// pill may meet around `center`. `height` and `width` are the pill's.
fn features_in_sphere(bsp: &CollisionBsp, center: Vec3, radius: f32, height: f32, width: f32) -> Features {
    let mut features = Features::default();
    let hits = bsp.test_sphere(center, radius + FEATURE_MARGIN);
    // (a sphere that reaches only vertices finds nothing to collide with)
    if hits.surfaces.is_empty() && hits.edges.is_empty() {
        return features;
    }
    for &v in &hits.vertices {
        features.add_vertex(bsp, v, height, width);
    }
    for &e in &hits.edges {
        features.add_edge(bsp, e, height, width);
    }
    for &s in &hits.surfaces {
        features.add_surface(bsp, s, height, width);
    }
    features
}

fn clip_position_to_plane(position: &Vec3, plane: &Plane3d) -> Vec3 {
    along(position, &plane.n, -plane_distance(plane, position))
}

fn clip_velocity_to_plane(velocity: &Vec3, plane: &Plane3d) -> Vec3 {
    let distance = -dot(velocity, &plane.n);
    along(velocity, &plane.n, distance)
}

fn clip_position_to_line(position: &Vec3, point: &Vec3, vector: &Vec3) -> Vec3 {
    let offset = sub(position, point);
    along(point, vector, dot(&offset, vector) / magnitude_squared(vector))
}

fn clip_velocity_to_line(velocity: &Vec3, vector: &Vec3) -> Vec3 {
    scale(vector, dot(velocity, vector) / magnitude_squared(vector))
}

/// real_math.c: line_from_planes3d
fn line_from_planes(plane0: &Plane3d, plane1: &Plane3d) -> Option<(Vec3, Vec3)> {
    let direction = cross(&plane0.n, &plane1.n);
    let determinant = magnitude_squared(&direction);
    if determinant.abs() < EPSILON {
        return None;
    }
    let local = cross(&plane1.n, &direction);
    let distance = plane0.d;
    let point = [local[0] * distance, local[1] * distance, local[2] * distance];
    let local = cross(&direction, &plane0.n);
    let distance = plane1.d;
    let inverse = 1.0 / determinant;
    let point = [
        (local[0] * distance + point[0]) * inverse,
        (local[1] * distance + point[1]) * inverse,
        (local[2] * distance + point[2]) * inverse,
    ];
    Some((point, direction))
}

/// real_math.c: point_from_planes3d
fn point_from_planes(plane0: &Plane3d, plane1: &Plane3d, plane2: &Plane3d) -> Option<Vec3> {
    let c = cross(&plane0.n, &plane1.n);
    let determinant = dot(&c, &plane2.n);
    if determinant.abs() < EPSILON {
        return None;
    }
    let c = cross(&plane1.n, &plane2.n);
    let distance = plane0.d;
    let point = [c[0] * distance, c[1] * distance, c[2] * distance];
    let c = cross(&plane2.n, &plane0.n);
    let distance = plane1.d;
    let point = [c[0] * distance + point[0], c[1] * distance + point[1], c[2] * distance + point[2]];
    let c = cross(&plane0.n, &plane1.n);
    let distance = plane2.d;
    let determinant = 1.0 / determinant;
    Some([
        (c[0] * distance + point[0]) * determinant,
        (c[1] * distance + point[1]) * determinant,
        (c[2] * distance + point[2]) * determinant,
    ])
}

fn nearly_zero(v: &Vec3) -> bool {
    v[0].abs() < EPSILON && v[1].abs() < EPSILON && v[2].abs() < EPSILON
}

/// collision_move_point: move the point along `old_velocity`, sliding along
/// what it meets.
fn move_point(old_position: Vec3, old_velocity: Vec3, features: &Features) -> Moved {
    let mut contacts: Vec<Contact> = Vec::new();
    let mut velocity = old_velocity;
    let mut clipped_position = old_position;
    let mut clipped_velocity = old_velocity;
    let mut clip_count = 0usize;
    let mut clip_indices = [0usize; 3];
    let mut clip_plane = Plane3d { n: [0.0; 3], d: 0.0 };
    let mut clip_line_point = [0.0; 3];
    let mut clip_line_vector = [0.0; 3];
    let mut clip_point = [0.0; 3];

    while !nearly_zero(&clipped_velocity) {
        match features.test_vector(&clipped_position, &clipped_velocity) {
            Hit::Contact(contact) => {
                contacts.push(contact);
                let position = contact.point;
                velocity = scale(&velocity, 1.0 - contact.t);

                let mut new_clip_indices = [0usize; 3];
                let mut new_clip_count = 1;
                new_clip_indices[0] = contacts.len() - 1;
                clip_plane = contacts[new_clip_indices[0]].plane;
                clipped_velocity = clip_velocity_to_plane(&velocity, &clip_plane);
                clipped_position = clip_position_to_plane(&position, &clip_plane);

                if clip_count > 0 {
                    let first = contacts[clip_indices[0]].plane;
                    let newest = contacts[new_clip_indices[0]].plane;
                    let line = if dot(&clipped_velocity, &first.n) < -EPSILON {
                        line_from_planes(&newest, &first)
                    } else {
                        None
                    };
                    if let Some((line_point, line_vector)) = line {
                        clip_line_point = line_point;
                        clip_line_vector = line_vector;
                        new_clip_count = 2;
                        new_clip_indices[1] = clip_indices[0];
                        clipped_velocity = clip_velocity_to_line(&velocity, &clip_line_vector);
                        clipped_position = clip_position_to_line(&position, &clip_line_point, &clip_line_vector);

                        if clip_count > 1 {
                            let second = contacts[clip_indices[1]].plane;
                            if dot(&clipped_velocity, &second.n) < -EPSILON {
                                let newest = contacts[new_clip_indices[0]].plane;
                                let other = contacts[new_clip_indices[1]].plane;
                                if let Some(p) = point_from_planes(&newest, &other, &second) {
                                    clip_point = p;
                                    new_clip_count = 3;
                                    new_clip_indices[2] = clip_indices[1];
                                    clipped_velocity = [0.0; 3];
                                    clipped_position = clip_point;
                                }
                            }
                        }
                    } else if clip_count > 1 {
                        let second = contacts[clip_indices[1]].plane;
                        if dot(&clipped_velocity, &second.n) < -EPSILON {
                            let newest = contacts[new_clip_indices[0]].plane;
                            if let Some((line_point, line_vector)) = line_from_planes(&newest, &second) {
                                clip_line_point = line_point;
                                clip_line_vector = line_vector;
                                new_clip_count = 2;
                                new_clip_indices[1] = clip_indices[1];
                                clipped_velocity = clip_velocity_to_line(&velocity, &clip_line_vector);
                                clipped_position =
                                    clip_position_to_line(&position, &clip_line_point, &clip_line_vector);
                            }
                        }
                    }
                }

                clip_count = new_clip_count;
                clip_indices = new_clip_indices;
            }
            Hit::Clear(end) => {
                clipped_position = end;
                break;
            }
        }
        if contacts.len() >= MAX_CONTACTS {
            break;
        }
    }

    let new_velocity = match clip_count {
        0 => old_velocity,
        1 => clip_velocity_to_plane(&old_velocity, &clip_plane),
        2 => clip_velocity_to_line(&old_velocity, &clip_line_vector),
        _ => [0.0; 3],
    };

    // where two or three surfaces held the point, one more contact that says
    // which way is out of the crease
    if clip_count > 1 && contacts.len() < MAX_CONTACTS {
        let last = contacts[clip_indices[clip_count - 1]];
        let mut contact =
            Contact { t: last.t, point: last.point, plane: Plane3d { n: [0.0; 3], d: 0.0 }, surface: NONE, flags: 0 };
        let mut minimum_k = 0.0f32;
        let mut steepest: Option<usize> = None;
        for (clip_index, &c) in clip_indices[..clip_count].iter().enumerate() {
            let k = contacts[c].plane.n[2];
            if k < minimum_k {
                minimum_k = k;
                steepest = Some(clip_index);
            }
        }
        let mut keep = true;
        if clip_count == 2 {
            let mut n = match steepest {
                Some(s) => {
                    let steepest_plane = contacts[clip_indices[s]].plane;
                    if s == 0 {
                        cross(&clip_line_vector, &steepest_plane.n)
                    } else {
                        cross(&steepest_plane.n, &clip_line_vector)
                    }
                }
                None => {
                    let k = clip_line_vector[2] / magnitude_squared(&clip_line_vector);
                    along(&[0.0, 0.0, 1.0], &clip_line_vector, -k)
                }
            };
            if normalize(&mut n) != 0.0 {
                contact.plane = Plane3d { n, d: dot(&clip_line_point, &n) };
            } else {
                keep = false;
            }
        } else {
            match steepest {
                Some(s) => {
                    let steepest_plane = contacts[clip_indices[s]].plane;
                    let mut n = along(&[0.0, 0.0, 1.0], &steepest_plane.n, -steepest_plane.n[2]);
                    if normalize(&mut n) != 0.0 {
                        contact.plane = Plane3d { n, d: dot(&clip_point, &n) };
                    } else {
                        keep = false;
                    }
                }
                None => {
                    let n = [0.0, 0.0, 1.0];
                    contact.plane = Plane3d { n, d: dot(&clip_point, &n) };
                }
            }
        }
        if keep {
            contacts.push(contact);
        }
    }

    Moved { position: clipped_position, velocity: new_velocity, contacts }
}

/// collision_move_pill: move a pill whose bottom sphere's centre is at
/// `position` (the pill stands `height` above it, and is `radius` wide) by
/// `velocity`, against the structure BSP.
pub(crate) fn move_pill(bsp: &CollisionBsp, position: Vec3, velocity: Vec3, height: f32, radius: f32) -> Moved {
    let center = [
        position[0] + velocity[0] * 0.5,
        position[1] + velocity[1] * 0.5,
        height * 0.5 + (position[2] + velocity[2] * 0.5),
    ];
    let reach = magnitude(&velocity) * 0.5 + height * 0.5 + radius;
    let features = features_in_sphere(bsp, center, reach, height, radius);
    if features.is_empty() {
        return Moved {
            position: [position[0] + velocity[0], position[1] + velocity[1], position[2] + velocity[2]],
            velocity,
            contacts: Vec::new(),
        };
    }
    move_point(position, velocity, &features)
}
