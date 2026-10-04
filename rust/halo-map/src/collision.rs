//! The structure BSP's collision BSP: its eight blocks as the engine stores
//! them, a port of the engine's ray test (`collision_bsp_test_vector` in
//! `source/physics/collision_bsp.c`) and a range check of every index.
//!
//! Index conventions are the engine's:
//!
//! - bsp3d child: `-1` is solid (no leaf); bit 31 set is a leaf index in the
//!   low 31 bits; otherwise a bsp3d node index. The root is node 0.
//! - bsp2d child or reference root: bit 31 set is a surface index in the low
//!   31 bits (`-1` is none); otherwise a bsp2d node index.
//! - plane designator (surfaces, bsp2d references): the low 31 bits index
//!   `planes`; bit 31 set means the negated plane.

const SIGN: i32 = i32::MIN;
const MASK: i32 = i32::MAX;
const NONE: i32 = -1;

/// Ray test flags (collisions.h).
pub const TEST_FRONT_FACING: u32 = 1 << 0;
pub const TEST_BACK_FACING: u32 = 1 << 1;
pub const TEST_IGNORE_TWO_SIDED: u32 = 1 << 2;
pub const TEST_IGNORE_INVISIBLE: u32 = 1 << 3;
pub const TEST_IGNORE_BREAKABLE: u32 = 1 << 4;

/// `Surface::flags` bits.
pub const SURFACE_TWO_SIDED: u8 = 1 << 0;
pub const SURFACE_INVISIBLE: u8 = 1 << 1;
pub const SURFACE_CLIMBABLE: u8 = 1 << 2;
pub const SURFACE_BREAKABLE: u8 = 1 << 3;

const LEAF_CONTAINS_TWO_SIDED: u16 = 1 << 0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bsp3dNode {
    /// Index into `planes`.
    pub plane: i32,
    /// `[back, front]`.
    pub children: [i32; 2],
}

/// A point `p` is in front of the plane when `n . p - d >= 0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plane3d {
    pub n: [f32; 3],
    pub d: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Leaf {
    /// Bit 0: the leaf contains two-sided surfaces.
    pub flags: u16,
    pub bsp2d_reference_count: i16,
    pub first_bsp2d_reference: i32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bsp2dReference {
    /// Plane designator.
    pub plane: i32,
    /// A bsp2d node, or (bit 31) a surface.
    pub root: i32,
}

/// A point `p` is on the right when `n . p - d >= 0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bsp2dNode {
    pub n: [f32; 2],
    pub d: f32,
    /// `[left, right]`.
    pub children: [i32; 2],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Surface {
    /// Plane designator.
    pub plane: i32,
    pub first_edge: i32,
    /// `SURFACE_*` bits.
    pub flags: u8,
    pub breakable_surface: u8,
    /// Index into the structure BSP's collision materials, or -1.
    pub material: i16,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Edge {
    /// `[start, end]`.
    pub vertices: [i32; 2],
    /// `[forward, reverse]`.
    pub edges: [i32; 2],
    /// `[left, right]`; -1 for none.
    pub surfaces: [i32; 2],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vertex {
    pub point: [f32; 3],
    pub first_edge: i32,
}

/// What a downward or general ray found.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RayHit {
    /// Fraction along the ray's vector (0 to 1).
    pub t: f32,
    pub surface_index: i32,
    /// Index into `planes` of the bsp3d plane the ray crossed.
    pub plane_index: i32,
}

/// What a downward ray found.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GroundHit {
    /// World units travelled from the ray's origin.
    pub distance: f32,
    /// Height of the point hit.
    pub z: f32,
    pub surface_index: i32,
}

/// What a sphere found: the indices of the surfaces, edges and vertices it
/// reaches, as `collision_bsp_test_sphere` lists them (each once, at most
/// [`MAX_SPHERE_FEATURES`] of each).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SphereHits {
    pub surfaces: Vec<i32>,
    pub edges: Vec<i32>,
    pub vertices: Vec<i32>,
}

/// The engine's cap on each list of a sphere test.
pub const MAX_SPHERE_FEATURES: usize = 256;

/// A sphere round each surface's vertices (centre and radius), for ruling a surface out of a sphere test
/// without walking its edges ([`CollisionBsp::with_bounds`]). Derived from the blocks, so it takes no
/// part in whether two BSPs are equal; empty (nothing is ruled out) until it is made.
#[derive(Debug, Clone, Default)]
pub struct SurfaceBounds(Vec<[f32; 4]>);

impl PartialEq for SurfaceBounds {
    fn eq(&self, _: &SurfaceBounds) -> bool {
        true
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CollisionBsp {
    pub bsp3d_nodes: Vec<Bsp3dNode>,
    pub planes: Vec<Plane3d>,
    pub leaves: Vec<Leaf>,
    pub bsp2d_references: Vec<Bsp2dReference>,
    pub bsp2d_nodes: Vec<Bsp2dNode>,
    pub surfaces: Vec<Surface>,
    pub edges: Vec<Edge>,
    pub vertices: Vec<Vertex>,
    /// See [`CollisionBsp::with_bounds`].
    pub bounds: SurfaceBounds,
}

/// The result of range-checking one kind of index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexCheck {
    pub field: &'static str,
    /// Indices examined.
    pub checked: usize,
    /// Of those, how many were out of range.
    pub bad: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum Contents {
    Unknown,
    Empty,
    SemiEmpty,
    Solid,
}

/// real_math.c: global_projection3d_mappings[projection][sign]
pub const PROJECTION: [[[usize; 2]; 2]; 3] = [[[2, 1], [1, 2]], [[0, 2], [2, 0]], [[1, 0], [0, 1]]];

pub fn project(p: &[f32; 3], projection: usize, sign: bool) -> [f32; 2] {
    let m = PROJECTION[projection][sign as usize];
    [p[m[0]], p[m[1]]]
}

struct Ctx<'a> {
    bsp: &'a CollisionBsp,
    flags: u32,
    point: [f32; 3],
    vector: [f32; 3],
    result_t: f32,
    last_leaf: i32,
    last_plane: i32,
    last_contents: Contents,
    hit: Option<RayHit>,
}

impl CollisionBsp {
    /// This BSP with a bounding sphere made for each surface, which [`CollisionBsp::test_sphere`] uses
    /// to leave out the work of surfaces that are plainly out of reach. Call it once the blocks are
    /// final (loading a map and [`CollisionBsp::from_bytes`] do): the spheres are not kept up to date
    /// if a block is changed afterwards. A surface whose edges do not close in a loop has none.
    pub fn with_bounds(mut self) -> CollisionBsp {
        let mut bounds = Vec::with_capacity(self.surfaces.len());
        for (surface_index, surface) in self.surfaces.iter().enumerate() {
            let mut points: Vec<[f32; 3]> = Vec::new();
            let mut edge_index = surface.first_edge;
            let mut closed = false;
            for _ in 0..=self.edges.len() {
                let Some(edge) = self.edges.get(edge_index as usize) else { break };
                let reverse = (edge.surfaces[1] == surface_index as i32) as usize;
                let Some(vertex) = self.vertices.get(edge.vertices[reverse] as usize) else { break };
                points.push(vertex.point);
                edge_index = edge.edges[reverse];
                if edge_index == surface.first_edge {
                    closed = true;
                    break;
                }
            }
            bounds.push(if closed { bounding_sphere(&points) } else { [0.0, 0.0, 0.0, f32::INFINITY] });
        }
        self.bounds = SurfaceBounds(bounds);
        self
    }

    /// Test a ray from `point` along `vector` (the full displacement; the hit
    /// is at `point + vector * t`) up to fraction `maximum_t`.
    ///
    /// Indexes the blocks without further checks: the BSP must have passed
    /// [`CollisionBsp::check_indices`], which loading a map does.
    pub fn test_vector(&self, flags: u32, point: [f32; 3], vector: [f32; 3], maximum_t: f32) -> Option<RayHit> {
        if self.bsp3d_nodes.is_empty() {
            return None;
        }
        let mut ctx = Ctx {
            bsp: self,
            flags,
            point,
            vector,
            result_t: maximum_t.max(0.0),
            last_leaf: NONE,
            last_plane: NONE,
            last_contents: Contents::Unknown,
            hit: None,
        };
        test_vector_recursive(&mut ctx, 0, 0.0, maximum_t.clamp(0.0, 1.0));
        ctx.hit
    }

    /// The surfaces, edges and vertices within `radius` of `center`: a port of
    /// `collision_bsp_test_sphere`, which the engine's moving pills ask for
    /// what they might hit. Breakable surfaces count as unbroken.
    ///
    /// Like [`CollisionBsp::test_vector`] it indexes the blocks without
    /// further checks.
    pub fn test_sphere(&self, center: [f32; 3], radius: f32) -> SphereHits {
        let mut hits = SphereHits::default();
        self.test_sphere_into(center, radius, &mut hits);
        hits
    }

    /// [`CollisionBsp::test_sphere`] into `hits`, which is cleared first: for callers that ask a great
    /// many times and keep the lists' memory.
    pub fn test_sphere_into(&self, center: [f32; 3], radius: f32, hits: &mut SphereHits) {
        hits.surfaces.clear();
        hits.edges.clear();
        hits.vertices.clear();
        let mut ctx = SphereCtx {
            bsp: self,
            center,
            radius,
            hits: core::mem::take(hits),
            stack: PlaneStack::new(),
            projection: 0,
            sign: false,
            center2d: [0.0; 2],
            tested: [(0, 0); TESTED],
            tested_count: 0,
        };
        if !self.bsp3d_nodes.is_empty() {
            sphere_recursive(&mut ctx, 0);
        }
        *hits = ctx.hits;
    }

    /// Drop a vertical ray from `point` straight down `length` world units and
    /// return the first front-facing (upward) surface it meets.
    pub fn ray_down(&self, point: [f32; 3], length: f32) -> Option<GroundHit> {
        let hit = self.test_vector(TEST_FRONT_FACING, point, [0.0, 0.0, -length], 1.0)?;
        let distance = hit.t * length;
        Some(GroundHit { distance, z: point[2] - distance, surface_index: hit.surface_index })
    }

    /// The leaf containing `point`, or `None` if the point is in solid.
    pub fn leaf_at_point(&self, point: [f32; 3]) -> Option<usize> {
        if self.bsp3d_nodes.is_empty() {
            return None;
        }
        let mut node_index = 0;
        loop {
            let node = &self.bsp3d_nodes[node_index as usize];
            let plane = &self.planes[node.plane as usize];
            let d = point[0] * plane.n[0] + point[1] * plane.n[1] + point[2] * plane.n[2] - plane.d;
            node_index = node.children[(d >= 0.0) as usize];
            if node_index & SIGN != 0 {
                break;
            }
        }
        (node_index != NONE).then_some((node_index & MASK) as usize)
    }

    /// The vertices of a surface's polygon in edge order, or `None` if its
    /// edge loop does not close within `max` edges or names another surface.
    pub fn surface_polygon(&self, surface_index: usize, max: usize) -> Option<Vec<[f32; 3]>> {
        let surface = self.surfaces.get(surface_index)?;
        let mut out = Vec::new();
        let mut edge_index = surface.first_edge;
        loop {
            let edge = self.edges.get(edge_index as usize)?;
            let reverse = (edge.surfaces[1] == surface_index as i32) as usize;
            if edge.surfaces[reverse] != surface_index as i32 {
                return None;
            }
            out.push(self.vertices.get(edge.vertices[reverse] as usize)?.point);
            if out.len() > max {
                return None;
            }
            edge_index = edge.edges[reverse];
            if edge_index == surface.first_edge {
                return Some(out);
            }
        }
    }

    /// [`CollisionBsp::surface_polygon`] for a polygon of at most `N` vertices, without allocating: the
    /// vertices and how many there are.
    pub fn surface_polygon_fixed<const N: usize>(&self, surface_index: usize) -> Option<([[f32; 3]; N], usize)> {
        let surface = self.surfaces.get(surface_index)?;
        let mut out = [[0.0; 3]; N];
        let mut count = 0;
        let mut edge_index = surface.first_edge;
        loop {
            let edge = self.edges.get(edge_index as usize)?;
            let reverse = (edge.surfaces[1] == surface_index as i32) as usize;
            if edge.surfaces[reverse] != surface_index as i32 {
                return None;
            }
            let point = self.vertices.get(edge.vertices[reverse] as usize)?.point;
            // (one more than `N` is too many, as for `surface_polygon`)
            *out.get_mut(count)? = point;
            count += 1;
            edge_index = edge.edges[reverse];
            if edge_index == surface.first_edge {
                return Some((out, count));
            }
        }
    }

    /// The plane a surface lies on, with its designator's sign bit applied.
    pub fn surface_plane(&self, surface_index: usize) -> Option<Plane3d> {
        let s = self.surfaces.get(surface_index)?;
        let p = *self.planes.get((s.plane & MASK) as usize)?;
        Some(if s.plane & SIGN != 0 { Plane3d { n: [-p.n[0], -p.n[1], -p.n[2]], d: -p.d } } else { p })
    }

    /// Check that every index in every block is in range of the block it
    /// refers to. `collision_material_count` is the structure BSP's number of
    /// collision materials, which `Surface::material` indexes.
    pub fn check_indices(&self, collision_material_count: usize) -> Vec<IndexCheck> {
        let mut out = Vec::new();
        let mut run = |field: &'static str, it: &mut dyn Iterator<Item = bool>| {
            let (mut checked, mut bad) = (0, 0);
            for ok in it {
                checked += 1;
                bad += !ok as usize;
            }
            out.push(IndexCheck { field, checked, bad });
        };
        let (nn, np, nl, nr, n2, ns, ne, nv) = (
            self.bsp3d_nodes.len(),
            self.planes.len(),
            self.leaves.len(),
            self.bsp2d_references.len(),
            self.bsp2d_nodes.len(),
            self.surfaces.len(),
            self.edges.len(),
            self.vertices.len(),
        );
        let index = |v: i32, n: usize| v >= 0 && (v as usize) < n;
        // -1 is allowed
        let opt = |v: i32, n: usize| v == NONE || index(v, n);
        // a child: -1, or bit 31 and a leaf/surface index, or a node index
        let child = |v: i32, nodes: usize, leaves: usize| {
            v == NONE || if v & SIGN != 0 { index(v & MASK, leaves) } else { index(v, nodes) }
        };

        // the engine indexes planes with the raw node designator, so its sign bit must be clear
        run("bsp3d node plane -> planes", &mut self.bsp3d_nodes.iter().map(|n| index(n.plane, np)));
        run(
            "bsp3d node back child -> nodes or leaves",
            &mut self.bsp3d_nodes.iter().map(|n| child(n.children[0], nn, nl)),
        );
        run(
            "bsp3d node front child -> nodes or leaves",
            &mut self.bsp3d_nodes.iter().map(|n| child(n.children[1], nn, nl)),
        );
        run(
            "leaf bsp2d references -> bsp2d references",
            &mut self.leaves.iter().map(|l| {
                l.bsp2d_reference_count == 0
                    || (l.bsp2d_reference_count > 0
                        && l.first_bsp2d_reference >= 0
                        && l.first_bsp2d_reference as usize + l.bsp2d_reference_count as usize <= nr)
            }),
        );
        run("bsp2d reference plane -> planes", &mut self.bsp2d_references.iter().map(|r| index(r.plane & MASK, np)));
        run(
            "bsp2d reference root -> bsp2d nodes or surfaces",
            &mut self.bsp2d_references.iter().map(|r| child(r.root, n2, ns)),
        );
        run(
            "bsp2d node left child -> bsp2d nodes or surfaces",
            &mut self.bsp2d_nodes.iter().map(|n| child(n.children[0], n2, ns)),
        );
        run(
            "bsp2d node right child -> bsp2d nodes or surfaces",
            &mut self.bsp2d_nodes.iter().map(|n| child(n.children[1], n2, ns)),
        );
        run("surface plane -> planes", &mut self.surfaces.iter().map(|s| index(s.plane & MASK, np)));
        run("surface first edge -> edges", &mut self.surfaces.iter().map(|s| index(s.first_edge, ne)));
        run(
            "surface material -> collision materials",
            &mut self.surfaces.iter().map(|s| opt(s.material as i32, collision_material_count)),
        );
        run("edge start vertex -> vertices", &mut self.edges.iter().map(|e| index(e.vertices[0], nv)));
        run("edge end vertex -> vertices", &mut self.edges.iter().map(|e| index(e.vertices[1], nv)));
        run("edge forward edge -> edges", &mut self.edges.iter().map(|e| index(e.edges[0], ne)));
        run("edge reverse edge -> edges", &mut self.edges.iter().map(|e| index(e.edges[1], ne)));
        run("edge left surface -> surfaces", &mut self.edges.iter().map(|e| opt(e.surfaces[0], ns)));
        run("edge right surface -> surfaces", &mut self.edges.iter().map(|e| opt(e.surfaces[1], ns)));
        run("vertex first edge -> edges", &mut self.vertices.iter().map(|v| index(v.first_edge, ne)));
        out
    }
}

/// The plane designators on the way down to a leaf. A BSP is a few dozen planes deep at most, so
/// they are kept in the context itself, with a heap list for the (never seen) deeper case.
struct PlaneStack {
    inline: [i32; PlaneStack::INLINE],
    len: usize,
    spill: Vec<i32>,
}

impl PlaneStack {
    const INLINE: usize = 64;

    fn new() -> PlaneStack {
        PlaneStack { inline: [0; PlaneStack::INLINE], len: 0, spill: Vec::new() }
    }

    fn push(&mut self, plane: i32) {
        if self.len < PlaneStack::INLINE {
            self.inline[self.len] = plane;
        } else {
            self.spill.push(plane);
        }
        self.len += 1;
    }

    fn pop(&mut self) {
        self.len -= 1;
        if self.len >= PlaneStack::INLINE {
            self.spill.pop();
        }
    }

    fn contains(&self, plane: i32) -> bool {
        self.inline[..self.len.min(PlaneStack::INLINE)].contains(&plane) || self.spill.contains(&plane)
    }
}

/// How far beyond a surface's bounding sphere and the sphere test's radius a surface has to be for its
/// edges not to be looked at. The edge test (`vector_intersects_sphere`) subtracts two products of the
/// size of the square of the distance to the edge's start, so its answer is only right to about
/// `f32::EPSILON * distance^2 / radius` (a hundredth of a unit at 500 units out for the smallest sphere
/// asked for): a tenth is ten times that, and costs next to nothing in the surfaces it fails to rule out.
const FAR_MARGIN: f32 = 0.1;

/// A sphere that holds all of `points`: their mean and the farthest of them from it (a little more).
fn bounding_sphere(points: &[[f32; 3]]) -> [f32; 4] {
    let n = points.len() as f32;
    let mut c = [0.0f32; 3];
    for p in points {
        for i in 0..3 {
            c[i] += p[i] / n;
        }
    }
    let mut farthest = 0.0f32;
    for p in points {
        let d = ((p[0] - c[0]).powi(2) + (p[1] - c[1]).powi(2) + (p[2] - c[2]).powi(2)).sqrt();
        farthest = farthest.max(d);
    }
    [c[0], c[1], c[2], farthest * 1.001 + 0.001]
}

/// How many (surface, reference plane) pairs a sphere test remembers having tested.
const TESTED: usize = 48;

struct SphereCtx<'a> {
    bsp: &'a CollisionBsp,
    center: [f32; 3],
    radius: f32,
    hits: SphereHits,
    /// The plane designators on the way down to the leaf.
    stack: PlaneStack,
    projection: usize,
    sign: bool,
    center2d: [f32; 2],
    /// The surfaces already tested, with the plane of the reference they were reached by: a surface
    /// that is reached again by the same plane (from another leaf it lies in) gives the same answer,
    /// which is in the lists already, so it is not tested twice.
    tested: [(i32, i32); TESTED],
    tested_count: usize,
}

fn add_feature(list: &mut Vec<i32>, index: i32) {
    if !list.contains(&index) && list.len() < MAX_SPHERE_FEATURES {
        list.push(index);
    }
}

/// real_math.c: fast_vector_intersects_sphere
fn vector_intersects_sphere(point: [f32; 3], vector: [f32; 3], center: [f32; 3], radius: f32) -> bool {
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

/// collision_bsp.c: collision_surface_test_sphere
///
/// One walk round the surface's edges does what the engine's three do (the vertices within the
/// radius, the edges the sphere reaches, and, if neither found anything, whether the centre is over
/// the polygon): each list gets its entries in the same order, so the result is the same.
fn surface_test_sphere(ctx: &mut SphereCtx, surface_index: i32) {
    let bsp = ctx.bsp;
    let surface = &bsp.surfaces[surface_index as usize];
    let radius_squared = ctx.radius * ctx.radius;
    let mut hit_feature = false;
    // (the centre is over the polygon while no edge has it on the wrong side)
    let mut over = true;
    // a surface that is out of reach by more than any rounding (a tenth of a unit) has no vertex
    // in the sphere and no edge that meets it: only whether the centre is over it is left to find out
    let far = bsp.bounds.0.get(surface_index as usize).is_some_and(|b| {
        let (dx, dy, dz) = (b[0] - ctx.center[0], b[1] - ctx.center[1], b[2] - ctx.center[2]);
        let reach = b[3] + ctx.radius + FAR_MARGIN;
        dx * dx + dy * dy + dz * dz > reach * reach
    });

    let mut edge_index = surface.first_edge;
    loop {
        let edge = &bsp.edges[edge_index as usize];
        let reverse = (edge.surfaces[1] == surface_index) as usize;
        let vertex_index = edge.vertices[reverse];
        let v0 = bsp.vertices[vertex_index as usize].point;
        let v1 = bsp.vertices[edge.vertices[1 - reverse] as usize].point;
        if !far {
            let (dx, dy, dz) = (v0[0] - ctx.center[0], v0[1] - ctx.center[1], v0[2] - ctx.center[2]);
            if dx * dx + dy * dy + dz * dz <= radius_squared {
                add_feature(&mut ctx.hits.vertices, vertex_index);
                hit_feature = true;
            }
            let vector = [v1[0] - v0[0], v1[1] - v0[1], v1[2] - v0[2]];
            if vector_intersects_sphere(v0, vector, ctx.center, ctx.radius) {
                add_feature(&mut ctx.hits.edges, edge_index);
                hit_feature = true;
            }
        }
        if over {
            let p0 = project(&v0, ctx.projection, ctx.sign);
            let p1 = project(&v1, ctx.projection, ctx.sign);
            let a = [p0[0] - ctx.center2d[0], p0[1] - ctx.center2d[1]];
            let b = [p1[0] - ctx.center2d[0], p1[1] - ctx.center2d[1]];
            // (not "at least 0": a NaN does not put the centre outside, as in the engine's loop, which gives up only on a negative)
            over = (a[0] * b[1] - a[1] * b[0]).partial_cmp(&0.0) != Some(core::cmp::Ordering::Less);
        }
        if far && !over {
            // (nothing in it can add the surface now)
            return;
        }
        edge_index = edge.edges[reverse];
        if edge_index == surface.first_edge {
            break;
        }
    }

    // no vertex or edge is near: the sphere's centre must be over the polygon
    if hit_feature || over {
        add_feature(&mut ctx.hits.surfaces, surface_index);
    }
}

/// collision_bsp.c: bsp2d_test_sphere_recursive
fn bsp2d_sphere_recursive(ctx: &mut SphereCtx, mut child_index: i32, reference_plane: i32) {
    while child_index & SIGN == 0 {
        let node = ctx.bsp.bsp2d_nodes[child_index as usize];
        let distance = (node.n[0] * ctx.center2d[0] + node.n[1] * ctx.center2d[1]) - node.d;
        if distance <= ctx.radius {
            bsp2d_sphere_recursive(ctx, node.children[0], reference_plane);
        }
        if distance < -ctx.radius {
            return;
        }
        child_index = node.children[1];
    }
    // (-1 is no surface)
    if child_index != NONE {
        let surface_index = child_index & MASK;
        let key = (surface_index, reference_plane);
        if ctx.tested[..ctx.tested_count].contains(&key) {
            return;
        }
        if ctx.tested_count < TESTED {
            ctx.tested[ctx.tested_count] = key;
            ctx.tested_count += 1;
        }
        surface_test_sphere(ctx, surface_index);
    }
}

/// collision_bsp.c: bsp3d_test_sphere_recursive
fn sphere_recursive(ctx: &mut SphereCtx, mut node_index: i32) {
    while node_index & SIGN == 0 {
        let node = ctx.bsp.bsp3d_nodes[node_index as usize];
        let plane = ctx.bsp.planes[node.plane as usize];
        let distance = ctx.center[0] * plane.n[0] + ctx.center[1] * plane.n[1] + ctx.center[2] * plane.n[2] - plane.d;
        let reaches_second_child = distance < ctx.radius;
        let child_index = distance > -ctx.radius;

        if child_index && reaches_second_child {
            ctx.stack.push(node.plane | SIGN);
            sphere_recursive(ctx, node.children[0]);
            ctx.stack.pop();
            ctx.stack.push(node.plane & MASK);
            sphere_recursive(ctx, node.children[1]);
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
        if !ctx.stack.contains(reference.plane) {
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
        bsp2d_sphere_recursive(ctx, reference.root, reference.plane);
    }
}

/// bsp2d.c: bsp2d_test_point
fn bsp2d_test_point(bsp: &CollisionBsp, point: [f32; 2], mut node_index: i32) -> i32 {
    while node_index & SIGN == 0 {
        let node = &bsp.bsp2d_nodes[node_index as usize];
        let distance = (node.n[0] * point[0] + node.n[1] * point[1]) - node.d;
        node_index = node.children[(distance >= 0.0) as usize];
    }
    if node_index != NONE {
        node_index & MASK
    } else {
        NONE
    }
}

/// collision_bsp.c: collision_surface_test_point (breakable surfaces are
/// treated as unbroken)
fn surface_test_point(bsp: &CollisionBsp, surface_index: i32, projection: usize, sign: bool, point: [f32; 2]) -> bool {
    let surface = &bsp.surfaces[surface_index as usize];
    let mut edge_index = surface.first_edge;
    loop {
        let edge = &bsp.edges[edge_index as usize];
        let reverse = (edge.surfaces[1] == surface_index) as usize;
        let p0 = project(&bsp.vertices[edge.vertices[reverse] as usize].point, projection, sign);
        let p1 = project(&bsp.vertices[edge.vertices[1 - reverse] as usize].point, projection, sign);
        let v0 = [point[0] - p0[0], point[1] - p0[1]];
        let v1 = [p1[0] - p0[0], p1[1] - p0[1]];
        if v0[0] * v1[1] - v0[1] * v1[0] > 0.0 {
            return false;
        }
        edge_index = edge.edges[reverse];
        if edge_index == surface.first_edge {
            return true;
        }
    }
}

/// collision_bsp.c: collision_leaf_test_vector
fn leaf_test_vector(ctx: &Ctx, leaf_index: i32, plane_index: i32, t: f32, test_surface: bool) -> i32 {
    let bsp = ctx.bsp;
    let leaf = &bsp.leaves[leaf_index as usize];
    let first = leaf.first_bsp2d_reference;
    for reference_index in first..first + leaf.bsp2d_reference_count as i32 {
        let reference = &bsp.bsp2d_references[reference_index as usize];
        if reference.plane & MASK == plane_index {
            let plane = &bsp.planes[plane_index as usize];
            let (ai, aj, ak) = (plane.n[0].abs(), plane.n[1].abs(), plane.n[2].abs());
            let projection = if ak >= aj && ak >= ai {
                2
            } else if aj >= ai {
                1
            } else {
                0
            };
            let sign = (plane.n[projection] > 0.0) != (reference.plane & SIGN != 0);
            let hit_point =
                [ctx.vector[0] * t + ctx.point[0], ctx.vector[1] * t + ctx.point[1], ctx.vector[2] * t + ctx.point[2]];
            let point2d = project(&hit_point, projection, sign);
            let surface_index = bsp2d_test_point(bsp, point2d, reference.root);
            if surface_index == NONE {
                return NONE;
            }
            if !test_surface || surface_test_point(bsp, surface_index, projection, sign, point2d) {
                return surface_index;
            }
        }
    }
    NONE
}

/// collision_bsp.c: collision_bsp_test_vector_recursive
fn test_vector_recursive(ctx: &mut Ctx, node_index: i32, t0: f32, t1: f32) -> bool {
    if node_index & SIGN == 0 {
        let node = ctx.bsp.bsp3d_nodes[node_index as usize];
        let plane = ctx.bsp.planes[node.plane as usize];
        let dot3 = |a: &[f32; 3], b: &[f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        let distance = dot3(&ctx.point, &plane.n) - plane.d;
        let dot = dot3(&ctx.vector, &plane.n);
        let distance0 = dot * t0 + distance;
        let distance1 = dot * t1 + distance;
        let reaches_back = distance0 < 0.0 || distance1 < 0.0;
        let reaches_front = distance0 >= 0.0 || distance1 >= 0.0;

        if reaches_back && reaches_front {
            let front = (dot > 0.0) as usize;
            let t = -(distance / dot);
            if test_vector_recursive(ctx, node.children[1 - front], t0, t) {
                return true;
            }
            if ctx.result_t <= t {
                return false;
            }
            ctx.last_plane = node.plane;
            if test_vector_recursive(ctx, node.children[front], t, t1) {
                return true;
            }
        } else if test_vector_recursive(ctx, node.children[reaches_front as usize], t0, t1) {
            return true;
        }
    } else {
        let mut leaf_index = NONE;
        let mut contents = Contents::Solid;
        let mut test_surface = false;

        if node_index != NONE {
            leaf_index = node_index & MASK;
            contents = if ctx.bsp.leaves[leaf_index as usize].flags & LEAF_CONTAINS_TWO_SIDED != 0 {
                Contents::SemiEmpty
            } else {
                Contents::Empty
            };
        }
        let open = |c: Contents| c == Contents::Empty || c == Contents::SemiEmpty;

        let test_leaf_index =
            if ctx.flags & TEST_FRONT_FACING != 0 && open(ctx.last_contents) && contents == Contents::Solid {
                ctx.last_leaf
            } else if ctx.flags & TEST_BACK_FACING != 0 && ctx.last_contents == Contents::Solid && open(contents) {
                leaf_index
            } else if ctx.flags & TEST_IGNORE_TWO_SIDED == 0
                && ctx.last_contents == Contents::SemiEmpty
                && contents == Contents::SemiEmpty
            {
                test_surface = true;
                if ctx.flags & TEST_FRONT_FACING != 0 {
                    ctx.last_leaf
                } else {
                    leaf_index
                }
            } else {
                NONE
            };

        if test_leaf_index != NONE {
            let surface_index = leaf_test_vector(ctx, test_leaf_index, ctx.last_plane, t0, test_surface);
            if surface_index != NONE {
                let surface = &ctx.bsp.surfaces[surface_index as usize];
                if (surface.flags & SURFACE_INVISIBLE == 0 || ctx.flags & TEST_IGNORE_INVISIBLE == 0)
                    && (surface.flags & SURFACE_BREAKABLE == 0 || ctx.flags & TEST_IGNORE_BREAKABLE == 0)
                {
                    ctx.result_t = t0;
                    ctx.hit = Some(RayHit { t: t0, surface_index, plane_index: ctx.last_plane });
                    return true;
                }
            }
        }
        ctx.last_leaf = leaf_index;
        ctx.last_contents = contents;
    }
    false
}
