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
const TAG_DATA_SIZE: usize = 0x4400;
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
    fn u16(&mut self, at: usize, v: u16) {
        self.0[at..at + 2].copy_from_slice(&v.to_le_bytes());
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
// fighting: the biped's collision model, a pistol with its magazine, trigger,
// projectile and damage, its first-person animations, and the scenario's
// starting equipment
const T_COLLISION_MODEL: usize = 0x1A00;
const T_WEAPON: usize = 0x1D00;
const T_PROJECTILE: usize = 0x2300;
const T_DAMAGE_EFFECT: usize = 0x2600;
const T_ANIMATION_GRAPH: usize = 0x2900;
const T_FIRST_PERSON_ANIMATIONS: usize = 0x2A00;
const T_FIRST_PERSON_INDICES: usize = 0x2A40;
const T_ANIMATIONS: usize = 0x2A80;
const T_WEAPON_ANIMATIONS: usize = 0x2B00;
const T_WEAPON_INDICES: usize = 0x2B40;
const T_MAGAZINES: usize = 0x2C00;
const T_TRIGGERS: usize = 0x2C80;
const T_MATERIALS: usize = 0x2E00;
const T_STARTING: usize = 0x2F00;
const T_ITEM_COLLECTION: usize = 0x3000;
const T_PERMUTATIONS: usize = 0x3080;
// the bullet's detonation effect: one event of two parts, a damage effect and something else
const T_EFFECT: usize = 0x3100;
const T_EFFECT_EVENT: usize = 0x3140;
const T_EFFECT_PARTS: usize = 0x3190;
// the warthog: its tag, its physics with one powered and two plain mass points,
// two seats and one weapon
const T_VEHICLE: usize = 0x3400;
const T_PHYSICS: usize = 0x3800;
const T_POWERED_MASS_POINTS: usize = 0x3880;
const T_MASS_POINTS: usize = 0x3900;
const T_SEATS: usize = 0x3A00;
const T_INITIAL_WEAPONS: usize = 0x3D00;
const NONE: u32 = 0xFFFF_FFFF;

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
    m.u32(t + 0xC, 13);
    m.code(t + 0x20, b"tags");
    let tags: [(&[u8; 4], &str); 13] = [
        (b"scnr", "levels\\synth\\synth"),
        (b"itmc", "item collections\\pistol"),
        (b"vehi", "vehicles\\warthog\\warthog"),
        (b"sbsp", "levels\\synth\\synth"),
        (b"matg", "globals\\globals"),
        (b"bipd", "characters\\cyborg_mp\\cyborg_mp"),
        (b"coll", "characters\\cyborg\\cyborg"),
        (b"weap", "weapons\\pistol\\pistol"),
        (b"proj", "weapons\\pistol\\bullet"),
        (b"jpt!", "weapons\\pistol\\bullet"),
        (b"antr", "weapons\\pistol\\fp"),
        (b"effe", "weapons\\pistol\\bullet hit"),
        (b"phys", "vehicles\\warthog\\warthog"),
    ];
    let mut name_at = T_NAMES;
    for (i, (group, name)) in tags.iter().enumerate() {
        let o = t + T_INSTANCES + i * 0x20;
        m.code(o, group);
        m.u32(o + 0xC, tag_id(i as u32));
        m.u32(o + 0x10, TAG_BASE + name_at as u32);
        m.cstr(t + name_at, name);
        name_at += 0x20;
    }
    m.u32(t + T_INSTANCES + 0x14, TAG_BASE + T_SCENARIO as u32);
    m.u32(t + T_INSTANCES + 4 * 0x20 + 0x14, TAG_BASE + T_GLOBALS as u32);
    m.u32(t + T_INSTANCES + 5 * 0x20 + 0x14, TAG_BASE + T_BIPED as u32);
    for (index, at) in [
        (1, T_ITEM_COLLECTION),
        (6, T_COLLISION_MODEL),
        (7, T_WEAPON),
        (8, T_PROJECTILE),
        (9, T_DAMAGE_EFFECT),
        (10, T_ANIMATION_GRAPH),
        (11, T_EFFECT),
        (2, T_VEHICLE),
        (12, T_PHYSICS),
    ] {
        m.u32(t + T_INSTANCES + index * 0x20 + 0x14, TAG_BASE + at as u32);
    }

    // scenario
    let s = t + T_SCENARIO;
    m.i16(s + 0x3C, 1);
    m.tag_block(s + 0x354, 1, T_PLAYERS);
    m.tag_block(s + 0x378, 1, T_FLAGS);
    m.tag_block(s + 0x384, 1, T_EQUIPMENT);
    m.tag_block(s + 0x390, 1, T_STARTING);
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
    // the vehicles' damage effects: only the one for a collision is there
    m.u32(t + T_FALLING_DAMAGE + 0x3C + 0xC, NONE);
    m.u32(t + T_FALLING_DAMAGE + 0x4C + 0xC, NONE);
    m.tag_ref(t + T_FALLING_DAMAGE + 0x5C, 9);
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
    m.tag_ref(t + T_BIPED + 0x70, 6); // collision model

    // the multiplayer player's body: 75 health and a 75 shield, stunned for 6
    // seconds by a hit and back in 4, a head and a body
    let c = t + T_COLLISION_MODEL;
    m.u32(c, 7);
    m.i16(c + 4, 1);
    m.f32(c + 8, 75.0);
    m.f32(c + 0x44, 0.25);
    m.f32(c + 0xCC, 75.0);
    m.i16(c + 0xD2, 22);
    m.f32(c + 0x10C, 6.0);
    m.f32(c + 0x110, 4.0);
    m.f32(c + 0x1C0, 0.008_333_334);
    m.tag_block(c + 0x234, 2, T_MATERIALS);
    for (i, (flags, body)) in [(1u32, 1.0f32), (0, 0.8)].iter().enumerate() {
        let o = t + T_MATERIALS + i * 0x48;
        m.u32(o + 0x20, *flags);
        m.i16(o + 0x24, 21);
        m.f32(o + 0x2C, 1.0);
        m.f32(o + 0x3C, *body);
    }

    // the warthog: a jeep that does 28 forward and 9 back, with the player's body,
    // a physics tag and a pistol in the hand of its gunner
    let wh = t + T_VEHICLE;
    m.u16(wh + 2, 5);
    m.f32(wh + 4, 3.0); // bounding radius
    m.f32s(wh + 8, &[0.0, 0.0, 0.75]);
    m.tag_ref(wh + 0x70, 6);
    m.tag_ref(wh + 0x80, 12);
    m.u32(wh + 0x17C, 0x40);
    m.f32(wh + 0x184, 0.5);
    m.tag_block(wh + 0x2D8, 1, T_INITIAL_WEAPONS);
    m.tag_ref(t + T_INITIAL_WEAPONS, 7);
    m.tag_block(wh + 0x2E4, 2, T_SEATS);
    for (i, (flags, label, marker)) in
        [(0x04u32, "warthog_d", "driver"), (0x08, "warthog_g", "gunner")].iter().enumerate()
    {
        let o = t + T_SEATS + i * 0x11C;
        m.u32(o, *flags);
        m.cstr(o + 4, label);
        m.cstr(o + 0x24, marker);
        m.f32s(o + 0x64, &[0.0, 0.5, 0.25]);
        m.f32s(o + 0x7C, &[1.5, 1.25]);
        m.f32s(o + 0xF0, &[-1.0, 1.0]);
    }
    m.u32(wh + 0x2F0, 0x80); // flags: causes collision damage
    m.i16(wh + 0x2F4, 1); // a jeep
    m.f32s(wh + 0x2F8, &[28.0, -9.0, 15.0, 30.0, 0.5, 0.625, 1.25, 0.7, 0.8]);
    m.i16(wh + 0x31C, 3);
    m.f32s(wh + 0x330, &[0.1, 0.2]);
    m.f32s(wh + 0x340, &[0.3, 0.4]);
    m.f32(wh + 0x364, 0.05);
    let ph = t + T_PHYSICS;
    m.f32s(ph, &[1.5, 2.0, 1200.0, 0.0, 0.0, 0.5, 1.0, 1.0, 0.25, 0.5, 0.75, 0.125, 0.0625]);
    m.f32s(ph + 0x38, &[0.5, 1.0, 1.5, 0.0, 0.125, 0.0, 0.1, 0.2, 0.3]);
    m.tag_block(ph + 0x68, 1, T_POWERED_MASS_POINTS);
    m.tag_block(ph + 0x74, 2, T_MASS_POINTS);
    let pm = t + T_POWERED_MASS_POINTS;
    m.cstr(pm, "engine");
    m.u32(pm + 0x20, 2);
    m.f32s(pm + 0x24, &[1.0, 0.5, 0.25, 0.75, 0.125, 0.0625]);
    for (i, name) in ["front", "back"].iter().enumerate() {
        let o = t + T_MASS_POINTS + i * 0x80;
        m.cstr(o, name);
        m.i16(o + 0x20, if i == 0 { 0 } else { -1 });
        m.i16(o + 0x22, 3 + i as i16);
        m.u32(o + 0x24, 1);
        m.f32s(o + 0x28, &[1.0, 2.0, 3.0, 4.0]);
        m.f32s(o + 0x38, &[0.5 - i as f32, 1.0, 0.0]);
        m.f32s(o + 0x44, &[0.0, 1.0, 0.0]);
        m.f32s(o + 0x50, &[0.0, 0.0, 1.0]);
        m.i16(o + 0x5C, 1);
        m.f32s(o + 0x60, &[0.9, 0.8, 0.4]);
    }

    // the pistol: a magazine of 12 and 60 rounds, one trigger at 3.5 a second that fires
    // a bullet of 25 damage, reloaded by a 70-frame first-person animation
    let w = t + T_WEAPON;
    m.u32(w + 0x308, 0);
    m.u32(w + 0x394 + 0xC, NONE); // no melee damage
    m.tag_ref(w + 0x46C, 10);
    m.tag_ref(w + 0x38, 10); // the weapon's own animations, the same graph
    m.tag_block(w + 0x4F0, 1, T_MAGAZINES);
    m.tag_block(w + 0x4FC, 1, T_TRIGGERS);
    let mag = t + T_MAGAZINES;
    m.i16(mag + 6, 60);
    m.i16(mag + 8, 120);
    m.i16(mag + 0xA, 12);
    m.f32(mag + 0x14, 2.17);
    m.i16(mag + 0x18, 12);
    let tr = t + T_TRIGGERS;
    m.f32s(tr + 4, &[3.5, 3.5]);
    m.f32s(tr + 0xF8, &[1.0, 1.0]);
    m.i16(tr + 0x20, 0);
    m.i16(tr + 0x22, 1);
    m.i16(tr + 0x6E, 1);
    m.tag_ref(tr + 0x94, 8);
    let pr = t + T_PROJECTILE;
    // (no super detonation and no attached damage: the references say none)
    for at in [0x18C, 0x214] {
        m.u32(pr + at + 0xC, NONE);
    }
    m.tag_ref(pr + 0x1AC, 11);
    m.f32s(pr + 0x1D0, &[20.0, 50.0]);
    m.tag_block(t + T_EFFECT + 0x34, 1, T_EFFECT_EVENT);
    m.tag_block(t + T_EFFECT_EVENT + 0x2C, 2, T_EFFECT_PARTS);
    m.tag_ref(t + T_EFFECT_PARTS + 0x18, 9); // a damage effect
    m.tag_ref(t + T_EFFECT_PARTS + 0x68 + 0x18, 7); // a weapon, which no effect makes
    m.f32(pr + 0x1C8, 40.0);
    m.f32s(pr + 0x1E4, &[10.0, 10.0]);
    m.tag_ref(pr + 0x224, 9);
    let d = t + T_DAMAGE_EFFECT;
    m.i16(d + 0x1C6, 2);
    m.u32(d + 0x1C8, 2);
    m.f32s(d + 0x1D0, &[25.0, 25.0, 25.0]);
    m.f32s(d, &[0.5, 2.0, 0.25]); // radii of an explosion: full to 0.5, none beyond 2
    m.u32(d + 0xC, 1);
    m.f32(d + 0x1CC, 0.6);
    m.f32(d + 0x200 + 4 * 21, 1.5);
    m.f32(d + 0x200 + 4 * 22, 1.0);

    // its first-person animations: the animation for reloading is the first of one
    let a = t + T_ANIMATION_GRAPH;
    m.tag_block(a + 0x48, 1, T_FIRST_PERSON_ANIMATIONS);
    m.tag_block(a + 0x74, 2, T_ANIMATIONS);
    m.tag_block(t + T_FIRST_PERSON_ANIMATIONS + 0x10, 8, T_FIRST_PERSON_INDICES);
    for i in 0..8usize {
        m.i16(t + T_FIRST_PERSON_INDICES + 2 * i, if i == 7 { 0 } else { -1 });
    }
    m.i16(t + T_ANIMATIONS + 0x22, 70);
    // ... and the weapon's own: the primary recoil (the tenth) is the second animation, 5 frames
    m.tag_block(a + 0x18, 1, T_WEAPON_ANIMATIONS);
    m.tag_block(t + T_WEAPON_ANIMATIONS + 0x10, 10, T_WEAPON_INDICES);
    for i in 0..10usize {
        m.i16(t + T_WEAPON_INDICES + 2 * i, if i == 9 { 1 } else { -1 });
    }
    m.i16(t + T_ANIMATIONS + 0xB4 + 0x22, 5);

    // what the map starts a Slayer player with: the one item collection, whose one item is the pistol
    let se = t + T_STARTING;
    m.i16(se + 4, game_type::SLAYER);
    m.tag_ref(se + 0x3C, 1);
    for i in 1..6usize {
        m.u32(se + 0x3C + 0x10 * i + 0xC, NONE);
    }
    m.tag_block(t + T_ITEM_COLLECTION, 1, T_PERMUTATIONS);
    m.f32(t + T_PERMUTATIONS + 0x20, 100.0);
    m.tag_ref(t + T_PERMUTATIONS + 0x24, 7);

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
fn the_weapons_and_the_players_body_are_read_from_the_tags() {
    let c = HaloMap::from_bytes(&build().0).unwrap().combat;
    assert_eq!(c.weapons.len(), 1);
    let w = &c.weapons[0];
    assert_eq!((w.tag_index, w.name.as_str(), w.reload_frames), (7, "weapons\\pistol\\pistol.weap", 70));
    assert_eq!(w.recoil_frames, 5);
    assert_eq!(w.melee_damage, None);
    let m = &w.magazines[0];
    assert_eq!((m.rounds_total_initial, m.rounds_total_maximum, m.rounds_loaded_maximum), (60, 120, 12));
    assert_eq!((m.reload_time, m.rounds_reloaded), (2.17, 12));
    let t = &w.triggers[0];
    assert_eq!((t.initial_rate_of_fire, t.final_rate_of_fire, t.rate_of_fire_acceleration), (3.5, 3.5, 1.0));
    assert_eq!((t.magazine_index, t.rounds_per_shot, t.projectiles_per_shot), (0, 1, 1));
    let p = t.projectile.as_ref().expect("the trigger fires a projectile");
    assert_eq!((p.maximum_range, p.initial_velocity), (40.0, 10.0));
    let d = p.impact_damage.expect("the projectile has an impact damage");
    assert_eq!((d.category, d.flags, d.minimum, d.lower, d.upper), (2, 2, 25.0, 25.0, 25.0));
    assert_eq!(d.tag_index, 9);
    assert_eq!(
        (d.falloff_radius, d.cutoff_radius, d.cutoff_scale, d.effect_flags, d.core_radius),
        (0.5, 2.0, 0.25, 1, 0.6)
    );
    assert_eq!((p.air_damage_range_lower, p.air_damage_range_upper), (20.0, 50.0));
    // the damage effect the projectile's detonation effect makes, once, and nothing of the part that is not one
    assert_eq!(p.detonation_damage.iter().map(|d| d.tag_index).collect::<Vec<_>>(), [9]);
    assert!(p.super_detonation_damage.is_empty() && p.attached_damage.is_none());
    assert_eq!((d.material_modifiers[21], d.material_modifiers[22], d.material_modifiers[0]), (1.5, 1.0, 0.0));

    let r = &c.resistance;
    assert_eq!((r.maximum_body_vitality, r.maximum_shield_vitality, r.shield_material_type), (75.0, 75.0, 22));
    assert_eq!((r.shield_stun_time, r.shield_recharge_time, r.shield_recharge_velocity), (6.0, 4.0, 0.008_333_334));
    assert_eq!((r.flags, r.indirect_damage_material_index, r.friendly_damage_resistance), (7, 1, 0.25));
    assert_eq!(r.materials.len(), 2);
    assert_eq!((r.materials[0].flags, r.materials[0].material_type), (1, 21));
    assert_eq!(r.materials[1].body_damage_multiplier, 0.8);

    assert_eq!(c.starting_equipment.len(), 1);
    assert_eq!(c.starting_equipment[0].game_types[0], game_type::SLAYER);
    assert_eq!(c.starting_equipment[0].collections, vec![vec![(100.0, 7)]]);
    assert_eq!(c.weapon(7).map(|w| w.name.as_str()), Some("weapons\\pistol\\pistol.weap"));
    assert!(c.weapon(8).is_none());
}

#[test]
fn the_combat_values_survive_their_bytes_and_bytes_that_are_not_theirs_are_refused() {
    let c = HaloMap::from_bytes(&build().0).unwrap().combat;
    let bytes = c.to_bytes();
    assert_eq!(halo_map::combat::Combat::from_bytes(&bytes).unwrap(), c);
    for cut in [0, 3, 4, 5, bytes.len() / 2, bytes.len() - 1] {
        assert!(halo_map::combat::Combat::from_bytes(&bytes[..cut]).is_err(), "cut at {cut}");
    }
    let mut longer = bytes.clone();
    longer.push(0);
    assert!(halo_map::combat::Combat::from_bytes(&longer).is_err());
    let mut wrong = bytes;
    wrong[0] = b'X';
    assert!(halo_map::combat::Combat::from_bytes(&wrong).is_err());
}

#[test]
fn a_weapon_with_no_first_person_animations_reloads_in_no_time_and_a_dangling_reference_is_refused() {
    let mut image = build();
    image.u32(HEADER + T_WEAPON + 0x46C + 0xC, NONE);
    image.u32(HEADER + T_WEAPON + 0x38 + 0xC, NONE);
    let weapon = &HaloMap::from_bytes(&image.0).unwrap().combat.weapons[0];
    assert_eq!((weapon.reload_frames, weapon.recoil_frames), (0, 0));
    // a damage that is a tag of the wrong kind
    let mut image = build();
    image.tag_ref(HEADER + T_PROJECTILE + 0x224, 7);
    assert!(matches!(HaloMap::from_bytes(&image.0), Err(MapError::Malformed(_))));
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

#[test]
fn a_vehicle_is_read_with_its_handling_physics_seats_weapon_and_body() {
    let map = HaloMap::from_bytes(&build().0).unwrap();
    let v = &map.vehicle_tags;
    assert_eq!(v.defs.len(), 1);
    let d = &v.defs[0];
    assert_eq!(d.name, "vehicles\\warthog\\warthog.vehi");
    assert_eq!(v.placements, vec![Some(d.tag_index)]);
    assert_eq!((d.object_flags, d.bounding_radius, d.bounding_offset), (5, 3.0, [0.0, 0.0, 0.75]));
    assert_eq!((d.unit_flags, d.child_damage_fraction), (0x40, 0.5));

    let h = &d.handling;
    assert_eq!(
        (h.flags, h.vehicle_type, h.function_modes),
        (0x80, halo_map::vehicles::vehicle_type::HUMAN_JEEP, [3, 0, 0, 0])
    );
    assert_eq!((h.maximum_forward_speed, h.maximum_reverse_speed), (28.0, -9.0));
    assert_eq!((h.speed_acceleration, h.speed_deceleration), (15.0, 30.0));
    assert_eq!((h.maximum_left_turn, h.maximum_right_turn, h.wheel_circumference), (0.5, 0.625, 1.25));
    assert_eq!(
        (h.maximum_left_slide, h.maximum_right_slide, h.unknown_340, h.unknown_344, h.unknown_364),
        (0.1, 0.2, 0.3, 0.4, 0.05)
    );

    let p = d.physics.as_ref().unwrap();
    assert_eq!((p.radius, p.moment, p.mass, p.center_of_mass), (1.5, 2.0, 1200.0, [0.0, 0.0, 0.5]));
    assert_eq!((p.gravity_scale, p.ground_friction, p.xx_moment, p.zz_moment), (1.0, 0.25, 0.1, 0.3));
    assert_eq!(p.powered_mass_points.len(), 1);
    assert_eq!((p.powered_mass_points[0].name.as_str(), p.powered_mass_points[0].antigrav_strength), ("engine", 1.0));
    assert_eq!(p.mass_points.len(), 2);
    let m = &p.mass_points[1];
    assert_eq!((m.name.as_str(), m.powered_mass_point_index, m.model_node_index), ("back", -1, 4));
    assert_eq!((m.mass, m.position, m.friction_type, m.radius), (2.0, [-0.5, 1.0, 0.0], 1, 0.4));

    assert_eq!(d.seats.len(), 2);
    assert_eq!((d.seats[0].label.as_str(), d.seats[0].marker_name.as_str()), ("warthog_d", "driver"));
    assert_eq!(d.seats[1].flags & halo_map::vehicles::seat_flag::GUNNER, halo_map::vehicles::seat_flag::GUNNER);
    assert_eq!(
        (d.seats[1].acceleration_scale, d.seats[1].yaw_rate, d.seats[1].yaw_maximum),
        ([0.0, 0.5, 0.25], 1.5, 1.0)
    );

    assert_eq!(d.weapons, vec![map.combat.weapons[0].tag_index]);
    assert_eq!(d.resistance.as_ref().unwrap(), &map.combat.resistance);
    assert!(v.hit_environment_damage.is_none() && v.killed_unit_damage.is_none());
    assert!(v.collision_damage.is_some());
}

#[test]
fn the_vehicles_survive_their_bytes_and_bytes_that_are_not_theirs_are_refused() {
    use halo_map::vehicles::Vehicles;
    let v = HaloMap::from_bytes(&build().0).unwrap().vehicle_tags;
    let bytes = v.to_bytes();
    assert_eq!(Vehicles::from_bytes(&bytes).unwrap(), v);
    assert_eq!(Vehicles::from_bytes(&Vehicles::default().to_bytes()).unwrap(), Vehicles::default());
    assert!(Vehicles::from_bytes(&bytes[..bytes.len() - 1]).is_err());
    assert!(Vehicles::from_bytes(&[bytes.as_slice(), &[0]].concat()).is_err());
    assert!(Vehicles::from_bytes(b"HCV0").is_err());

    let mut broken = v.clone();
    broken.defs[0].handling.maximum_forward_speed = f32::NAN;
    assert!(Vehicles::from_bytes(&broken.to_bytes()).is_err());
    let mut broken = v.clone();
    broken.defs[0].physics.as_mut().unwrap().mass_points[0].powered_mass_point_index = 1;
    assert!(Vehicles::from_bytes(&broken.to_bytes()).is_err());
    let mut broken = v.clone();
    broken.placements.push(Some(999));
    assert!(Vehicles::from_bytes(&broken.to_bytes()).is_err());
    let mut broken = v;
    broken.collision_damage.as_mut().unwrap().upper = f32::INFINITY;
    assert!(Vehicles::from_bytes(&broken.to_bytes()).is_err());
}

#[test]
fn a_vehicle_that_carries_a_weapon_the_map_lacks_is_refused() {
    let v = HaloMap::from_bytes(&build().0).unwrap();
    let mut other = v.vehicle_tags.clone();
    other.defs[0].weapons = vec![999];
    assert!(other.check_against(&v.combat).is_err());
    assert!(v.vehicle_tags.check_against(&v.combat).is_ok());
}

#[test]
fn a_vehicle_placement_of_a_palette_entry_that_is_not_there_is_refused() {
    let mut image = build();
    // the placement's palette index, beyond the one entry
    image.i16(HEADER + T_VEHICLES, 3);
    assert!(HaloMap::from_bytes(&image.0).is_err());
}
