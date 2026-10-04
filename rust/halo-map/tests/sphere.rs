//! `CollisionBsp::test_sphere` against the straightforward port it was optimised from (kept here as
//! the oracle, unchanged from the commit before issue #48): the same lists of surfaces, edges and
//! vertices, in the same order, for spheres all round every vertex of every map.
//!
//! Needs the game's own maps (`HALO_MAP_DIR`); without them the test passes without testing.

mod common;

use halo_map::collision::{project, CollisionBsp, SphereHits, MAX_SPHERE_FEATURES};
use halo_map::HaloMap;

const SIGN: i32 = i32::MIN;
const MASK: i32 = i32::MAX;
const NONE: i32 = -1;

fn reference(bsp: &CollisionBsp, center: [f32; 3], radius: f32) -> SphereHits {
    let mut ctx = RefCtx {
        bsp,
        center,
        radius,
        hits: SphereHits::default(),
        stack: Vec::new(),
        projection: 0,
        sign: false,
        center2d: [0.0; 2],
    };
    if !bsp.bsp3d_nodes.is_empty() {
        ref_sphere_recursive(&mut ctx, 0);
    }
    ctx.hits
}

struct RefCtx<'a> {
    bsp: &'a CollisionBsp,
    center: [f32; 3],
    radius: f32,
    hits: SphereHits,
    /// The plane designators on the way down to the leaf.
    stack: Vec<i32>,
    projection: usize,
    sign: bool,
    center2d: [f32; 2],
}

fn ref_add_feature(list: &mut Vec<i32>, index: i32) {
    if !list.contains(&index) && list.len() < MAX_SPHERE_FEATURES {
        list.push(index);
    }
}

/// real_math.c: fast_ref_vector_intersects_sphere
fn ref_vector_intersects_sphere(point: [f32; 3], vector: [f32; 3], center: [f32; 3], radius: f32) -> bool {
    let p = [point[0] - center[0], point[1] - center[1], point[2] - center[2]];
    let c = (p[0] * p[0]) + (p[1] * p[1]) + (p[2] * p[2]) - (radius * radius);
    if c < 0.0 {
        return true;
    }
    let b = vector[0] * p[0] + vector[1] * p[1] + vector[2] * p[2];
    if b >= 0.0 {
        return false;
    }
    let a = vector[0] * vector[0] + vector[1] * vector[1] + vector[2] * vector[2];
    let disc = b * b - a * c;
    if disc <= 0.0 {
        return false;
    }
    let neg_a_minus_b = -a - b;
    if neg_a_minus_b < 0.0 {
        true
    } else {
        neg_a_minus_b * neg_a_minus_b < disc
    }
}

/// collision_bsp.c: collision_ref_surface_test_sphere
fn ref_surface_test_sphere(ctx: &mut RefCtx, surface_index: i32) {
    let bsp = ctx.bsp;
    let surface = &bsp.surfaces[surface_index as usize];
    let radius_squared = ctx.radius * ctx.radius;
    let mut hit_feature = false;

    let mut edge_index = surface.first_edge;
    loop {
        let edge = &bsp.edges[edge_index as usize];
        let reverse = (edge.surfaces[1] == surface_index) as usize;
        let vertex_index = edge.vertices[reverse];
        let v = bsp.vertices[vertex_index as usize].point;
        let (dx, dy, dz) = (v[0] - ctx.center[0], v[1] - ctx.center[1], v[2] - ctx.center[2]);
        if dx * dx + dy * dy + dz * dz <= radius_squared {
            ref_add_feature(&mut ctx.hits.vertices, vertex_index);
            hit_feature = true;
        }
        edge_index = edge.edges[reverse];
        if edge_index == surface.first_edge {
            break;
        }
    }

    let mut edge_index = surface.first_edge;
    loop {
        let edge = &bsp.edges[edge_index as usize];
        let reverse = (edge.surfaces[1] == surface_index) as usize;
        let v0 = bsp.vertices[edge.vertices[reverse] as usize].point;
        let v1 = bsp.vertices[edge.vertices[1 - reverse] as usize].point;
        let vector = [v1[0] - v0[0], v1[1] - v0[1], v1[2] - v0[2]];
        if ref_vector_intersects_sphere(v0, vector, ctx.center, ctx.radius) {
            ref_add_feature(&mut ctx.hits.edges, edge_index);
            hit_feature = true;
        }
        edge_index = edge.edges[reverse];
        if edge_index == surface.first_edge {
            break;
        }
    }

    if !hit_feature {
        // no vertex or edge is near: the sphere's centre must be over the polygon
        let mut edge_index = surface.first_edge;
        loop {
            let edge = &bsp.edges[edge_index as usize];
            let reverse = (edge.surfaces[1] == surface_index) as usize;
            let p0 = project(&bsp.vertices[edge.vertices[reverse] as usize].point, ctx.projection, ctx.sign);
            let p1 = project(&bsp.vertices[edge.vertices[1 - reverse] as usize].point, ctx.projection, ctx.sign);
            let v0 = [p0[0] - ctx.center2d[0], p0[1] - ctx.center2d[1]];
            let v1 = [p1[0] - ctx.center2d[0], p1[1] - ctx.center2d[1]];
            if v0[0] * v1[1] - v0[1] * v1[0] < 0.0 {
                return;
            }
            edge_index = edge.edges[reverse];
            if edge_index == surface.first_edge {
                break;
            }
        }
    }
    ref_add_feature(&mut ctx.hits.surfaces, surface_index);
}

/// collision_bsp.c: bsp2d_test_ref_sphere_recursive
fn ref_bsp2d_ref_sphere_recursive(ctx: &mut RefCtx, mut child_index: i32) {
    while child_index & SIGN == 0 {
        let node = ctx.bsp.bsp2d_nodes[child_index as usize];
        let distance = (node.n[0] * ctx.center2d[0] + node.n[1] * ctx.center2d[1]) - node.d;
        if distance <= ctx.radius {
            ref_bsp2d_ref_sphere_recursive(ctx, node.children[0]);
        }
        if distance < -ctx.radius {
            return;
        }
        child_index = node.children[1];
    }
    // (-1 is no surface)
    if child_index != NONE {
        ref_surface_test_sphere(ctx, child_index & MASK);
    }
}

/// collision_bsp.c: bsp3d_test_ref_sphere_recursive
fn ref_sphere_recursive(ctx: &mut RefCtx, mut node_index: i32) {
    while node_index & SIGN == 0 {
        let node = ctx.bsp.bsp3d_nodes[node_index as usize];
        let plane = ctx.bsp.planes[node.plane as usize];
        let distance = ctx.center[0] * plane.n[0] + ctx.center[1] * plane.n[1] + ctx.center[2] * plane.n[2] - plane.d;
        let reaches_second_child = distance < ctx.radius;
        let child_index = distance > -ctx.radius;

        if child_index && reaches_second_child {
            ctx.stack.push(node.plane | SIGN);
            ref_sphere_recursive(ctx, node.children[0]);
            ctx.stack.pop();
            ctx.stack.push(node.plane & MASK);
            ref_sphere_recursive(ctx, node.children[1]);
            ctx.stack.pop();
            return;
        }
        node_index = node.children[child_index as usize];
    }

    if node_index == NONE {
        return;
    }
    let leaf = ctx.bsp.leaves[(node_index & MASK) as usize];
    let first = leaf.first_bsp2d_reference;
    for reference_index in first..first + leaf.bsp2d_reference_count as i32 {
        let reference = ctx.bsp.bsp2d_references[reference_index as usize];
        if !ctx.stack.contains(&reference.plane) {
            continue;
        }
        let plane = ctx.bsp.planes[(reference.plane & MASK) as usize];
        let c = ctx.center;
        let plane_distance = -(c[0] * plane.n[0] + c[1] * plane.n[1] + c[2] * plane.n[2] - plane.d);
        let projected = [
            plane.n[0] * plane_distance + c[0],
            plane.n[1] * plane_distance + c[1],
            plane.n[2] * plane_distance + c[2],
        ];
        let (ai, aj, ak) = (plane.n[0].abs(), plane.n[1].abs(), plane.n[2].abs());
        ctx.projection = if ak >= aj && ak >= ai {
            2
        } else if aj >= ai {
            1
        } else {
            0
        };
        ctx.sign = (plane.n[ctx.projection] > 0.0) != (reference.plane & SIGN != 0);
        ctx.center2d = project(&projected, ctx.projection, ctx.sign);
        ref_bsp2d_ref_sphere_recursive(ctx, reference.root);
    }
}

#[test]
fn test_sphere_lists_what_the_straightforward_port_lists_around_every_vertex_of_every_map() {
    let Some(dir) = common::map_dir() else { return };
    let mut compared = 0u64;
    let mut with_surfaces = 0u64;
    for name in common::MAPS {
        let map = HaloMap::from_path(common::map_path(&dir, name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        let bsp = &map.collision;
        // (a debug build is slow: every 8th vertex then)
        let stride = if cfg!(debug_assertions) { 8 } else { 1 };
        for vertex in bsp.vertices.iter().step_by(stride) {
            let p = vertex.point;
            for offset in [[0.0, 0.0, 0.5], [0.3, 0.2, 1.0], [-0.8, 0.5, 0.3], [0.05, -0.6, 0.0], [1.5, 1.5, -0.4]] {
                let center = [p[0] + offset[0], p[1] + offset[1], p[2] + offset[2]];
                // the radii the pill's footing asks with (a standing pill: about 1.2 and with the drop, 1.7), and one more
                for radius in [1.0, 1.3, 1.75, 2.5] {
                    let new = bsp.test_sphere(center, radius);
                    let old = reference(bsp, center, radius);
                    assert_eq!(new, old, "{name}: centre {center:?} radius {radius}");
                    compared += 1;
                    with_surfaces += !old.surfaces.is_empty() as u64;
                }
            }
        }
    }
    eprintln!("{compared} spheres compared, {with_surfaces} reached a surface");
    assert!(compared == 0 || with_surfaces > 0);
}
