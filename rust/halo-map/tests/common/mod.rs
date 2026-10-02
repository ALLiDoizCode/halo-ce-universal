//! Shared by the tests that use the developer's own map files.

#![allow(dead_code)]

use std::path::PathBuf;

use halo_map::collision::{CollisionBsp, SURFACE_TWO_SIDED};

/// The 13 Xbox multiplayer maps.
pub const MAPS: [&str; 13] = [
    "beavercreek",
    "bloodgulch",
    "boardingaction",
    "carousel",
    "chillout",
    "damnation",
    "hangemhigh",
    "longest",
    "prisoner",
    "putput",
    "ratrace",
    "sidewinder",
    "wizard",
];

/// The folder named by `HALO_MAP_DIR`, or `None` (after saying so) when the
/// variable is unset, in which case the calling test should return.
pub fn map_dir() -> Option<PathBuf> {
    match std::env::var_os("HALO_MAP_DIR") {
        Some(dir) => Some(PathBuf::from(dir)),
        None => {
            eprintln!("HALO_MAP_DIR is not set: skipping, this test needs the game's own map files");
            None
        }
    }
}

pub fn map_path(dir: &std::path::Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.map"))
}

/// A reference ray test that does not use the BSP trees: the highest
/// upward-facing (or two-sided) surface polygon that the vertical line through
/// `point` crosses within `length` below it. Returns its height.
pub fn brute_force_ground(bsp: &CollisionBsp, point: [f32; 3], length: f32) -> Option<f32> {
    let (x, y, top) = (point[0] as f64, point[1] as f64, point[2] as f64);
    let mut best: Option<f64> = None;
    for i in 0..bsp.surfaces.len() {
        let plane = bsp.surface_plane(i).unwrap();
        let (n, d) = (plane.n, plane.d);
        let two_sided = bsp.surfaces[i].flags & SURFACE_TWO_SIDED != 0;
        if n[2].abs() < 1e-6 || (n[2] < 0.0 && !two_sided) {
            continue;
        }
        let z = (d as f64 - n[0] as f64 * x - n[1] as f64 * y) / n[2] as f64;
        if z > top || z < top - length as f64 || best.is_some_and(|b| b >= z) {
            continue;
        }
        let Some(verts) = bsp.surface_polygon(i, 8) else { continue };
        // convex polygon: the point is inside when the edge cross products share a sign
        let (mut pos, mut neg) = (false, false);
        for k in 0..verts.len() {
            let a = verts[k];
            let b = verts[(k + 1) % verts.len()];
            let c = (b[0] as f64 - a[0] as f64) * (y - a[1] as f64) - (b[1] as f64 - a[1] as f64) * (x - a[0] as f64);
            if c > 1e-9 {
                pos = true;
            } else if c < -1e-9 {
                neg = true;
            }
        }
        if !(pos && neg) {
            best = Some(z);
        }
    }
    best.map(|z| z as f32)
}
