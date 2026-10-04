//! A compact byte form of a [`CollisionBsp`], so that a server can keep a
//! map's collision data in one database row and rebuild it later without the
//! map file. Little-endian, floats as their IEEE-754 bits:
//!
//! ```text
//! "HCB1"                       4 bytes
//! counts of the eight blocks   8 x i32   (nodes, planes, leaves, references,
//!                                         2d nodes, surfaces, edges, vertices)
//! the eight blocks in that order, each element field by field
//! ```
//!
//! Decoding checks the lengths and every index, so bytes that decode can be
//! used by the ray tests without further checks.

use crate::collision::{Bsp2dNode, Bsp2dReference, Bsp3dNode, CollisionBsp, Edge, Leaf, Plane3d, Surface, Vertex};
use crate::error::{malformed, MapError, Result};

const MAGIC: &[u8; 4] = b"HCB1";
const HEADER: usize = 4 + 8 * 4;
/// Bytes per element of each block, in block order.
const ELEMENT_SIZES: [usize; 8] = [12, 16, 8, 8, 20, 12, 24, 16];
/// `Surface::material` is an `i16`, so this admits every value it can hold.
const ANY_MATERIAL: usize = 1 << 15;

struct Writer(Vec<u8>);

impl Writer {
    fn i32(&mut self, v: i32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn i16(&mut self, v: i16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn f32(&mut self, v: f32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
}

struct Cursor<'a>(&'a [u8]);

impl Cursor<'_> {
    fn take<const N: usize>(&mut self) -> [u8; N] {
        // the caller has checked the whole length against the counts
        let (head, rest) = self.0.split_at(N);
        self.0 = rest;
        head.try_into().unwrap()
    }
    fn i32(&mut self) -> i32 {
        i32::from_le_bytes(self.take())
    }
    fn i16(&mut self) -> i16 {
        i16::from_le_bytes(self.take())
    }
    fn u16(&mut self) -> u16 {
        u16::from_le_bytes(self.take())
    }
    fn u8(&mut self) -> u8 {
        self.take::<1>()[0]
    }
    fn f32(&mut self) -> f32 {
        f32::from_le_bytes(self.take())
    }
    fn f32s<const N: usize>(&mut self) -> [f32; N] {
        let mut out = [0.0; N];
        for v in &mut out {
            *v = self.f32();
        }
        out
    }
    fn i32s<const N: usize>(&mut self) -> [i32; N] {
        let mut out = [0; N];
        for v in &mut out {
            *v = self.i32();
        }
        out
    }
}

impl CollisionBsp {
    /// The byte form described in the [module documentation](self).
    pub fn to_bytes(&self) -> Vec<u8> {
        let counts = self.counts();
        let size = HEADER + counts.iter().zip(ELEMENT_SIZES).map(|(n, s)| n * s).sum::<usize>();
        let mut w = Writer(Vec::with_capacity(size));
        w.0.extend_from_slice(MAGIC);
        for n in counts {
            w.i32(n as i32);
        }
        for n in &self.bsp3d_nodes {
            w.i32(n.plane);
            n.children.iter().for_each(|c| w.i32(*c));
        }
        for p in &self.planes {
            p.n.iter().for_each(|v| w.f32(*v));
            w.f32(p.d);
        }
        for l in &self.leaves {
            w.u16(l.flags);
            w.i16(l.bsp2d_reference_count);
            w.i32(l.first_bsp2d_reference);
        }
        for r in &self.bsp2d_references {
            w.i32(r.plane);
            w.i32(r.root);
        }
        for n in &self.bsp2d_nodes {
            n.n.iter().for_each(|v| w.f32(*v));
            w.f32(n.d);
            n.children.iter().for_each(|c| w.i32(*c));
        }
        for s in &self.surfaces {
            w.i32(s.plane);
            w.i32(s.first_edge);
            w.u8(s.flags);
            w.u8(s.breakable_surface);
            w.i16(s.material);
        }
        for e in &self.edges {
            e.vertices.iter().for_each(|v| w.i32(*v));
            e.edges.iter().for_each(|v| w.i32(*v));
            e.surfaces.iter().for_each(|v| w.i32(*v));
        }
        for v in &self.vertices {
            v.point.iter().for_each(|c| w.f32(*c));
            w.i32(v.first_edge);
        }
        w.0
    }

    /// Rebuild a collision BSP from [`CollisionBsp::to_bytes`]. Fails on a
    /// wrong header, a length that does not match the counts, or any index
    /// out of range.
    pub fn from_bytes(bytes: &[u8]) -> Result<CollisionBsp> {
        if bytes.len() < HEADER || &bytes[..4] != MAGIC {
            return malformed("collision data does not start with the HCB1 header");
        }
        let mut c = Cursor(&bytes[4..HEADER]);
        let mut counts = [0usize; 8];
        let mut expected = HEADER as u64;
        for (count, size) in counts.iter_mut().zip(ELEMENT_SIZES) {
            let n = c.i32();
            if n < 0 {
                return malformed("collision data has a negative block count");
            }
            *count = n as usize;
            expected += n as u64 * size as u64;
        }
        if expected != bytes.len() as u64 {
            return malformed(format!("collision data is {} bytes, its counts say {expected}", bytes.len()));
        }
        let mut c = Cursor(&bytes[HEADER..]);
        let bsp = CollisionBsp {
            bsp3d_nodes: (0..counts[0]).map(|_| Bsp3dNode { plane: c.i32(), children: c.i32s() }).collect(),
            planes: (0..counts[1]).map(|_| Plane3d { n: c.f32s(), d: c.f32() }).collect(),
            leaves: (0..counts[2])
                .map(|_| Leaf { flags: c.u16(), bsp2d_reference_count: c.i16(), first_bsp2d_reference: c.i32() })
                .collect(),
            bsp2d_references: (0..counts[3]).map(|_| Bsp2dReference { plane: c.i32(), root: c.i32() }).collect(),
            bsp2d_nodes: (0..counts[4]).map(|_| Bsp2dNode { n: c.f32s(), d: c.f32(), children: c.i32s() }).collect(),
            surfaces: (0..counts[5])
                .map(|_| Surface {
                    plane: c.i32(),
                    first_edge: c.i32(),
                    flags: c.u8(),
                    breakable_surface: c.u8(),
                    material: c.i16(),
                })
                .collect(),
            edges: (0..counts[6]).map(|_| Edge { vertices: c.i32s(), edges: c.i32s(), surfaces: c.i32s() }).collect(),
            vertices: (0..counts[7]).map(|_| Vertex { point: c.f32s(), first_edge: c.i32() }).collect(),
            bounds: Default::default(),
        };
        if let Some(bad) = bsp.check_indices(ANY_MATERIAL).into_iter().find(|c| c.bad > 0) {
            return Err(MapError::IndexOutOfRange { field: bad.field, count: bad.bad });
        }
        Ok(bsp.with_bounds())
    }

    fn counts(&self) -> [usize; 8] {
        [
            self.bsp3d_nodes.len(),
            self.planes.len(),
            self.leaves.len(),
            self.bsp2d_references.len(),
            self.bsp2d_nodes.len(),
            self.surfaces.len(),
            self.edges.len(),
            self.vertices.len(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny() -> CollisionBsp {
        CollisionBsp {
            bsp3d_nodes: vec![Bsp3dNode { plane: 0, children: [-1, i32::MIN] }],
            planes: vec![Plane3d { n: [0.0, 0.0, 1.0], d: 0.5 }],
            leaves: vec![Leaf { flags: 1, bsp2d_reference_count: 1, first_bsp2d_reference: 0 }],
            bsp2d_references: vec![Bsp2dReference { plane: 0, root: i32::MIN }],
            bsp2d_nodes: vec![],
            surfaces: vec![Surface { plane: 0, first_edge: 0, flags: 2, breakable_surface: 3, material: -1 }],
            edges: vec![
                Edge { vertices: [0, 1], edges: [1, 1], surfaces: [0, -1] },
                Edge { vertices: [1, 0], edges: [0, 0], surfaces: [0, -1] },
            ],
            vertices: vec![
                Vertex { point: [0.0, 0.0, 0.5], first_edge: 0 },
                Vertex { point: [1.0, 0.0, 0.5], first_edge: 1 },
            ],
            bounds: Default::default(),
        }
    }

    #[test]
    fn bytes_round_trip() {
        let bsp = tiny();
        assert_eq!(CollisionBsp::from_bytes(&bsp.to_bytes()).unwrap(), bsp);
    }

    #[test]
    fn empty_round_trips() {
        let bsp = CollisionBsp::default();
        assert_eq!(CollisionBsp::from_bytes(&bsp.to_bytes()).unwrap(), bsp);
    }

    #[test]
    fn damaged_bytes_are_refused() {
        let bytes = tiny().to_bytes();
        assert!(CollisionBsp::from_bytes(&bytes[..bytes.len() - 1]).is_err(), "short");
        assert!(CollisionBsp::from_bytes(&[bytes.clone(), vec![0]].concat()).is_err(), "long");
        assert!(CollisionBsp::from_bytes(b"nope").is_err(), "header");
        let mut bad_index = bytes.clone();
        // the first node's plane index, just past the header
        bad_index[HEADER..HEADER + 4].copy_from_slice(&7i32.to_le_bytes());
        assert!(matches!(CollisionBsp::from_bytes(&bad_index), Err(MapError::IndexOutOfRange { .. })));
        let mut huge = bytes;
        huge[4..8].copy_from_slice(&i32::MAX.to_le_bytes());
        assert!(CollisionBsp::from_bytes(&huge).is_err(), "count larger than the data");
    }
}
