//! Small invented maps for tests and examples, so that nothing here needs the
//! game's own data.

use alloc::vec::Vec;

use halo_map::collision::{Bsp2dReference, Bsp3dNode, CollisionBsp, Edge, Leaf, Plane3d, Surface, Vertex};

use crate::map::MapData;

/// Half the side of [`flat_floor_map`]'s floor, in world units.
pub const FLOOR_HALF_SIZE: f32 = 50.0;

const SOLID: i32 = -1;
const LEAF_0: i32 = i32::MIN;

/// An open space above a flat floor at height 0 with solid ground under it. The
/// floor's polygon is the square of side 100 centred on the origin, but its
/// 2D BSP has no nodes, so the floor extends as far as the plane does.
pub fn flat_floor_map() -> MapData {
    let h = FLOOR_HALF_SIZE;
    let corners = [[-h, -h], [h, -h], [h, h], [-h, h]];
    let collision = CollisionBsp {
        // z = 0: solid behind (below) it, leaf 0 in front (above) it
        bsp3d_nodes: Vec::from([Bsp3dNode { plane: 0, children: [SOLID, LEAF_0] }]),
        planes: Vec::from([Plane3d { n: [0.0, 0.0, 1.0], d: 0.0 }]),
        // the leaf's one reference is the floor surface
        leaves: Vec::from([Leaf { flags: 0, bsp2d_reference_count: 1, first_bsp2d_reference: 0 }]),
        bsp2d_references: Vec::from([Bsp2dReference { plane: 0, root: LEAF_0 }]),
        bsp2d_nodes: Vec::new(),
        surfaces: Vec::from([Surface { plane: 0, first_edge: 0, flags: 0, breakable_surface: 0, material: 0 }]),
        // four edges around the square, each with the surface on its left
        edges: (0..4)
            .map(|i: i32| Edge { vertices: [i, (i + 1) % 4], edges: [(i + 1) % 4, (i + 3) % 4], surfaces: [0, -1] })
            .collect(),
        vertices: corners
            .iter()
            .enumerate()
            .map(|(i, c)| Vertex { point: [c[0], c[1], 0.0], first_edge: i as i32 })
            .collect(),
    };
    MapData { collision, world_bounds: [-h, h, -h, h, -5.0, 5.0] }
}
