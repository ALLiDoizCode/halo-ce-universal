//! Where a walking player can go on a map: the navigation of the load
//! generator's simulated players (`halo-slayer-load`).
//!
//! A [`Nav`] is a grid laid over the map, built once from its collision data:
//! a node for each square of the plan where there is ground to stand on (one
//! for each floor, where there are floors above floors), and a link between
//! two neighbouring nodes where a walking player can pass from one to the
//! other: not through a wall, not up ground steeper than the tags let a
//! player stand on, not over a drop. It is an approximation of what
//! `halo_sim::walk` allows (that is the reason the hunters keep their stuck
//! check), made of the map's own numbers.
//!
//! [`Nav::heading`] finds the way from one point to another over the links
//! (an A* search) and gives the direction to walk in to follow it. Where there
//! is no way, or either end is off the grid, it gives nothing, and the caller
//! walks straight, as it did before there was a grid.
//!
//! Everything is deterministic: the same map and settings give the same grid
//! and the same paths.

use std::collections::{BinaryHeap, HashMap, VecDeque};

use halo_sim::walk::footing;
use halo_sim::MapData;

use halo_map::collision::{SURFACE_CLIMBABLE, TEST_BACK_FACING, TEST_FRONT_FACING};

/// No node: an unused link, or a node with no parent.
const NONE: u32 = u32::MAX;

/// The eight neighbours of a square, the four along the axes first.
const DIRECTIONS: [(i32, i32); 8] = [(1, 0), (0, 1), (-1, 0), (0, -1), (1, 1), (-1, 1), (-1, -1), (1, -1)];

/// How far apart in height two floors in one square must be to be two nodes.
const FLOOR_GAP: f32 = 0.6;

/// How far (world units) above the ground a player's origin may be, and still count as
/// standing on the node's floor.
const LOCATE_HEIGHT: f32 = 1.5;

/// How far from the line a straight walk between two places the ground of a square may be
/// (world units): more is a drop or a wall to walk off or into, not ground the line crosses.
const LINE_HEIGHT: f32 = 0.4;

/// What stepping onto a node costs more for each of its eight neighbours that cannot be walked
/// to, in steps.
const EDGE_COST: f32 = 0.4;

/// How far ahead along the path (world units) the direction is taken from.
const LOOKAHEAD: f32 = 4.0;

/// The most nodes a search looks at before giving up (a way round that is that long is
/// not one a hunter is sent on: they walk straight, as before).
const MAX_EXPANSIONS: usize = 20_000;

/// How deep (world units) a node's pill may be in a wall, and still be a place to stand: the
/// width of what a walker is let squeeze through, such as the ramps out of Blood Gulch's bases,
/// which are hardly wider than a player.
const CLEARANCE: f32 = 0.05;

/// How many floors one spot of the plan is looked at for (a way in under a roof is a second).
const MAX_FLOORS: usize = 4;

/// The most nodes a grid may have: a guard against a cell size too small for the map.
const MAX_NODES: usize = 4_000_000;

/// The width of the squares of a grid for the walkers of the load generator, world units: narrow
/// enough for the ramps out of the bases of Blood Gulch, which are hardly wider than a player.
pub const DEFAULT_CELL: f32 = 0.5;

/// What the grid is built from: the ground and the walls of a map, or, in a test, a picture.
pub trait Terrain {
    /// The ground to stand on at `xy` that is within a step of `near` (the height of the
    /// place being walked from), if there is any.
    fn ground(&self, xy: [f32; 2], near: f32) -> Option<Ground>;

    /// Whether a walking player can go from `a` to `b`, nodes in neighbouring squares.
    fn passable(&self, a: Spot, b: Spot) -> bool;
}

/// Ground to stand on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ground {
    /// Its height.
    pub z: f32,
    /// A surface the map flags climbable: how a map makes stairs, which a player walks up
    /// however steep they are.
    pub climbable: bool,
}

/// A place on the grid, as [`Terrain::passable`] is asked about it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spot {
    pub at: [f32; 3],
    pub climbable: bool,
}

/// A map's ground and walls, as `halo_sim`'s walking meets them.
pub struct MapTerrain<'a> {
    map: &'a MapData,
    /// The greatest rise of ground for each unit of the plan that a player can walk up (or down).
    slope: f32,
}

impl<'a> MapTerrain<'a> {
    pub fn new(map: &'a MapData) -> MapTerrain<'a> {
        let k = map.movement.minimum_normal_k.clamp(0.05, 1.0);
        MapTerrain { map, slope: (1.0 - k * k).sqrt() / k }
    }
}

impl Terrain for MapTerrain<'_> {
    fn ground(&self, xy: [f32; 2], near: f32) -> Option<Ground> {
        let m = &self.map.movement;
        // (within a cell's rise of the place walked from, and a little more for the cell's size)
        let reach = self.slope * 2.0 + 0.3;
        let floor = near - reach;
        let mut from = near + reach;
        let mut best: Option<Ground> = None;
        // every surface down the column that could be stood on, the top one first: a roof is
        // above the way in under it
        for _ in 0..MAX_FLOORS {
            let Some(hit) = self.map.collision.ray_down([xy[0], xy[1], from], from - floor) else { break };
            from = hit.z - 0.05;
            let Some(plane) = self.map.collision.surface_plane(hit.surface_index as usize) else { continue };
            // (ground a player stands on: not a surface steeper than the tags' limit, unless it
            // is flagged climbable)
            let climbable = self.map.collision.surfaces[hit.surface_index as usize].flags & SURFACE_CLIMBABLE != 0;
            if plane.n[2] < m.minimum_normal_k && !climbable {
                continue;
            }
            // the unit's origin is a hair above the ground, as the walkers are placed (and on a
            // slope, high enough for the pill's round base to rest on it and not be in it)
            let z = hit.z + 0.01 + m.collision_radius * (1.0 / plane.n[2].clamp(0.3, 1.0) - 1.0);
            let stand = footing(self.map, [xy[0], xy[1], z]);
            if stand.penetration <= CLEARANCE
                && stand.standing
                && best.is_none_or(|b| (z - near).abs() < (b.z - near).abs())
            {
                best = Some(Ground { z, climbable });
            }
        }
        best
    }

    fn passable(&self, a: Spot, b: Spot) -> bool {
        let (a_climbs, b_climbs) = (a.climbable, b.climbable);
        let (a, b) = (a.at, b.at);
        let run = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
        if (b[2] - a[2]).abs() > run * self.slope + 0.05 && !(a_climbs || b_climbs) {
            return false;
        }
        let m = &self.map.movement;
        // (a walker's knees and its head, against anything solid, whichever way it faces)
        [m.collision_radius + 0.05, (m.collision_height_standing - 0.1).max(m.collision_radius + 0.1)].iter().all(
            |up| {
                let from = [a[0], a[1], a[2] + up];
                let along = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
                self.map.collision.test_vector(TEST_FRONT_FACING | TEST_BACK_FACING, from, along, 1.0).is_none()
            },
        )
    }
}

/// The walkable squares of a map and the passages between them.
pub struct Nav {
    cell: f32,
    /// Where each node stands: the centre of its square, on the ground.
    nodes: Vec<[f32; 3]>,
    /// Per node, whether the ground is climbable (stairs).
    stairs: Vec<bool>,
    /// Per node, the neighbour in each of the [`DIRECTIONS`], or [`NONE`].
    links: Vec<[u32; 8]>,
    /// The nodes of each square of the plan (more than one where floors are above floors).
    squares: HashMap<(i32, i32), Vec<u32>>,
    /// Per node, which of the separate stretches of ground it is in (nodes with the same
    /// number can be walked between; nodes with different ones cannot).
    region: Vec<u32>,
}

/// What a search needs, kept between searches so that none allocates.
#[derive(Default)]
pub struct Scratch {
    stamp: Vec<u32>,
    cost: Vec<f32>,
    parent: Vec<u32>,
    round: u32,
    open: BinaryHeap<Open>,
}

#[derive(PartialEq)]
struct Open {
    estimate: f32,
    node: u32,
}

impl Eq for Open {}

impl Ord for Open {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // (a min-heap by estimate; the nearer node number first on a tie, for repeatability)
        other.estimate.total_cmp(&self.estimate).then(other.node.cmp(&self.node))
    }
}

impl PartialOrd for Open {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Nav {
    /// The grid of a map's ground, squares `cell` world units wide, joined up from its player
    /// starting locations (what cannot be walked to from one is not on the grid).
    pub fn from_map(map: &MapData, cell: f32) -> Nav {
        let seeds: Vec<[f32; 3]> = map.starts.iter().map(|s| s.position).collect();
        Nav::build(&MapTerrain::new(map), cell, &seeds)
    }

    /// The grid of `terrain`, squares `cell` world units wide, of every square that can be
    /// walked to from the `seeds` (points on or near the ground).
    pub fn build(terrain: &impl Terrain, cell: f32, seeds: &[[f32; 3]]) -> Nav {
        let mut nav = Nav {
            cell: cell.max(0.05),
            nodes: Vec::new(),
            stairs: Vec::new(),
            links: Vec::new(),
            squares: HashMap::new(),
            region: Vec::new(),
        };
        let mut queue = VecDeque::new();
        for seed in seeds {
            let key = nav.square_of(*seed);
            let centre = nav.centre_of(key);
            if let Some(ground) = terrain.ground(centre, seed[2]) {
                if let Some((node, true)) = nav.node_at(key, ground) {
                    queue.push_back(node);
                }
            }
        }
        while let Some(from) = queue.pop_front() {
            if nav.nodes.len() > MAX_NODES {
                break;
            }
            let a = Spot { at: nav.nodes[from as usize], climbable: nav.stairs[from as usize] };
            let key = nav.square_of(a.at);
            // which of the steps from here lead somewhere
            let mut open = [false; 8];
            for (d, (dx, dy)) in DIRECTIONS.iter().enumerate() {
                let to = (key.0 + dx, key.1 + dy);
                let centre = nav.centre_of(to);
                let Some(ground) = terrain.ground(centre, a.at[2]) else { continue };
                let b = Spot { at: [centre[0], centre[1], ground.z], climbable: ground.climbable };
                if !terrain.passable(a, b) {
                    continue;
                }
                open[d] = true;
                let Some((node, fresh)) = nav.node_at(to, ground) else { continue };
                if fresh {
                    queue.push_back(node);
                }
                nav.links[from as usize][d] = node;
            }
            // (no cutting a corner: a diagonal needs both of the steps along the axes it is
            // made of, so a wall of squares touching at their corners has no gap)
            for d in 4..8 {
                let (first, second) = match d {
                    4 => (0, 1),
                    5 => (2, 1),
                    6 => (2, 3),
                    _ => (0, 3),
                };
                if !open[first] || !open[second] {
                    nav.links[from as usize][d] = NONE;
                }
            }
        }
        nav.join_up();
        nav
    }

    /// How many nodes there are.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    fn square_of(&self, p: [f32; 3]) -> (i32, i32) {
        ((p[0] / self.cell).floor() as i32, (p[1] / self.cell).floor() as i32)
    }

    fn centre_of(&self, key: (i32, i32)) -> [f32; 2] {
        [(key.0 as f32 + 0.5) * self.cell, (key.1 as f32 + 0.5) * self.cell]
    }

    /// The node of the square at about height `z`: made if it is not there yet (`true`).
    fn node_at(&mut self, key: (i32, i32), ground: Ground) -> Option<(u32, bool)> {
        let z = ground.z;
        let centre = self.centre_of(key);
        let floors = self.squares.entry(key).or_default();
        if let Some(&node) = floors.iter().find(|&&n| (self.nodes[n as usize][2] - z).abs() < FLOOR_GAP) {
            return Some((node, false));
        }
        let node = self.nodes.len() as u32;
        floors.push(node);
        self.nodes.push([centre[0], centre[1], z]);
        self.stairs.push(ground.climbable);
        self.links.push([NONE; 8]);
        Some((node, true))
    }

    /// Number the separate stretches of ground, following the links both ways.
    fn join_up(&mut self) {
        // (links are one way where a drop is: a stretch is what is joined either way round)
        let mut back: Vec<Vec<u32>> = vec![Vec::new(); self.nodes.len()];
        for (from, links) in self.links.iter().enumerate() {
            for &to in links.iter().filter(|&&to| to != NONE) {
                back[to as usize].push(from as u32);
            }
        }
        self.region = vec![NONE; self.nodes.len()];
        let mut next = 0;
        for start in 0..self.nodes.len() {
            if self.region[start] != NONE {
                continue;
            }
            self.region[start] = next;
            let mut stack = vec![start as u32];
            while let Some(node) = stack.pop() {
                let forward = self.links[node as usize].iter().copied().filter(|&n| n != NONE);
                for neighbour in forward.chain(back[node as usize].iter().copied()) {
                    if self.region[neighbour as usize] == NONE {
                        self.region[neighbour as usize] = next;
                        stack.push(neighbour);
                    }
                }
            }
            next += 1;
        }
    }

    /// The node a player at `p` stands on, if they stand on the grid: the nearest of those in
    /// their square and the ones around it, on about their floor.
    pub fn locate(&self, p: [f32; 3]) -> Option<u32> {
        let (cx, cy) = self.square_of(p);
        let mut best: Option<(f32, u32)> = None;
        for dx in -1..=1 {
            for dy in -1..=1 {
                for &node in self.squares.get(&(cx + dx, cy + dy)).into_iter().flatten() {
                    let n = self.nodes[node as usize];
                    if (p[2] - n[2]).abs() > LOCATE_HEIGHT {
                        continue;
                    }
                    let d2 = (p[0] - n[0]).powi(2) + (p[1] - n[1]).powi(2) + (p[2] - n[2]).powi(2);
                    if best.is_none_or(|(b, _)| d2 < b) {
                        best = Some((d2, node));
                    }
                }
            }
        }
        best.map(|(_, node)| node)
    }

    /// Whether a way over the grid from `from` to `to` may exist: both are on it, and in one
    /// stretch of ground.
    pub fn connected(&self, from: [f32; 3], to: [f32; 3]) -> bool {
        match (self.locate(from), self.locate(to)) {
            (Some(a), Some(b)) => self.region[a as usize] == self.region[b as usize],
            _ => false,
        }
    }

    /// The nodes to walk through, from `from` to `to`, the nearest ones to the two ends first and
    /// last; `None` when either is off the grid, there is no way, or it is too far round to look for.
    pub fn route(&self, scratch: &mut Scratch, from: [f32; 3], to: [f32; 3]) -> Option<Vec<u32>> {
        let (start, goal) = (self.locate(from)?, self.locate(to)?);
        if self.region[start as usize] != self.region[goal as usize] {
            return None;
        }
        if start == goal {
            return Some(vec![start]);
        }
        scratch.begin(self.nodes.len());
        let target = self.nodes[goal as usize];
        let estimate = |node: u32| {
            let n = self.nodes[node as usize];
            let (dx, dy) = ((n[0] - target[0]).abs() / self.cell, (n[1] - target[1]).abs() / self.cell);
            // (the cost of the steps along an axis is 1, across 2^0.5)
            dx.max(dy) + (core::f32::consts::SQRT_2 - 1.0) * dx.min(dy)
        };
        scratch.reach(start, 0.0, NONE);
        scratch.open.push(Open { estimate: estimate(start), node: start });
        let mut expanded = 0;
        while let Some(Open { node, estimate: guess }) = scratch.open.pop() {
            if node == goal {
                let mut path = vec![goal];
                let mut at = goal;
                while scratch.parent[at as usize] != NONE {
                    at = scratch.parent[at as usize];
                    path.push(at);
                }
                path.reverse();
                return Some(path);
            }
            let cost = scratch.cost[node as usize];
            // (a stale entry: the node was reached more cheaply since)
            if guess > cost + estimate(node) + 1e-4 {
                continue;
            }
            expanded += 1;
            if expanded > MAX_EXPANSIONS {
                return None;
            }
            for (d, &next) in self.links[node as usize].iter().enumerate() {
                if next == NONE {
                    continue;
                }
                let step = if d < 4 { 1.0 } else { core::f32::consts::SQRT_2 };
                // (dearer to step onto ground with walls or drops beside it, so that a path keeps off
                // an edge a walker would drift over where there is room)
                let missing = self.links[next as usize].iter().filter(|&&l| l == NONE).count() as f32;
                let through = cost + step + EDGE_COST * missing;
                if !scratch.has(next) || through < scratch.cost[next as usize] {
                    scratch.reach(next, through, node);
                    scratch.open.push(Open { estimate: through + estimate(next), node: next });
                }
            }
        }
        None
    }

    /// The direction to walk in, to get from `from` to `to` over the grid: radians in the
    /// map's plane from the x axis towards the y axis, as the walkers' headings are. `None`
    /// where [`Nav::route`] has no way.
    pub fn heading(&self, scratch: &mut Scratch, from: [f32; 3], to: [f32; 3]) -> Option<f32> {
        let path = self.route(scratch, from, to)?;
        let look = (LOOKAHEAD / self.cell).ceil().max(1.0) as usize;
        // the farthest point of the first stretch of the path that can be walked straight to
        // (so that a corner is not cut, and a bend is not walked wide)
        let last = look.min(path.len() - 1);
        let aim = (1..=last)
            .rev()
            .find(|&i| i == 1 || self.clear_line(from, self.nodes[path[i] as usize]))
            .map(|i| {
                // (once the end of the path is near, it is the place itself that is walked to)
                if i + 1 == path.len() {
                    [to[0], to[1]]
                } else {
                    let n = self.nodes[path[i] as usize];
                    [n[0], n[1]]
                }
            })
            .unwrap_or([to[0], to[1]]);
        Some((aim[1] - from[1]).atan2(aim[0] - from[0]))
    }

    /// Whether the straight line between two places crosses only squares that have ground on
    /// about the height of the line.
    fn clear_line(&self, from: [f32; 3], to: [f32; 3]) -> bool {
        let length = ((to[0] - from[0]).powi(2) + (to[1] - from[1]).powi(2)).sqrt();
        let samples = (length / (self.cell * 0.25)).ceil().max(1.0) as usize;
        (1..samples).all(|i| {
            let t = i as f32 / samples as f32;
            let p = [from[0] + (to[0] - from[0]) * t, from[1] + (to[1] - from[1]) * t, from[2] + (to[2] - from[2]) * t];
            self.squares
                .get(&self.square_of(p))
                .into_iter()
                .flatten()
                .any(|&n| (self.nodes[n as usize][2] - p[2]).abs() <= LINE_HEIGHT)
        })
    }
}

impl Scratch {
    fn begin(&mut self, nodes: usize) {
        if self.stamp.len() != nodes {
            self.stamp = vec![0; nodes];
            self.cost = vec![0.0; nodes];
            self.parent = vec![NONE; nodes];
            self.round = 0;
        }
        self.round += 1;
        self.open.clear();
    }

    fn has(&self, node: u32) -> bool {
        self.stamp[node as usize] == self.round
    }

    fn reach(&mut self, node: u32, cost: f32, parent: u32) {
        self.stamp[node as usize] = self.round;
        self.cost[node as usize] = cost;
        self.parent[node as usize] = parent;
    }
}

/// A flat map drawn in characters, for tests (of this and of the gunners that steer by it).
#[cfg(test)]
pub(crate) mod picture {
    use super::*;

    /// A picture of a flat map: `#` is a wall, anything else is floor, one character a square
    /// `1` unit wide, the first row at the top (largest y).
    pub(crate) struct Picture(pub Vec<Vec<char>>);

    impl Picture {
        pub(crate) fn of(rows: &[&str]) -> Picture {
            Picture(rows.iter().map(|r| r.chars().collect()).collect())
        }

        pub(crate) fn at(&self, x: i32, y: i32) -> char {
            let row = self.0.len() as i32 - 1 - y;
            if row < 0 || x < 0 {
                return '#';
            }
            self.0.get(row as usize).and_then(|r| r.get(x as usize)).copied().unwrap_or('#')
        }

        /// The middle of the character `c`.
        pub(crate) fn find(&self, c: char) -> [f32; 3] {
            for y in 0..self.0.len() as i32 {
                for x in 0..self.0[0].len() as i32 {
                    if self.at(x, y) == c {
                        return [x as f32 + 0.5, y as f32 + 0.5, 0.0];
                    }
                }
            }
            panic!("no {c}")
        }
    }

    impl Terrain for Picture {
        fn ground(&self, xy: [f32; 2], _near: f32) -> Option<Ground> {
            (self.at(xy[0].floor() as i32, xy[1].floor() as i32) != '#').then_some(Ground { z: 0.0, climbable: false })
        }

        fn passable(&self, _a: Spot, _b: Spot) -> bool {
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::picture::Picture;
    use super::*;

    fn nav_of(rows: &[&str]) -> (Nav, Picture) {
        let picture = Picture::of(rows);
        let seed = picture.find('A');
        (Nav::build(&picture, 1.0, &[seed]), picture)
    }

    /// Follow headings from `from` to within a unit of `to`, a step of a unit at a time, as
    /// a hunter does (a new heading each time), and count the steps; `None` if it never arrives
    /// or walks into a wall.
    fn walk_there(nav: &Nav, picture: &Picture, from: [f32; 3], to: [f32; 3]) -> Option<usize> {
        let mut scratch = Scratch::default();
        let mut at = from;
        for step in 0..800 {
            if (at[0] - to[0]).hypot(at[1] - to[1]) < 1.0 {
                return Some(step);
            }
            let heading = nav.heading(&mut scratch, at, to)?;
            at = [at[0] + heading.cos() * 0.5, at[1] + heading.sin() * 0.5, 0.0];
            picture.ground([at[0], at[1]], 0.0)?;
        }
        None
    }

    #[test]
    fn a_path_goes_round_a_wall() {
        let (nav, picture) =
            nav_of(&["..........", "....#.....", "....#.....", "A...#....B", "....#.....", "....#....."]);
        let (a, b) = (picture.find('A'), picture.find('B'));
        let mut scratch = Scratch::default();
        // straight ahead is into the wall; the way is up and over its end
        let heading = nav.heading(&mut scratch, a, b).expect("there is a way round");
        assert!(heading.sin() > 0.3, "turns towards the opening at the top: {heading}");
        let steps = walk_there(&nav, &picture, a, b).expect("arrives round the wall");
        assert!(steps < 60, "and not by a long way: {steps} steps");
    }

    #[test]
    fn nothing_is_walked_through_a_wall_and_the_gaps_in_a_diagonal_wall_are_shut() {
        // a wall of squares touching at their corners: nobody squeezes between them
        let picture = Picture::of(&["...#.", "..#..", ".#...", "#...B", "A...."]);
        let (a, b, corner) = (picture.find('A'), picture.find('B'), [0.5, 4.5, 0.0]);
        let nav = Nav::build(&picture, 1.0, &[a, corner]);
        assert!(nav.connected(a, b), "the bottom right is one stretch of ground");
        assert!(nav.connected(corner, [1.5, 4.5, 0.0]), "so is the corner above the diagonal");
        assert!(!nav.connected(a, corner), "and the two are not joined through the diagonal wall");
    }

    #[test]
    fn with_no_way_there_is_no_heading() {
        let (nav, picture) = nav_of(&["A..#..B", "...#...", "...#..."]);
        let mut scratch = Scratch::default();
        assert!(!nav.connected(picture.find('A'), picture.find('B')));
        assert_eq!(nav.heading(&mut scratch, picture.find('A'), picture.find('B')), None);
    }

    #[test]
    fn off_the_grid_there_is_no_heading() {
        let (nav, picture) = nav_of(&["A....B"]);
        let mut scratch = Scratch::default();
        assert_eq!(nav.heading(&mut scratch, [20.0, 20.0, 0.0], picture.find('B')), None);
        assert_eq!(nav.heading(&mut scratch, picture.find('A'), [20.0, 20.0, 0.0]), None);
        // a player high above the floor is not on it either
        assert_eq!(nav.heading(&mut scratch, [1.5, 0.5, 30.0], picture.find('B')), None);
    }

    #[test]
    fn in_the_open_the_heading_is_straight_at_the_target() {
        let (nav, picture) = nav_of(&["......", "A....B", "......"]);
        let mut scratch = Scratch::default();
        let heading = nav.heading(&mut scratch, picture.find('A'), picture.find('B')).unwrap();
        assert!(heading.abs() < 1e-5, "{heading}");
        // and next to it, at the target itself
        let near = nav.heading(&mut scratch, [4.0, 1.5, 0.0], picture.find('B')).unwrap();
        assert!((near - 0.0).abs() < 0.8, "{near}");
    }

    #[test]
    fn a_maze_is_followed_to_its_end() {
        // a corridor a unit wide, back and forth
        let (nav, picture) = nav_of(&["A.....", "#####.", "......", ".#####", "......", "#####B"]);
        let steps = walk_there(&nav, &picture, picture.find('A'), picture.find('B'));
        assert!(steps.is_some(), "the way through the maze is found");
    }

    #[test]
    fn the_paths_are_the_same_every_time() {
        let (nav, picture) = nav_of(&["A.......", ".####...", "...#.##.", ".#...#B."]);
        let (mut one, mut two) = (Scratch::default(), Scratch::default());
        let (a, b) = (picture.find('A'), picture.find('B'));
        assert_eq!(nav.route(&mut one, a, b), nav.route(&mut two, a, b));
        assert_eq!(nav.route(&mut one, a, b), nav.route(&mut one, a, b));
    }
}
