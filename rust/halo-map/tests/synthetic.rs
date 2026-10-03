//! Tests of the loader on a tiny map built here byte for byte in the cache
//! file format: no game data needed, so these always run.
//!
//! The map is a square floor at z = 0 (solid below, open above) with one
//! player start, one netgame flag, one netgame equipment and one vehicle, and
//! the globals tag and multiplayer biped tag that hold the players' movement
//! values.

use std::io::Write;

use halo_map::{flag_type, game_type, HaloMap, MapError};

const TAG_BASE: u32 = 0x803A_6000;
const BSP_BASE: u32 = 0x4000_0000;
const HEADER: usize = 0x800;
const TAG_DATA_SIZE: usize = 0x2000;
const BSP_DATA_SIZE: usize = 0x1000;

/// The map's inflated image, header included.
struct Image(Vec<u8>);

impl Image {
    fn new() -> Image {
        Image(vec![0; HEADER + TAG_DATA_SIZE + BSP_DATA_SIZE])
    }
    fn u32(&mut self, at: usize, v: u32) {
        self.0[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }
    fn i32(&mut self, at: usize, v: i32) {
        self.u32(at, v as u32);
    }
    fn i16(&mut self, at: usize, v: i16) {
        self.0[at..at + 2].copy_from_slice(&v.to_le_bytes());
    }
    fn f32(&mut self, at: usize, v: f32) {
        self.u32(at, v.to_bits());
    }
    fn f32s(&mut self, at: usize, vs: &[f32]) {
        for (i, v) in vs.iter().enumerate() {
            self.f32(at + i * 4, *v);
        }
    }
    /// A four-character code as the engine's big-endian group tag, stored little-endian.
    fn code(&mut self, at: usize, c: &[u8; 4]) {
        self.u32(at, u32::from_be_bytes(*c));
    }
    fn cstr(&mut self, at: usize, s: &str) {
        self.0[at..at + s.len()].copy_from_slice(s.as_bytes());
    }
    /// A tag block at `at`: `count` elements at `target`, an offset in tag data.
    fn tag_block(&mut self, at: usize, count: i32, target: usize) {
        self.i32(at, count);
        self.u32(at + 4, TAG_BASE + target as u32);
    }
    /// A tag reference at `at` to tag `index`.
    fn tag_ref(&mut self, at: usize, index: u32) {
        self.u32(at + 0xC, tag_id(index));
    }
}

fn tag_id(index: u32) -> u32 {
    0xE174_0000 | index
}

/// Offsets within the tag data.
const T_INSTANCES: usize = 0x40;
const T_NAMES: usize = 0x200;
const T_SCENARIO: usize = 0x400;
const T_PLAYERS: usize = 0xA00;
const T_FLAGS: usize = 0xA80;
const T_EQUIPMENT: usize = 0xB80;
const T_VEHICLES: usize = 0xC80;
const T_PALETTE: usize = 0xD00;
const T_BSP_REFS: usize = 0xD80;
const T_GLOBALS: usize = 0x1000;
const T_PLAYER_INFORMATION: usize = 0x11C0;
const T_MULTIPLAYER_INFORMATION: usize = 0x12C0;
const T_BIPED: usize = 0x1380;
const T_FALLING_DAMAGE: usize = 0x1900;

/// Offsets within the structure BSP data.
const B_SBSP: usize = 0x40;
const B_COLLISION: usize = 0x300;
const B_NODES: usize = 0x400;
const B_PLANES: usize = 0x420;
const B_LEAVES: usize = 0x440;
const B_REFS: usize = 0x460;
const B_SURFACES: usize = 0x480;
const B_EDGES: usize = 0x4A0;
const B_VERTICES: usize = 0x520;

const SOLID: i32 = -1;
const LEAF_0: i32 = i32::MIN;

fn build() -> Image {
    let mut m = Image::new();

    // cache file header
    m.code(0, b"head");
    m.i32(4, 5);
    m.u32(8, (HEADER + TAG_DATA_SIZE + BSP_DATA_SIZE) as u32);
    m.u32(0x10, HEADER as u32);
    m.u32(0x14, TAG_DATA_SIZE as u32);
    m.cstr(0x20, "synth");
    m.cstr(0x40, "01.10.12.2276");
    m.code(0x7FC, b"foot");

    // tag data: header, then the four tags
    let t = HEADER;
    m.u32(t, TAG_BASE + T_INSTANCES as u32);
    m.u32(t + 4, tag_id(0));
    m.u32(t + 0xC, 6);
    m.code(t + 0x20, b"tags");
    let tags: [(&[u8; 4], &str); 6] = [
        (b"scnr", "levels\\synth\\synth"),
        (b"itmc", "item collections\\pistol"),
        (b"vehi", "vehicles\\warthog\\warthog"),
        (b"sbsp", "levels\\synth\\synth"),
        (b"matg", "globals\\globals"),
        (b"bipd", "characters\\cyborg_mp\\cyborg_mp"),
    ];
    let mut name_at = T_NAMES;
    for (i, (group, name)) in tags.iter().enumerate() {
        let o = t + T_INSTANCES + i * 0x20;
        m.code(o, group);
        m.u32(o + 0xC, tag_id(i as u32));
        m.u32(o + 0x10, TAG_BASE + name_at as u32);
        m.cstr(t + name_at, name);
        name_at += 0x40;
    }
    m.u32(t + T_INSTANCES + 0x14, TAG_BASE + T_SCENARIO as u32);
    m.u32(t + T_INSTANCES + 4 * 0x20 + 0x14, TAG_BASE + T_GLOBALS as u32);
    m.u32(t + T_INSTANCES + 5 * 0x20 + 0x14, TAG_BASE + T_BIPED as u32);

    // scenario
    let s = t + T_SCENARIO;
    m.i16(s + 0x3C, 1);
    m.tag_block(s + 0x354, 1, T_PLAYERS);
    m.tag_block(s + 0x378, 1, T_FLAGS);
    m.tag_block(s + 0x384, 1, T_EQUIPMENT);
    m.tag_block(s + 0x240, 1, T_VEHICLES);
    m.tag_block(s + 0x24C, 1, T_PALETTE);
    m.tag_block(s + 0x5A4, 1, T_BSP_REFS);

    let p = t + T_PLAYERS; // player starting location
    m.f32s(p, &[1.0, 2.0, 0.0]);
    m.f32(p + 0xC, 1.5);
    m.i16(p + 0x10, 1);
    m.i16(p + 0x14, game_type::ALL_NORMAL);

    let f = t + T_FLAGS; // netgame flag
    m.f32s(f, &[-3.0, 4.0, 0.5]);
    m.f32(f + 0xC, -1.0);
    m.i16(f + 0x10, flag_type::CTF_FLAG);
    m.i16(f + 0x12, 0);

    let e = t + T_EQUIPMENT; // netgame equipment
    m.i16(e + 4, game_type::SLAYER);
    m.i16(e + 0xC, 1);
    m.i16(e + 0xE, 30);
    m.f32s(e + 0x40, &[5.0, 5.0, 0.25]);
    m.f32(e + 0x4C, 0.5);
    m.tag_ref(e + 0x50, 1);

    let v = t + T_VEHICLES; // vehicle; palette entry 0 is the warthog
    m.i16(v, 0);
    m.f32s(v + 8, &[-2.0, -2.0, 0.5]);
    m.f32s(v + 0x14, &[0.25, 0.0, 0.0]);
    m.tag_ref(t + T_PALETTE, 2);

    // globals: the multiplayer information names the biped, the player
    // information has the speeds; the biped has the pill and the slopes
    let g = t + T_GLOBALS;
    m.tag_block(g + 0x164, 1, T_MULTIPLAYER_INFORMATION);
    m.tag_block(g + 0x170, 1, T_PLAYER_INFORMATION);
    m.tag_block(g + 0x188, 1, T_FALLING_DAMAGE);
    m.f32s(t + T_FALLING_DAMAGE + 0x8C, &[0.35, 0.125, 0.3125]); // maximum falling, minimum damage, maximum damage
    m.tag_ref(t + T_MULTIPLAYER_INFORMATION + 0x10, 5);
    let pi = t + T_PLAYER_INFORMATION;
    m.f32s(pi + 0x34, &[2.5, 2.0, 1.75, 0.5, 1.0, 0.75, 0.625, 0.25, 0.04]);
    let bd = t + T_BIPED + 0x2F0;
    m.f32(bd + 0x74, 1.5); // downhill velocity scale
    m.f32(bd + 0x80, 0.5); // uphill velocity scale
    m.f32s(bd + 0x134, &[0.75, 0.5, 0.25]); // collision height standing, crouching, radius
    m.f32s(bd + 0x1E0, &[0.7, -0.3, -0.7, 0.3, 0.7]);
    m.f32(bd + 0xC4, 0.0625); // jump velocity
    m.f32s(bd + 0xE4, &[0.25, 0.75, 1.5, 3.0, 9.0]); // landing times, landing velocities
    m.f32(bd + 0x1DC, 0.125); // crouch transition velocity

    let b = HEADER + TAG_DATA_SIZE; // structure bsp data, which the scenario locates
    let r = t + T_BSP_REFS;
    m.u32(r, b as u32);
    m.u32(r + 4, BSP_DATA_SIZE as u32);
    m.u32(r + 8, BSP_BASE);
    m.tag_ref(r + 0x10, 3);

    let bsp_ptr = |off: usize| BSP_BASE + off as u32;
    let bsp_block = |m: &mut Image, at: usize, count: i32, target: usize| {
        m.i32(at, count);
        m.u32(at + 4, bsp_ptr(target));
    };
    m.u32(b, bsp_ptr(B_SBSP));
    m.code(b + 0x14, b"sbsp");
    let sb = b + B_SBSP;
    m.i32(sb + 0xA4, 1); // one collision material
    bsp_block(&mut m, sb + 0xB0, 1, B_COLLISION);
    m.f32s(sb + 0xC8, &[-10.0, 10.0, -10.0, 10.0, -5.0, 5.0]);

    // collision bsp: one bsp3d node on the plane z = 0, solid behind it and
    // leaf 0 in front; the leaf's one reference is the floor surface
    let c = b + B_COLLISION;
    bsp_block(&mut m, c, 1, B_NODES);
    bsp_block(&mut m, c + 0x0C, 1, B_PLANES);
    bsp_block(&mut m, c + 0x18, 1, B_LEAVES);
    bsp_block(&mut m, c + 0x24, 1, B_REFS);
    bsp_block(&mut m, c + 0x30, 0, 0);
    bsp_block(&mut m, c + 0x3C, 1, B_SURFACES);
    bsp_block(&mut m, c + 0x48, 4, B_EDGES);
    bsp_block(&mut m, c + 0x54, 4, B_VERTICES);

    let n = b + B_NODES;
    m.i32(n, 0);
    m.i32(n + 4, SOLID);
    m.i32(n + 8, LEAF_0);
    m.f32s(b + B_PLANES, &[0.0, 0.0, 1.0, 0.0]);
    m.i16(b + B_LEAVES + 2, 1); // one bsp2d reference, the first
    m.i32(b + B_REFS, 0); // plane 0
    m.i32(b + B_REFS + 4, LEAF_0); // root is surface 0
    m.i32(b + B_SURFACES, 0); // plane 0, first edge 0
                              // four edges around the square, each with the surface on its left
    for i in 0..4usize {
        let e = b + B_EDGES + i * 0x18;
        m.i32(e, i as i32);
        m.i32(e + 4, ((i + 1) % 4) as i32);
        m.i32(e + 8, ((i + 1) % 4) as i32);
        m.i32(e + 12, ((i + 3) % 4) as i32);
        m.i32(e + 16, 0);
        m.i32(e + 20, -1);
    }
    let corners = [[-5.0, -5.0], [5.0, -5.0], [5.0, 5.0], [-5.0, 5.0]];
    for (i, c) in corners.iter().enumerate() {
        let o = b + B_VERTICES + i * 0x10;
        m.f32s(o, &[c[0], c[1], 0.0]);
        m.i32(o + 12, i as i32);
    }
    m
}

/// The file as the Xbox stores it: the header, then the rest as one zlib
/// stream, then padding.
fn compress(image: &[u8]) -> Vec<u8> {
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(&image[HEADER..]).unwrap();
    let mut file = image[..HEADER].to_vec();
    file.extend(z.finish().unwrap());
    file.extend([0; 700]);
    file
}

#[test]
fn a_plain_map_loads_its_placements() {
    let map = HaloMap::from_bytes(&build().0).unwrap();

    assert_eq!(map.header.name, "synth");
    assert!(!map.header.compressed);
    assert_eq!(map.scenario_name, "levels\\synth\\synth");
    assert_eq!(map.world_bounds, [-10.0, 10.0, -10.0, 10.0, -5.0, 5.0]);

    assert_eq!(map.player_starts.len(), 1);
    let s = map.player_starts[0];
    assert_eq!((s.position, s.facing, s.team_index), ([1.0, 2.0, 0.0], 1.5, 1));
    assert_eq!(s.game_types, [game_type::ALL_NORMAL, 0, 0, 0]);

    assert_eq!(map.netgame_flags.len(), 1);
    let f = map.netgame_flags[0];
    assert_eq!((f.position, f.facing, f.flag_type, f.team_index), ([-3.0, 4.0, 0.5], -1.0, flag_type::CTF_FLAG, 0));

    assert_eq!(map.netgame_equipment.len(), 1);
    let e = &map.netgame_equipment[0];
    assert_eq!((e.position, e.facing, e.team_index, e.spawn_time), ([5.0, 5.0, 0.25], 0.5, 1, 30));
    assert_eq!(e.game_types[0], game_type::SLAYER);
    assert_eq!(e.tag_name, "item collections\\pistol.itmc");

    assert_eq!(map.vehicles.len(), 1);
    let v = &map.vehicles[0];
    assert_eq!((v.position, v.rotation), ([-2.0, -2.0, 0.5], [0.25, 0.0, 0.0]));
    assert_eq!(v.tag_name, "vehicles\\warthog\\warthog.vehi");
}

#[test]
fn the_players_movement_values_are_read_from_the_globals_and_the_multiplayer_biped() {
    let m = HaloMap::from_bytes(&build().0).unwrap().movement;
    assert_eq!((m.run_forward_speed, m.run_backward_speed, m.run_sideways_speed), (2.5, 2.0, 1.75));
    assert_eq!((m.run_acceleration, m.sneak_forward_speed, m.sneak_backward_speed), (0.5, 1.0, 0.75));
    assert_eq!((m.sneak_sideways_speed, m.sneak_acceleration, m.airborne_acceleration), (0.625, 0.25, 0.04));
    assert_eq!((m.collision_height_standing, m.collision_height_crouching, m.collision_radius), (0.75, 0.5, 0.25));
    assert_eq!((m.downhill_velocity_scale, m.uphill_velocity_scale), (1.5, 0.5));
    assert_eq!(
        (m.minimum_normal_k, m.downhill_k0, m.downhill_k1, m.uphill_k0, m.uphill_k1),
        (0.7, -0.3, -0.7, 0.3, 0.7)
    );
    assert_eq!((m.jump_velocity, m.crouch_transition_velocity), (0.0625, 0.125));
    assert_eq!(
        (
            m.maximum_soft_landing_time,
            m.maximum_hard_landing_time,
            m.minimum_soft_landing_velocity,
            m.minimum_hard_landing_velocity,
            m.maximum_hard_landing_velocity
        ),
        (0.25, 0.75, 1.5, 3.0, 9.0)
    );
    assert_eq!(
        (m.maximum_falling_velocity, m.minimum_damage_velocity, m.maximum_damage_velocity),
        (0.35, 0.125, 0.3125)
    );
}

#[test]
fn a_map_without_the_globals_or_with_a_biped_that_cannot_stand_is_refused() {
    let mut image = build();
    // the globals tag is not one
    image.code(HEADER + T_INSTANCES + 4 * 0x20, b"scnr");
    assert!(matches!(HaloMap::from_bytes(&image.0), Err(MapError::Malformed(_))));

    let mut image = build();
    // a pill with no radius
    image.f32(HEADER + T_BIPED + 0x2F0 + 0x13C, 0.0);
    assert!(matches!(HaloMap::from_bytes(&image.0), Err(MapError::Malformed(_))));
}

#[test]
fn a_compressed_map_loads_the_same_as_a_plain_one() {
    let image = build().0;
    let compressed = HaloMap::from_bytes(&compress(&image)).unwrap();
    assert!(compressed.header.compressed);

    let mut expected = HaloMap::from_bytes(&image).unwrap();
    expected.header.compressed = true;
    assert_eq!(compressed, expected);
}

#[test]
fn a_ray_dropped_onto_the_floor_hits_it() {
    let map = HaloMap::from_bytes(&build().0).unwrap();
    let hit = map.collision.ray_down([1.0, 2.0, 1.0], 3.0).expect("the floor is 1 unit below");
    assert!((hit.distance - 1.0).abs() < 1e-5);
    assert!(hit.z.abs() < 1e-5);
    assert_eq!(hit.surface_index, 0);
}

#[test]
fn a_ray_that_stops_short_of_the_floor_or_starts_under_it_hits_nothing() {
    let map = HaloMap::from_bytes(&build().0).unwrap();
    assert_eq!(map.collision.ray_down([1.0, 2.0, 1.0], 0.5), None, "stops short");
    assert_eq!(map.collision.ray_down([1.0, 2.0, -1.0], 3.0), None, "the floor faces up; this ray starts inside it");
}

#[test]
fn the_floor_is_open_above_and_solid_below() {
    let map = HaloMap::from_bytes(&build().0).unwrap();
    assert_eq!(map.collision.leaf_at_point([0.0, 0.0, 1.0]), Some(0));
    assert_eq!(map.collision.leaf_at_point([0.0, 0.0, -1.0]), None);
}

#[test]
fn a_sphere_finds_the_surfaces_edges_and_vertices_within_its_reach() {
    let map = HaloMap::from_bytes(&build().0).unwrap();
    // over the middle of the floor, close enough to touch it: the surface and nothing else
    let hits = map.collision.test_sphere([0.0, 0.0, 0.3], 0.5);
    assert_eq!((hits.surfaces, hits.edges, hits.vertices), (vec![0], vec![], vec![]));
    // too high to reach it
    let hits = map.collision.test_sphere([0.0, 0.0, 0.6], 0.5);
    assert_eq!((hits.surfaces.len(), hits.edges.len(), hits.vertices.len()), (0, 0, 0));
    // under it, in the solid: nothing to touch
    let hits = map.collision.test_sphere([0.0, 0.0, -3.0], 0.5);
    assert!(hits.surfaces.is_empty() && hits.edges.is_empty());
    // by the corner (5, 5): the surface, the two edges meeting there and that vertex
    let hits = map.collision.test_sphere([4.9, 4.9, 0.1], 0.5);
    assert_eq!(hits.surfaces, vec![0]);
    assert_eq!(hits.vertices, vec![2]);
    let mut edges = hits.edges;
    edges.sort();
    assert_eq!(edges, vec![1, 2]);
    // past the edge of the polygon, over the open air beyond it: its edge is
    // within reach, so the surface is too
    let hits = map.collision.test_sphere([5.3, 0.0, 0.1], 0.5);
    assert_eq!((hits.surfaces, hits.edges), (vec![0], vec![1]));
    // further out, the plane is within reach but the polygon is not
    let hits = map.collision.test_sphere([5.8, 0.0, 0.1], 0.5);
    assert!(hits.surfaces.is_empty() && hits.edges.is_empty());
}

#[test]
fn the_floor_polygon_is_its_four_corners() {
    let map = HaloMap::from_bytes(&build().0).unwrap();
    let poly = map.collision.surface_polygon(0, 8).unwrap();
    assert_eq!(poly, vec![[-5.0, -5.0, 0.0], [5.0, -5.0, 0.0], [5.0, 5.0, 0.0], [-5.0, 5.0, 0.0]]);
}

#[test]
fn an_out_of_range_index_refuses_the_map() {
    let mut image = build();
    // the floor surface's first edge, 99 of 4
    image.i32(HEADER + TAG_DATA_SIZE + B_SURFACES + 4, 99);
    match HaloMap::from_bytes(&image.0) {
        Err(MapError::IndexOutOfRange { field, count: 1 }) => assert_eq!(field, "surface first edge -> edges"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_map_of_another_cache_version_is_refused() {
    let mut image = build();
    image.i32(4, 7);
    assert!(matches!(HaloMap::from_bytes(&image.0), Err(MapError::UnsupportedVersion(7))));
}

#[test]
fn a_file_that_is_not_a_map_is_refused() {
    assert!(matches!(HaloMap::from_bytes(b"hello"), Err(MapError::Malformed(_))));
    assert!(matches!(HaloMap::from_bytes(&vec![0; 0x900]), Err(MapError::Malformed(_))));
}

#[test]
fn a_pointer_outside_tag_data_is_refused() {
    let mut image = build();
    // the scenario's player block, pointing before the tag base
    image.u32(HEADER + T_SCENARIO + 0x354 + 4, 0x1000);
    assert!(matches!(HaloMap::from_bytes(&image.0), Err(MapError::Malformed(_))));
}

#[test]
fn a_stream_that_inflates_to_the_wrong_size_is_refused() {
    let mut image = build();
    // the header claims one more byte than the stream holds
    let length = image.0.len() as u32 + 1;
    image.u32(8, length);
    assert!(matches!(HaloMap::from_bytes(&compress(&image.0)), Err(MapError::Decompress(_))));
}

#[test]
fn a_truncated_map_is_refused_and_never_panics() {
    let image = build().0;
    for len in (0..image.len()).step_by(97) {
        assert!(HaloMap::from_bytes(&image[..len]).is_err(), "{len} bytes loaded");
    }
    let file = compress(&image);
    for len in (0..file.len() - 700).step_by(97) {
        assert!(HaloMap::from_bytes(&file[..len]).is_err(), "{len} compressed bytes loaded");
    }
}
