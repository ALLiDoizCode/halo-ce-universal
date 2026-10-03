//! Parsing of the cache file: the header, the zlib stream, the tag index, the
//! scenario's multiplayer placements and the structure BSP's collision BSP.
//!
//! Struct layouts come from the engine source (`source/scenario/`,
//! `source/cache/`); the offsets below were produced by compiling a probe
//! against the engine headers with the port's ABI flags, so they are the
//! engine's own numbers and not hand counts.

use std::io::Read;
use std::path::Path;

use crate::collision::{Bsp2dNode, Bsp2dReference, Bsp3dNode, CollisionBsp, Edge, Leaf, Plane3d, Surface, Vertex};
use crate::error::{malformed, MapError, Result};
use crate::movement::Movement;
use crate::reader::{Raw, Space};

/// physical_memory_map.c: TAG_CACHE_BASE_ADDRESS, where tag data is loaded
/// and so what its pointers are relative to.
const TAG_CACHE_BASE_ADDRESS: u32 = 0x803A_6000;
const CACHE_HEADER_SIZE: usize = 0x800;
const CACHE_VERSION: i32 = 5;

// struct scenario (0x5B0 bytes)
const SCN_SIZE: usize = 0x5B0;
const SCN_TYPE: usize = 0x3C;
const SCN_VEHICLES: usize = 0x240;
const SCN_VEHICLE_PALETTE: usize = 0x24C;
const SCN_PLAYERS: usize = 0x354;
const SCN_NETGAME_FLAGS: usize = 0x378;
const SCN_NETGAME_EQUIPMENT: usize = 0x384;
const SCN_STRUCTURE_BSP_REFERENCES: usize = 0x5A4;

// struct game_globals (0x1AC bytes), the tag of group 'matg'
const GLOBALS_SIZE: usize = 0x1AC;
const GLOBALS_MULTIPLAYER_INFORMATION: usize = 0x164;
const GLOBALS_PLAYER_INFORMATION: usize = 0x170;
const GLOBALS_FALLING_DAMAGE: usize = 0x188;
const SZ_MULTIPLAYER_INFORMATION: usize = 0xA0;
const SZ_PLAYER_INFORMATION: usize = 0xF4;
// game_globals_falling_damage (0x98 bytes), the runtime values at its end
const SZ_FALLING_DAMAGE: usize = 0x98;
const FD_RUNTIME_MAXIMUM_FALLING_VELOCITY: usize = 0x8C;
const FD_RUNTIME_MINIMUM_DAMAGE_VELOCITY: usize = 0x90;
const FD_RUNTIME_MAXIMUM_DAMAGE_VELOCITY: usize = 0x94;
/// game_globals_multiplayer_information: the player's unit, a tag reference
const MPI_UNIT: usize = 0x10;
// game_globals_player_information
const PI_RUN_FORWARD_SPEED: usize = 0x34;
const PI_RUN_BACKWARD_SPEED: usize = 0x38;
const PI_RUN_SIDEWAYS_SPEED: usize = 0x3C;
const PI_RUN_ACCELERATION: usize = 0x40;
const PI_SNEAK_FORWARD_SPEED: usize = 0x44;
const PI_SNEAK_BACKWARD_SPEED: usize = 0x48;
const PI_SNEAK_SIDEWAYS_SPEED: usize = 0x4C;
const PI_SNEAK_ACCELERATION: usize = 0x50;
const PI_AIRBORNE_ACCELERATION: usize = 0x54;
// struct biped_definition (0x4F4 bytes), whose biped part starts at 0x2F0
const BIPED_SIZE: usize = 0x4F4;
const BIPED_DOWNHILL_VELOCITY_SCALE: usize = 0x2F0 + 0x74;
const BIPED_UPHILL_VELOCITY_SCALE: usize = 0x2F0 + 0x80;
const BIPED_JUMP_VELOCITY: usize = 0x2F0 + 0xC4;
const BIPED_MAXIMUM_SOFT_LANDING_TIME: usize = 0x2F0 + 0xE4;
const BIPED_MAXIMUM_HARD_LANDING_TIME: usize = 0x2F0 + 0xE8;
const BIPED_MINIMUM_SOFT_LANDING_VELOCITY: usize = 0x2F0 + 0xEC;
const BIPED_MINIMUM_HARD_LANDING_VELOCITY: usize = 0x2F0 + 0xF0;
const BIPED_MAXIMUM_HARD_LANDING_VELOCITY: usize = 0x2F0 + 0xF4;
const BIPED_COLLISION_HEIGHT_STANDING: usize = 0x2F0 + 0x134;
const BIPED_COLLISION_HEIGHT_CROUCHING: usize = 0x2F0 + 0x138;
const BIPED_COLLISION_RADIUS: usize = 0x2F0 + 0x13C;
const BIPED_RUNTIME_CROUCH_TRANSITION_VELOCITY: usize = 0x2F0 + 0x1DC;
const BIPED_RUNTIME_MINIMUM_NORMAL_K: usize = 0x2F0 + 0x1E0;
const BIPED_RUNTIME_DOWNHILL_K0: usize = 0x2F0 + 0x1E4;
const BIPED_RUNTIME_DOWNHILL_K1: usize = 0x2F0 + 0x1E8;
const BIPED_RUNTIME_UPHILL_K0: usize = 0x2F0 + 0x1EC;
const BIPED_RUNTIME_UPHILL_K1: usize = 0x2F0 + 0x1F0;

// element sizes
const SZ_PLAYER_START: usize = 0x34;
const SZ_NETGAME_FLAG: usize = 0x94;
const SZ_NETGAME_EQUIPMENT: usize = 0x90;
const SZ_VEHICLE: usize = 0x78;
const SZ_PALETTE_ENTRY: usize = 0x30;
const SZ_BSP_REFERENCE: usize = 0x20;
const SZ_TAG_INSTANCE: usize = 0x20;

// struct structure_bsp (0x288 bytes)
const SBSP_SIZE: usize = 0x288;
const SBSP_COLLISION_MATERIALS: usize = 0xA4;
const SBSP_COLLISION_BSP: usize = 0xB0;
const SBSP_WORLD_BOUNDS: usize = 0xC8;
const SZ_COLLISION_BSP: usize = 0x60;

const SZ_BSP3D_NODE: usize = 0x0C;
const SZ_PLANE3D: usize = 0x10;
const SZ_LEAF: usize = 0x08;
const SZ_BSP2D_REFERENCE: usize = 0x08;
const SZ_BSP2D_NODE: usize = 0x14;
const SZ_SURFACE: usize = 0x0C;
const SZ_EDGE: usize = 0x18;
const SZ_VERTEX: usize = 0x10;

/// Values of `PlayerStart::game_types` and `NetgameEquipment::game_types`.
pub mod game_type {
    pub const NONE: i16 = 0;
    pub const CTF: i16 = 1;
    pub const SLAYER: i16 = 2;
    pub const ODDBALL: i16 = 3;
    pub const KING: i16 = 4;
    pub const RACE: i16 = 5;
    /// All game types.
    pub const ALL: i16 = 12;
    /// All except CTF.
    pub const ALL_NON_CTF: i16 = 13;
    /// All except CTF and Race.
    pub const ALL_NORMAL: i16 = 14;
}

/// Values of `NetgameFlag::flag_type` (`enum netgame_flag_type`).
pub mod flag_type {
    pub const CTF_FLAG: i16 = 0;
    pub const CTF_VEHICLE: i16 = 1;
    pub const ODDBALL_BALL_SPAWN: i16 = 2;
    pub const RACE_TRACK: i16 = 3;
    pub const RACE_VEHICLE: i16 = 4;
    pub const VEGAS_BANK: i16 = 5;
    pub const TELEPORTER_SOURCE: i16 = 6;
    pub const TELEPORTER_TARGET: i16 = 7;
    pub const HILL: i16 = 8;
}

/// What the cache file's header says about the map.
#[derive(Debug, Clone, PartialEq)]
pub struct MapHeader {
    /// The map's name, such as `bloodgulch`.
    pub name: String,
    pub build: String,
    pub checksum: u32,
    /// Size of the whole file once inflated, header included.
    pub file_length: u32,
    /// Whether the file was a header followed by a zlib stream (as on the
    /// retail Xbox disc) rather than already inflated.
    pub compressed: bool,
}

/// A player starting location.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerStart {
    pub position: [f32; 3],
    /// Radians.
    pub facing: f32,
    /// 0 or 1 in team games, -1 for none.
    pub team_index: i16,
    /// `game_type` values; the start is used for each game type listed.
    pub game_types: [i16; 4],
}

/// A netgame flag: a CTF flag or vehicle spot, the oddball spawn, a race
/// checkpoint or a hill. The engine's flag carries no tag.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NetgameFlag {
    pub position: [f32; 3],
    /// Radians.
    pub facing: f32,
    /// A `flag_type` value.
    pub flag_type: i16,
    pub team_index: i16,
}

/// A netgame equipment spawn: a weapon, grenade or powerup placed for
/// multiplayer.
#[derive(Debug, Clone, PartialEq)]
pub struct NetgameEquipment {
    pub flags: u32,
    /// `game_type` values.
    pub game_types: [i16; 4],
    pub team_index: i16,
    /// Seconds the engine waits before respawning it.
    pub spawn_time: i16,
    pub position: [f32; 3],
    /// Radians.
    pub facing: f32,
    /// The item collection tag, as `name.group`.
    pub tag_name: String,
}

/// A vehicle placed in the scenario.
#[derive(Debug, Clone, PartialEq)]
pub struct VehiclePlacement {
    pub position: [f32; 3],
    /// Yaw, pitch and roll in radians.
    pub rotation: [f32; 3],
    /// The vehicle tag, as `name.group`.
    pub tag_name: String,
}

/// The parts of a map the large-scale mode needs.
#[derive(Debug, Clone, PartialEq)]
pub struct HaloMap {
    pub header: MapHeader,
    /// The scenario tag's name.
    pub scenario_name: String,
    /// The structure BSP tag's name.
    pub structure_bsp_name: String,
    /// `x0, x1, y0, y1, z0, z1` in world units.
    pub world_bounds: [f32; 6],
    pub collision: CollisionBsp,
    /// What the tags say of how a player moves on foot.
    pub movement: Movement,
    /// How many collision materials the structure BSP has.
    pub collision_material_count: usize,
    pub player_starts: Vec<PlayerStart>,
    pub netgame_flags: Vec<NetgameFlag>,
    pub netgame_equipment: Vec<NetgameEquipment>,
    pub vehicles: Vec<VehiclePlacement>,
}

impl HaloMap {
    /// Read a map file from disk, compressed or not.
    pub fn from_path(path: impl AsRef<Path>) -> Result<HaloMap> {
        Self::from_bytes(&std::fs::read(path)?)
    }

    /// Read a map file from memory, compressed or not.
    pub fn from_bytes(file: &[u8]) -> Result<HaloMap> {
        let (image, compressed) = inflate(file)?;
        parse(&image, compressed)
    }
}

/// The inflated image of a map file, and whether it had to be inflated.
///
/// A retail file is the header stored raw followed by one zlib stream that
/// inflates to `file_length - 0x800` bytes (then zero padding), as in the
/// engine's `cache_files_decompress_windows.c`; a file at least `file_length`
/// long is already inflated.
fn inflate(file: &[u8]) -> Result<(Vec<u8>, bool)> {
    let raw = Raw(file);
    if file.len() < CACHE_HEADER_SIZE {
        return malformed("too small to be a cache file");
    }
    if raw.u32(0)? != u32::from_be_bytes(*b"head") || raw.u32(0x7FC)? != u32::from_be_bytes(*b"foot") {
        return malformed("missing head/foot signatures");
    }
    let file_length = raw.u32(8)? as usize;
    if file_length < CACHE_HEADER_SIZE {
        return malformed("header states a file shorter than the header");
    }
    if file.len() >= file_length {
        return Ok((file.to_vec(), false));
    }
    let body = file_length - CACHE_HEADER_SIZE;
    let mut out = Vec::with_capacity(file_length);
    out.extend_from_slice(&file[..CACHE_HEADER_SIZE]);
    // `take` bounds the output to what the header states, so a corrupt or
    // hostile stream cannot inflate without limit
    let mut stream = flate2::read::ZlibDecoder::new(&file[CACHE_HEADER_SIZE..]).take(body as u64 + 1);
    stream.read_to_end(&mut out).map_err(|e| MapError::Decompress(e.to_string()))?;
    if out.len() != file_length {
        return Err(MapError::Decompress(format!("inflated to {} bytes, the header says {file_length}", out.len())));
    }
    Ok((out, true))
}

fn group_string(tag: u32) -> String {
    tag.to_be_bytes().iter().map(|&b| b as char).collect()
}

struct TagInstance {
    group: String,
    tag_index: u32,
    name: String,
    base_address: u32,
}

fn parse(data: &[u8], compressed: bool) -> Result<HaloMap> {
    let raw = Raw(data);

    // struct cache_file_header (0x800)
    let version = raw.i32(4)?;
    if version != CACHE_VERSION {
        return Err(MapError::UnsupportedVersion(version));
    }
    let tag_data_offset = raw.u32(0x10)? as usize;
    let tag_data_size = raw.u32(0x14)? as usize;
    let header = MapHeader {
        name: raw.cstr(0x20, 0x20)?,
        build: raw.cstr(0x40, 0x20)?,
        checksum: raw.u32(0x64)?,
        file_length: raw.u32(8)?,
        compressed,
    };

    let tags_space =
        Space { what: "tag data", base: TAG_CACHE_BASE_ADDRESS, file_offset: tag_data_offset, size: tag_data_size };
    raw.bytes(tags_space.file_offset, tags_space.size)?;

    // struct cache_file_tag_header at the start of the tag data
    let th = tags_space.file_offset;
    if raw.u32(th + 0x20)? != u32::from_be_bytes(*b"tags") {
        return malformed("tag header signature is not 'tags'");
    }
    let scenario_tag_index = raw.u32(th + 4)?;
    let tag_count = raw.u32(th + 0xC)? as usize;
    let Some(instances_size) = tag_count.checked_mul(SZ_TAG_INSTANCE) else {
        return malformed("tag count overflows");
    };
    let instances = tags_space.resolve(raw.u32(th)?, instances_size)?;

    let mut tags = Vec::with_capacity(tag_count);
    for i in 0..tag_count {
        let o = instances + i * SZ_TAG_INSTANCE;
        let name_off = tags_space.resolve(raw.u32(o + 0x10)?, 1)?;
        tags.push(TagInstance {
            group: group_string(raw.u32(o)?),
            tag_index: raw.u32(o + 0xC)?,
            name: raw.cstr(name_off, 256)?,
            base_address: raw.u32(o + 0x14)?,
        });
    }

    // struct tag_reference { group_tag, name, name_length, index } at `off`,
    // as "name.group". The absolute tag index is the datum index's low 16 bits.
    let tag_name = |off: usize| -> Result<String> {
        let index = raw.u32(off + 0xC)?;
        if index == 0xFFFF_FFFF {
            return Ok(String::new());
        }
        let Some(tag) = tags.get((index & 0xFFFF) as usize) else {
            return malformed(format!("tag reference 0x{index:08X} is out of range"));
        };
        if tag.tag_index != index {
            return malformed(format!("tag reference 0x{index:08X} does not match tag 0x{:08X}", tag.tag_index));
        }
        Ok(format!("{}.{}", tag.name, tag.group.trim_end()))
    };

    let Some(scenario) = tags.get((scenario_tag_index & 0xFFFF) as usize) else {
        return malformed("scenario tag index out of range");
    };
    if scenario.group != "scnr" {
        return malformed(format!("scenario tag has group '{}'", scenario.group));
    }
    let scn = tags_space.resolve(scenario.base_address, SCN_SIZE)?;
    let scenario_type = raw.i16(scn + SCN_TYPE)?;
    if scenario_type != 1 {
        return malformed(format!("scenario type {scenario_type} is not multiplayer"));
    }

    let (n, p) = tags_space.block(&raw, scn + SCN_PLAYERS, SZ_PLAYER_START)?;
    let mut player_starts = Vec::with_capacity(n);
    for i in 0..n {
        let o = p + i * SZ_PLAYER_START;
        player_starts.push(PlayerStart {
            position: raw.f32s(o)?,
            facing: raw.f32(o + 0xC)?,
            team_index: raw.i16(o + 0x10)?,
            game_types: raw.i16s(o + 0x14)?,
        });
    }

    let (n, p) = tags_space.block(&raw, scn + SCN_NETGAME_FLAGS, SZ_NETGAME_FLAG)?;
    let mut netgame_flags = Vec::with_capacity(n);
    for i in 0..n {
        let o = p + i * SZ_NETGAME_FLAG;
        netgame_flags.push(NetgameFlag {
            position: raw.f32s(o)?,
            facing: raw.f32(o + 0xC)?,
            flag_type: raw.i16(o + 0x10)?,
            team_index: raw.i16(o + 0x12)?,
        });
    }

    let (n, p) = tags_space.block(&raw, scn + SCN_NETGAME_EQUIPMENT, SZ_NETGAME_EQUIPMENT)?;
    let mut netgame_equipment = Vec::with_capacity(n);
    for i in 0..n {
        let o = p + i * SZ_NETGAME_EQUIPMENT;
        netgame_equipment.push(NetgameEquipment {
            flags: raw.u32(o)?,
            game_types: raw.i16s(o + 4)?,
            team_index: raw.i16(o + 0xC)?,
            spawn_time: raw.i16(o + 0xE)?,
            position: raw.f32s(o + 0x40)?,
            facing: raw.f32(o + 0x4C)?,
            tag_name: tag_name(o + 0x50)?,
        });
    }

    // scenario vehicles: a struct scenario_object_datum, whose palette entry
    // index names the vehicle tag
    let (palette_count, palette) = tags_space.block(&raw, scn + SCN_VEHICLE_PALETTE, SZ_PALETTE_ENTRY)?;
    let (n, p) = tags_space.block(&raw, scn + SCN_VEHICLES, SZ_VEHICLE)?;
    let mut vehicles = Vec::with_capacity(n);
    for i in 0..n {
        let o = p + i * SZ_VEHICLE;
        let palette_index = raw.i16(o)?;
        let tag_name = if palette_index < 0 {
            String::new()
        } else if (palette_index as usize) < palette_count {
            tag_name(palette + palette_index as usize * SZ_PALETTE_ENTRY)?
        } else {
            return malformed(format!("vehicle palette index {palette_index} out of range ({palette_count})"));
        };
        vehicles.push(VehiclePlacement { position: raw.f32s(o + 8)?, rotation: raw.f32s(o + 0x14)?, tag_name });
    }

    let movement = parse_movement(&raw, &tags_space, &tags)?;

    // struct scenario_structure_bsp_reference { file_offset, file_size,
    // base_address, pad, tag_reference }; a multiplayer map has one
    let (bsp_refs, bp) = tags_space.block(&raw, scn + SCN_STRUCTURE_BSP_REFERENCES, SZ_BSP_REFERENCE)?;
    if bsp_refs == 0 {
        return malformed("scenario has no structure BSP");
    }
    let bsp_space = Space {
        what: "structure bsp data",
        base: raw.u32(bp + 8)?,
        file_offset: raw.u32(bp)? as usize,
        size: raw.u32(bp + 4)? as usize,
    };
    let structure_bsp_name = tag_name(bp + 0x10)?;
    raw.bytes(bsp_space.file_offset, bsp_space.size)?;

    // struct cache_file_structure_bsp_header at the start of the BSP data
    let bh = bsp_space.file_offset;
    if raw.u32(bh + 0x14)? != u32::from_be_bytes(*b"sbsp") {
        return malformed("structure bsp header signature is not 'sbsp'");
    }
    let sbsp = bsp_space.resolve(raw.u32(bh)?, SBSP_SIZE)?;
    let world_bounds = raw.f32s(sbsp + SBSP_WORLD_BOUNDS)?;
    let collision_material_count = raw.i32(sbsp + SBSP_COLLISION_MATERIALS)?.max(0) as usize;

    let (cn, cb) = bsp_space.block(&raw, sbsp + SBSP_COLLISION_BSP, SZ_COLLISION_BSP)?;
    if cn != 1 {
        return malformed(format!("structure bsp has {cn} collision bsps, expected 1"));
    }
    let collision = parse_collision_bsp(&raw, &bsp_space, cb)?;

    // Refuse a map whose indices are out of range, so that nothing indexing
    // the collision data afterwards can go out of bounds.
    if let Some(bad) = collision.check_indices(collision_material_count).into_iter().find(|c| c.bad > 0) {
        return Err(MapError::IndexOutOfRange { field: bad.field, count: bad.bad });
    }

    Ok(HaloMap {
        header,
        scenario_name: scenario.name.clone(),
        structure_bsp_name,
        world_bounds,
        collision,
        movement,
        collision_material_count,
        player_starts,
        netgame_flags,
        netgame_equipment,
        vehicles,
    })
}

/// The movement values: the globals tag's player information, and the tag of
/// the biped a multiplayer player is (the globals' multiplayer information
/// names it).
fn parse_movement(raw: &Raw, space: &Space, tags: &[TagInstance]) -> Result<Movement> {
    let Some(globals) = tags.iter().find(|t| t.group == "matg") else {
        return malformed("the map has no globals tag");
    };
    let g = space.resolve(globals.base_address, GLOBALS_SIZE)?;

    let (n, mpi) = space.block(raw, g + GLOBALS_MULTIPLAYER_INFORMATION, SZ_MULTIPLAYER_INFORMATION)?;
    if n == 0 {
        return malformed("the globals tag has no multiplayer information");
    }
    // the unit's tag reference: the datum index is its fourth word
    let unit_index = raw.u32(mpi + MPI_UNIT + 0xC)?;
    let Some(biped_tag) = tags.get((unit_index & 0xFFFF) as usize).filter(|t| t.tag_index == unit_index) else {
        return malformed("the multiplayer unit is not a tag of the map");
    };
    if biped_tag.group != "bipd" {
        return malformed(format!("the multiplayer unit has group '{}', not a biped", biped_tag.group));
    }
    let b = space.resolve(biped_tag.base_address, BIPED_SIZE)?;

    let (n, pi) = space.block(raw, g + GLOBALS_PLAYER_INFORMATION, SZ_PLAYER_INFORMATION)?;
    if n == 0 {
        return malformed("the globals tag has no player information");
    }

    let (n, fd) = space.block(raw, g + GLOBALS_FALLING_DAMAGE, SZ_FALLING_DAMAGE)?;
    if n == 0 {
        return malformed("the globals tag has no falling damage");
    }

    let movement = Movement {
        run_forward_speed: raw.f32(pi + PI_RUN_FORWARD_SPEED)?,
        run_backward_speed: raw.f32(pi + PI_RUN_BACKWARD_SPEED)?,
        run_sideways_speed: raw.f32(pi + PI_RUN_SIDEWAYS_SPEED)?,
        run_acceleration: raw.f32(pi + PI_RUN_ACCELERATION)?,
        sneak_forward_speed: raw.f32(pi + PI_SNEAK_FORWARD_SPEED)?,
        sneak_backward_speed: raw.f32(pi + PI_SNEAK_BACKWARD_SPEED)?,
        sneak_sideways_speed: raw.f32(pi + PI_SNEAK_SIDEWAYS_SPEED)?,
        sneak_acceleration: raw.f32(pi + PI_SNEAK_ACCELERATION)?,
        airborne_acceleration: raw.f32(pi + PI_AIRBORNE_ACCELERATION)?,
        collision_radius: raw.f32(b + BIPED_COLLISION_RADIUS)?,
        collision_height_standing: raw.f32(b + BIPED_COLLISION_HEIGHT_STANDING)?,
        collision_height_crouching: raw.f32(b + BIPED_COLLISION_HEIGHT_CROUCHING)?,
        minimum_normal_k: raw.f32(b + BIPED_RUNTIME_MINIMUM_NORMAL_K)?,
        downhill_k0: raw.f32(b + BIPED_RUNTIME_DOWNHILL_K0)?,
        downhill_k1: raw.f32(b + BIPED_RUNTIME_DOWNHILL_K1)?,
        downhill_velocity_scale: raw.f32(b + BIPED_DOWNHILL_VELOCITY_SCALE)?,
        uphill_k0: raw.f32(b + BIPED_RUNTIME_UPHILL_K0)?,
        uphill_k1: raw.f32(b + BIPED_RUNTIME_UPHILL_K1)?,
        uphill_velocity_scale: raw.f32(b + BIPED_UPHILL_VELOCITY_SCALE)?,
        jump_velocity: raw.f32(b + BIPED_JUMP_VELOCITY)?,
        crouch_transition_velocity: raw.f32(b + BIPED_RUNTIME_CROUCH_TRANSITION_VELOCITY)?,
        maximum_soft_landing_time: raw.f32(b + BIPED_MAXIMUM_SOFT_LANDING_TIME)?,
        maximum_hard_landing_time: raw.f32(b + BIPED_MAXIMUM_HARD_LANDING_TIME)?,
        minimum_soft_landing_velocity: raw.f32(b + BIPED_MINIMUM_SOFT_LANDING_VELOCITY)?,
        minimum_hard_landing_velocity: raw.f32(b + BIPED_MINIMUM_HARD_LANDING_VELOCITY)?,
        maximum_hard_landing_velocity: raw.f32(b + BIPED_MAXIMUM_HARD_LANDING_VELOCITY)?,
        minimum_damage_velocity: raw.f32(fd + FD_RUNTIME_MINIMUM_DAMAGE_VELOCITY)?,
        maximum_damage_velocity: raw.f32(fd + FD_RUNTIME_MAXIMUM_DAMAGE_VELOCITY)?,
        maximum_falling_velocity: raw.f32(fd + FD_RUNTIME_MAXIMUM_FALLING_VELOCITY)?,
    };
    if !movement.is_sane() {
        return malformed(format!("the movement values of the tags are not usable: {movement:?}"));
    }
    Ok(movement)
}

/// struct collision_bsp (0x60): eight tag blocks in the order bsp3d nodes,
/// planes, leaves, bsp2d references, bsp2d nodes, surfaces, edges, vertices.
fn parse_collision_bsp(raw: &Raw, space: &Space, cb: usize) -> Result<CollisionBsp> {
    let mut out = CollisionBsp::default();

    let (n, p) = space.block(raw, cb, SZ_BSP3D_NODE)?;
    for i in 0..n {
        let o = p + i * SZ_BSP3D_NODE;
        out.bsp3d_nodes.push(Bsp3dNode { plane: raw.i32(o)?, children: [raw.i32(o + 4)?, raw.i32(o + 8)?] });
    }
    let (n, p) = space.block(raw, cb + 0x0C, SZ_PLANE3D)?;
    for i in 0..n {
        let o = p + i * SZ_PLANE3D;
        out.planes.push(Plane3d { n: raw.f32s(o)?, d: raw.f32(o + 12)? });
    }
    let (n, p) = space.block(raw, cb + 0x18, SZ_LEAF)?;
    for i in 0..n {
        let o = p + i * SZ_LEAF;
        out.leaves.push(Leaf {
            flags: raw.u16(o)?,
            bsp2d_reference_count: raw.i16(o + 2)?,
            first_bsp2d_reference: raw.i32(o + 4)?,
        });
    }
    let (n, p) = space.block(raw, cb + 0x24, SZ_BSP2D_REFERENCE)?;
    for i in 0..n {
        let o = p + i * SZ_BSP2D_REFERENCE;
        out.bsp2d_references.push(Bsp2dReference { plane: raw.i32(o)?, root: raw.i32(o + 4)? });
    }
    let (n, p) = space.block(raw, cb + 0x30, SZ_BSP2D_NODE)?;
    for i in 0..n {
        let o = p + i * SZ_BSP2D_NODE;
        out.bsp2d_nodes.push(Bsp2dNode {
            n: raw.f32s(o)?,
            d: raw.f32(o + 8)?,
            children: [raw.i32(o + 12)?, raw.i32(o + 16)?],
        });
    }
    let (n, p) = space.block(raw, cb + 0x3C, SZ_SURFACE)?;
    for i in 0..n {
        let o = p + i * SZ_SURFACE;
        out.surfaces.push(Surface {
            plane: raw.i32(o)?,
            first_edge: raw.i32(o + 4)?,
            flags: raw.u8(o + 8)?,
            breakable_surface: raw.u8(o + 9)?,
            material: raw.i16(o + 10)?,
        });
    }
    let (n, p) = space.block(raw, cb + 0x48, SZ_EDGE)?;
    for i in 0..n {
        let o = p + i * SZ_EDGE;
        out.edges.push(Edge {
            vertices: [raw.i32(o)?, raw.i32(o + 4)?],
            edges: [raw.i32(o + 8)?, raw.i32(o + 12)?],
            surfaces: [raw.i32(o + 16)?, raw.i32(o + 20)?],
        });
    }
    let (n, p) = space.block(raw, cb + 0x54, SZ_VERTEX)?;
    for i in 0..n {
        let o = p + i * SZ_VERTEX;
        out.vertices.push(Vertex { point: raw.f32s(o)?, first_edge: raw.i32(o + 12)? });
    }
    Ok(out)
}
