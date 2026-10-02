//! Small invented maps for tests and examples, so that nothing here needs the
//! game's own data. Their movement numbers are made up too (they are in the
//! range of a real map's, which is what lets the tests mean something), not
//! read from any.

use alloc::vec::Vec;

use halo_map::collision::{Bsp2dReference, Bsp3dNode, CollisionBsp, Edge, Leaf, Plane3d, Surface, Vertex};
use halo_map::Movement;

use crate::map::MapData;

/// Half the side of [`flat_floor_map`]'s floor, in world units.
pub const FLOOR_HALF_SIZE: f32 = 50.0;

/// Where [`walled_floor_map`]'s wall is: solid from this `x` on.
pub const WALL_X: f32 = 10.0;

/// Where [`ramp_map`]'s ramp starts, along `x`.
pub const RAMP_START_X: f32 = 0.0;

const SOLID: i32 = -1;
const LEAF_0: i32 = i32::MIN;
const SIGN: i32 = i32::MIN;

/// Made-up movement values of about the size a real map's are.
pub fn movement() -> Movement {
    Movement {
        run_forward_speed: 2.2,
        run_backward_speed: 1.9,
        run_sideways_speed: 1.9,
        run_acceleration: 0.3,
        sneak_forward_speed: 0.8,
        sneak_backward_speed: 0.6,
        sneak_sideways_speed: 0.5,
        sneak_acceleration: 0.15,
        airborne_acceleration: 0.03,
        collision_radius: 0.2,
        collision_height_standing: 0.7,
        collision_height_crouching: 0.5,
        minimum_normal_k: 0.7,
        downhill_k0: -0.3,
        downhill_k1: -0.7,
        downhill_velocity_scale: 1.2,
        uphill_k0: 0.3,
        uphill_k1: 0.7,
        uphill_velocity_scale: 0.6,
    }
}

/// One surface of a fixture: a plane designator and a polygon whose corners
/// go anticlockwise seen from the open side.
struct Polygon {
    plane: i32,
    flags: u8,
    corners: Vec<[f32; 3]>,
}

/// Fill in the surfaces, edges and vertices of a closed mesh from its
/// polygons: each pair of polygons that meet along an edge share one edge
/// (left of it the one that runs along it, right of it the one that runs
/// back); an edge on the border of the mesh has no surface on its right.
fn mesh(polygons: &[Polygon]) -> (Vec<Surface>, Vec<Edge>, Vec<Vertex>) {
    let mut vertices: Vec<Vertex> = Vec::new();
    let mut edges: Vec<Edge> = Vec::new();
    let mut surfaces = Vec::new();
    for (surface_index, polygon) in polygons.iter().enumerate() {
        let ids: Vec<i32> = polygon
            .corners
            .iter()
            .map(|c| match vertices.iter().position(|v| v.point == *c) {
                Some(i) => i as i32,
                None => {
                    vertices.push(Vertex { point: *c, first_edge: 0 });
                    vertices.len() as i32 - 1
                }
            })
            .collect();
        // (the edge, and which side of it this polygon is) of each corner's outgoing edge
        let mut loop_edges: Vec<(usize, usize)> = Vec::new();
        for i in 0..ids.len() {
            let (a, b) = (ids[i], ids[(i + 1) % ids.len()]);
            // an edge that already runs the other way, with no surface on its right yet
            match edges.iter().position(|e| e.vertices == [b, a] && e.surfaces[1] == -1) {
                Some(e) => {
                    edges[e].surfaces[1] = surface_index as i32;
                    loop_edges.push((e, 1));
                }
                None => {
                    edges.push(Edge { vertices: [a, b], edges: [0, 0], surfaces: [surface_index as i32, -1] });
                    loop_edges.push((edges.len() - 1, 0));
                }
            }
        }
        for i in 0..loop_edges.len() {
            let (e, side) = loop_edges[i];
            edges[e].edges[side] = loop_edges[(i + 1) % loop_edges.len()].0 as i32;
        }
        surfaces.push(Surface {
            plane: polygon.plane,
            first_edge: loop_edges[0].0 as i32,
            flags: polygon.flags,
            breakable_surface: 0,
            material: 0,
        });
    }
    for (i, e) in edges.iter().enumerate() {
        vertices[e.vertices[0] as usize].first_edge = i as i32;
    }
    (surfaces, edges, vertices)
}

fn unit_plane(n: [f32; 3], d: f32) -> Plane3d {
    let length = crate::math::sqrt(n[0] * n[0] + n[1] * n[1] + n[2] * n[2]);
    Plane3d { n: [n[0] / length, n[1] / length, n[2] / length], d: d / length }
}

fn world_bounds() -> [f32; 6] {
    let h = FLOOR_HALF_SIZE;
    [-h, h, -h, h, -5.0, 30.0]
}

fn map_of(collision: CollisionBsp) -> MapData {
    MapData { collision, world_bounds: world_bounds(), movement: movement() }
}

/// An open space above a flat floor at height 0 with solid ground under it. The
/// floor's polygon is the square of side 100 centred on the origin, but its
/// 2D BSP has no nodes, so the floor extends as far as the plane does.
pub fn flat_floor_map() -> MapData {
    let h = FLOOR_HALF_SIZE;
    let (surfaces, edges, vertices) = mesh(&[Polygon {
        plane: 0,
        flags: halo_map::collision::SURFACE_CLIMBABLE,
        corners: alloc::vec![[-h, -h, 0.0], [h, -h, 0.0], [h, h, 0.0], [-h, h, 0.0]],
    }]);
    map_of(CollisionBsp {
        // z = 0: solid behind (below) it, leaf 0 in front (above) it
        bsp3d_nodes: Vec::from([Bsp3dNode { plane: 0, children: [SOLID, LEAF_0] }]),
        planes: Vec::from([Plane3d { n: [0.0, 0.0, 1.0], d: 0.0 }]),
        // the leaf's one reference is the floor surface
        leaves: Vec::from([Leaf { flags: 0, bsp2d_reference_count: 1, first_bsp2d_reference: 0 }]),
        bsp2d_references: Vec::from([Bsp2dReference { plane: 0, root: LEAF_0 }]),
        bsp2d_nodes: Vec::new(),
        surfaces,
        edges,
        vertices,
    })
}

/// The same floor with a wall across it: solid from [`WALL_X`] on, a vertical
/// face looking back along -x, 20 world units high.
pub fn walled_floor_map() -> MapData {
    let (h, w) = (FLOOR_HALF_SIZE, WALL_X);
    let (surfaces, edges, vertices) = mesh(&[
        Polygon {
            plane: 0,
            flags: halo_map::collision::SURFACE_CLIMBABLE,
            corners: alloc::vec![[-h, -h, 0.0], [w, -h, 0.0], [w, h, 0.0], [-h, h, 0.0]],
        },
        Polygon {
            plane: 1 | SIGN,
            flags: 0,
            corners: alloc::vec![[w, -h, 0.0], [w, -h, 20.0], [w, h, 20.0], [w, h, 0.0]],
        },
    ]);
    map_of(CollisionBsp {
        // z = 0: solid below; above it x = WALL_X: open before, solid after
        bsp3d_nodes: Vec::from([
            Bsp3dNode { plane: 0, children: [SOLID, 1] },
            Bsp3dNode { plane: 1, children: [LEAF_0, SOLID] },
        ]),
        planes: Vec::from([Plane3d { n: [0.0, 0.0, 1.0], d: 0.0 }, Plane3d { n: [1.0, 0.0, 0.0], d: w }]),
        leaves: Vec::from([Leaf { flags: 0, bsp2d_reference_count: 2, first_bsp2d_reference: 0 }]),
        bsp2d_references: Vec::from([
            Bsp2dReference { plane: 0, root: LEAF_0 },
            // (the leaf is on the back of the wall's plane, which its surface is the back of)
            Bsp2dReference { plane: 1 | SIGN, root: LEAF_0 | 1 },
        ]),
        bsp2d_nodes: Vec::new(),
        surfaces,
        edges,
        vertices,
    })
}

/// A flat floor at height 0 up to [`RAMP_START_X`], then a ramp up along +x
/// that rises `rise` for each unit it runs (for 50 units).
pub fn ramp_map(rise: f32) -> MapData {
    let (h, s) = (FLOOR_HALF_SIZE, RAMP_START_X);
    let (surfaces, edges, vertices) = mesh(&[
        Polygon {
            plane: 0,
            flags: halo_map::collision::SURFACE_CLIMBABLE,
            corners: alloc::vec![[-h, -h, 0.0], [s, -h, 0.0], [s, h, 0.0], [-h, h, 0.0]],
        },
        Polygon {
            plane: 1,
            flags: halo_map::collision::SURFACE_CLIMBABLE,
            corners: alloc::vec![[s, -h, 0.0], [h, -h, rise * (h - s)], [h, h, rise * (h - s)], [s, h, 0.0]],
        },
    ]);
    // the ramp's plane: z = rise * (x - s), that is, -rise x + z = -rise s
    let ramp = unit_plane([-rise, 0.0, 1.0], -rise * s);
    map_of(CollisionBsp {
        // open above both planes
        bsp3d_nodes: Vec::from([
            Bsp3dNode { plane: 0, children: [SOLID, 1] },
            Bsp3dNode { plane: 1, children: [SOLID, LEAF_0] },
        ]),
        planes: Vec::from([Plane3d { n: [0.0, 0.0, 1.0], d: 0.0 }, ramp]),
        leaves: Vec::from([Leaf { flags: 0, bsp2d_reference_count: 2, first_bsp2d_reference: 0 }]),
        bsp2d_references: Vec::from([
            Bsp2dReference { plane: 0, root: LEAF_0 },
            Bsp2dReference { plane: 1, root: LEAF_0 | 1 },
        ]),
        bsp2d_nodes: Vec::new(),
        surfaces,
        edges,
        vertices,
    })
}
