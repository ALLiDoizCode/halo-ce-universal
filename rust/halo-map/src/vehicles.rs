//! What the map's tags say of vehicles: for every vehicle tag, its handling
//! (`source/units/vehicles.c`'s `struct vehicle_definition`), its physics tag
//! (`source/physics/physics_definitions.h`: the body, its powered and plain
//! mass points), its seats and the weapons it carries
//! (`source/units/unit_definitions.h`), its collision model's resistance (what
//! it takes of damage), and the damage effects the game globals name for a
//! vehicle hitting things. As with [`crate::combat`] and [`crate::items`], they
//! are read from the map and never retuned.
//!
//! [`Vehicles::placements`] names, for each of the map's vehicle placements
//! ([`crate::HaloMap::vehicles`], in the same order), the vehicle tag it places.
//!
//! [`Vehicles::to_bytes`] and [`Vehicles::from_bytes`] are the byte form a
//! server keeps with the map's other data (see `halo_sim::MapData::to_bytes`).

use crate::combat::{Combat, Damage, Resistance};
use crate::error::{malformed, Result};

/// `enum vehicle_type` (`units/vehicles.c`): the Warthog is a `HUMAN_JEEP`, the
/// Scorpion a `HUMAN_TANK`, the Ghost an `ALIEN_SCOUT` and the Banshee an
/// `ALIEN_FIGHTER`.
pub mod vehicle_type {
    pub const HUMAN_TANK: i16 = 0;
    pub const HUMAN_JEEP: i16 = 1;
    pub const HUMAN_BOAT: i16 = 2;
    pub const HUMAN_PLANE: i16 = 3;
    pub const ALIEN_SCOUT: i16 = 4;
    pub const ALIEN_FIGHTER: i16 = 5;
    pub const TURRET: i16 = 6;
}

/// `struct vehicle_definition` past the unit's: how the vehicle drives. The
/// names are the engine's where it has one; the others keep the offset in the
/// tag, as `vehicles.c` has them.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Handling {
    /// `vehicle_definition.flags`.
    pub flags: u32,
    /// A [`vehicle_type`] value.
    pub vehicle_type: i16,
    pub maximum_forward_speed: f32,
    pub maximum_reverse_speed: f32,
    pub speed_acceleration: f32,
    pub speed_deceleration: f32,
    /// The turn limits (`vehicles.c`'s `unknown308` and `unknown30c`).
    pub maximum_left_turn: f32,
    pub maximum_right_turn: f32,
    pub wheel_circumference: f32,
    pub unknown_314: f32,
    pub unknown_318: f32,
    pub function_modes: [i16; 4],
    pub maximum_left_slide: f32,
    pub maximum_right_slide: f32,
    pub unknown_340: f32,
    pub unknown_344: f32,
    pub unknown_364: f32,
}

/// `struct powered_mass_point_definition`: a point that pushes, such as a
/// hovering vehicle's.
#[derive(Debug, Clone, PartialEq)]
pub struct PoweredMassPoint {
    pub name: String,
    pub flags: u32,
    pub antigrav_strength: f32,
    pub antigrav_offset: f32,
    pub antigrav_height: f32,
    pub antigrav_damp_fraction: f32,
    pub antigrav_normal_k1: f32,
    pub antigrav_normal_k0: f32,
}

/// `struct mass_point_definition`: a point of the body (a wheel, a corner).
#[derive(Debug, Clone, PartialEq)]
pub struct MassPoint {
    pub name: String,
    /// The powered mass point that drives it; -1 for none.
    pub powered_mass_point_index: i16,
    pub model_node_index: i16,
    pub flags: u32,
    pub relative_mass: f32,
    pub mass: f32,
    pub relative_density: f32,
    pub density: f32,
    pub position: [f32; 3],
    pub forward: [f32; 3],
    pub up: [f32; 3],
    pub friction_type: i16,
    pub friction_parallel_scale: f32,
    pub friction_perpendicular_scale: f32,
    pub radius: f32,
}

/// `struct physics_definition`.
#[derive(Debug, Clone, PartialEq)]
pub struct Physics {
    pub radius: f32,
    pub moment: f32,
    pub mass: f32,
    pub center_of_mass: [f32; 3],
    pub density: f32,
    pub gravity_scale: f32,
    pub ground_friction: f32,
    pub ground_depth: f32,
    pub ground_damp_fraction: f32,
    pub ground_normal_k1: f32,
    pub ground_normal_k0: f32,
    pub water_friction: f32,
    pub water_depth: f32,
    pub water_density: f32,
    pub air_friction: f32,
    pub xx_moment: f32,
    pub yy_moment: f32,
    pub zz_moment: f32,
    pub powered_mass_points: Vec<PoweredMassPoint>,
    pub mass_points: Vec<MassPoint>,
}

/// `unit_seat.flags`, the bits `units/unit_definitions.h` names.
pub mod seat_flag {
    pub const INVISIBLE: u32 = 1 << 0;
    pub const DRIVER: u32 = 1 << 2;
    pub const GUNNER: u32 = 1 << 3;
    pub const THIRD_PERSON_CAMERA: u32 = 1 << 4;
    pub const THIRD_PERSON_ON_ENTER: u32 = 1 << 6;
    pub const FIRST_PERSON_CAMERA: u32 = 1 << 7;
}

/// `struct unit_seat`: where a rider sits and what they may do there.
#[derive(Debug, Clone, PartialEq)]
pub struct Seat {
    pub flags: u32,
    pub label: String,
    pub marker_name: String,
    pub acceleration_scale: [f32; 3],
    pub yaw_rate: f32,
    pub pitch_rate: f32,
    pub yaw_minimum: f32,
    pub yaw_maximum: f32,
}

/// A vehicle tag.
#[derive(Debug, Clone, PartialEq)]
pub struct VehicleDef {
    /// The tag's index among the map's tags: what the server and the client
    /// name the vehicle by.
    pub tag_index: u16,
    /// As `name.vehicle`.
    pub name: String,
    /// `object_definition.flags` and bounding sphere.
    pub object_flags: u16,
    pub bounding_radius: f32,
    pub bounding_offset: [f32; 3],
    /// `unit_definition.flags`, and the share of a rider's damage the vehicle takes.
    pub unit_flags: u32,
    pub child_damage_fraction: f32,
    pub handling: Handling,
    pub physics: Option<Physics>,
    pub seats: Vec<Seat>,
    /// The weapons the vehicle carries (`initial_weapons`), by tag index: each
    /// is one of the map's weapons ([`Combat::weapons`]).
    pub weapons: Vec<u16>,
    /// What the vehicle takes of damage: its collision model's resistance.
    pub resistance: Option<Resistance>,
}

/// Everything the map says of vehicles.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Vehicles {
    pub defs: Vec<VehicleDef>,
    /// For each of the map's vehicle placements, in order, the tag index of
    /// the vehicle it places; `None` for a placement that names none.
    pub placements: Vec<Option<u16>>,
    /// The globals' damage effects for a vehicle hitting the environment, a
    /// unit it kills and a unit it runs into.
    pub hit_environment_damage: Option<Damage>,
    pub killed_unit_damage: Option<Damage>,
    pub collision_damage: Option<Damage>,
}

impl Vehicles {
    /// The vehicle with this tag index.
    pub fn def(&self, tag_index: u16) -> Option<&VehicleDef> {
        self.defs.iter().find(|d| d.tag_index == tag_index)
    }
}

// ---------- the byte form

const MAGIC: &[u8; 4] = b"HCV1";
/// The most elements any list of the byte form may claim.
const MAX_LIST: usize = 4096;

struct Writer(Vec<u8>);

impl Writer {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn i16(&mut self, v: i16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f32(&mut self, v: f32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f32s(&mut self, vs: &[f32]) {
        vs.iter().for_each(|v| self.f32(*v));
    }
    fn count(&mut self, n: usize) {
        self.u16(n as u16);
    }
    fn string(&mut self, s: &str) {
        self.count(s.len());
        self.0.extend_from_slice(s.as_bytes());
    }
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N]> {
        match self.0.split_at_checked(N) {
            Some((head, rest)) => {
                self.0 = rest;
                Ok(head.try_into().unwrap())
            }
            None => malformed("vehicle data is cut short"),
        }
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take::<1>()?[0])
    }
    fn i16(&mut self) -> Result<i16> {
        Ok(i16::from_le_bytes(self.take()?))
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take()?))
    }
    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.take()?))
    }
    fn f32s<const N: usize>(&mut self) -> Result<[f32; N]> {
        let mut out = [0.0; N];
        for v in &mut out {
            *v = self.f32()?;
        }
        Ok(out)
    }
    fn count(&mut self) -> Result<usize> {
        let n = self.u16()? as usize;
        if n > MAX_LIST {
            return malformed(format!("vehicle data has a list of {n}"));
        }
        Ok(n)
    }
    fn string(&mut self) -> Result<String> {
        let len = self.count()?;
        let Some((s, rest)) = self.0.split_at_checked(len) else {
            return malformed("vehicle data is cut short");
        };
        self.0 = rest;
        Ok(String::from_utf8_lossy(s).into_owned())
    }
}

impl Handling {
    fn numbers(&self) -> [f32; 14] {
        [
            self.maximum_forward_speed,
            self.maximum_reverse_speed,
            self.speed_acceleration,
            self.speed_deceleration,
            self.maximum_left_turn,
            self.maximum_right_turn,
            self.wheel_circumference,
            self.unknown_314,
            self.unknown_318,
            self.maximum_left_slide,
            self.maximum_right_slide,
            self.unknown_340,
            self.unknown_344,
            self.unknown_364,
        ]
    }

    fn write(&self, w: &mut Writer) {
        w.u32(self.flags);
        w.i16(self.vehicle_type);
        self.function_modes.iter().for_each(|m| w.i16(*m));
        w.f32s(&self.numbers());
    }

    fn read(r: &mut Reader) -> Result<Handling> {
        let flags = r.u32()?;
        let vehicle_type = r.i16()?;
        let function_modes = [r.i16()?, r.i16()?, r.i16()?, r.i16()?];
        let n: [f32; 14] = r.f32s()?;
        Ok(Handling {
            flags,
            vehicle_type,
            maximum_forward_speed: n[0],
            maximum_reverse_speed: n[1],
            speed_acceleration: n[2],
            speed_deceleration: n[3],
            maximum_left_turn: n[4],
            maximum_right_turn: n[5],
            wheel_circumference: n[6],
            unknown_314: n[7],
            unknown_318: n[8],
            function_modes,
            maximum_left_slide: n[9],
            maximum_right_slide: n[10],
            unknown_340: n[11],
            unknown_344: n[12],
            unknown_364: n[13],
        })
    }
}

impl Physics {
    fn numbers(&self) -> [f32; 20] {
        [
            self.radius,
            self.moment,
            self.mass,
            self.center_of_mass[0],
            self.center_of_mass[1],
            self.center_of_mass[2],
            self.density,
            self.gravity_scale,
            self.ground_friction,
            self.ground_depth,
            self.ground_damp_fraction,
            self.ground_normal_k1,
            self.ground_normal_k0,
            self.water_friction,
            self.water_depth,
            self.water_density,
            self.air_friction,
            self.xx_moment,
            self.yy_moment,
            self.zz_moment,
        ]
    }

    fn write(&self, w: &mut Writer) {
        w.f32s(&self.numbers());
        w.count(self.powered_mass_points.len());
        for p in &self.powered_mass_points {
            w.string(&p.name);
            w.u32(p.flags);
            w.f32s(&[
                p.antigrav_strength,
                p.antigrav_offset,
                p.antigrav_height,
                p.antigrav_damp_fraction,
                p.antigrav_normal_k1,
                p.antigrav_normal_k0,
            ]);
        }
        w.count(self.mass_points.len());
        for m in &self.mass_points {
            w.string(&m.name);
            w.i16(m.powered_mass_point_index);
            w.i16(m.model_node_index);
            w.u32(m.flags);
            w.f32s(&[m.relative_mass, m.mass, m.relative_density, m.density]);
            w.f32s(&m.position);
            w.f32s(&m.forward);
            w.f32s(&m.up);
            w.i16(m.friction_type);
            w.f32s(&[m.friction_parallel_scale, m.friction_perpendicular_scale, m.radius]);
        }
    }

    fn read(r: &mut Reader) -> Result<Physics> {
        let n: [f32; 20] = r.f32s()?;
        let mut powered_mass_points = Vec::new();
        for _ in 0..r.count()? {
            powered_mass_points.push(PoweredMassPoint {
                name: r.string()?,
                flags: r.u32()?,
                antigrav_strength: r.f32()?,
                antigrav_offset: r.f32()?,
                antigrav_height: r.f32()?,
                antigrav_damp_fraction: r.f32()?,
                antigrav_normal_k1: r.f32()?,
                antigrav_normal_k0: r.f32()?,
            });
        }
        let mut mass_points = Vec::new();
        for _ in 0..r.count()? {
            mass_points.push(MassPoint {
                name: r.string()?,
                powered_mass_point_index: r.i16()?,
                model_node_index: r.i16()?,
                flags: r.u32()?,
                relative_mass: r.f32()?,
                mass: r.f32()?,
                relative_density: r.f32()?,
                density: r.f32()?,
                position: r.f32s()?,
                forward: r.f32s()?,
                up: r.f32s()?,
                friction_type: r.i16()?,
                friction_parallel_scale: r.f32()?,
                friction_perpendicular_scale: r.f32()?,
                radius: r.f32()?,
            });
        }
        Ok(Physics {
            radius: n[0],
            moment: n[1],
            mass: n[2],
            center_of_mass: [n[3], n[4], n[5]],
            density: n[6],
            gravity_scale: n[7],
            ground_friction: n[8],
            ground_depth: n[9],
            ground_damp_fraction: n[10],
            ground_normal_k1: n[11],
            ground_normal_k0: n[12],
            water_friction: n[13],
            water_depth: n[14],
            water_density: n[15],
            air_friction: n[16],
            xx_moment: n[17],
            yy_moment: n[18],
            zz_moment: n[19],
            powered_mass_points,
            mass_points,
        })
    }

    /// Whether the numbers can be used and every mass point's powered mass
    /// point is one the body has.
    fn check(&self) -> Result<()> {
        let finite = |vs: &[f32]| vs.iter().all(|v| v.is_finite());
        let powered = |p: &PoweredMassPoint| {
            finite(&[
                p.antigrav_strength,
                p.antigrav_offset,
                p.antigrav_height,
                p.antigrav_damp_fraction,
                p.antigrav_normal_k1,
                p.antigrav_normal_k0,
            ])
        };
        let plain = |m: &MassPoint| {
            finite(&[m.relative_mass, m.mass, m.relative_density, m.density, m.radius])
                && finite(&[m.friction_parallel_scale, m.friction_perpendicular_scale])
                && finite(&m.position)
                && finite(&m.forward)
                && finite(&m.up)
        };
        if !finite(&self.numbers())
            || !self.powered_mass_points.iter().all(powered)
            || !self.mass_points.iter().all(plain)
        {
            return malformed("vehicle data has a physics with a number that is not one");
        }
        let powered_count = self.powered_mass_points.len();
        if self
            .mass_points
            .iter()
            .any(|m| m.powered_mass_point_index < -1 || m.powered_mass_point_index as i32 >= powered_count as i32)
        {
            return malformed("vehicle data has a mass point with a powered mass point the body does not have");
        }
        Ok(())
    }
}

impl Seat {
    fn write(&self, w: &mut Writer) {
        w.u32(self.flags);
        w.string(&self.label);
        w.string(&self.marker_name);
        w.f32s(&self.acceleration_scale);
        w.f32s(&[self.yaw_rate, self.pitch_rate, self.yaw_minimum, self.yaw_maximum]);
    }

    fn read(r: &mut Reader) -> Result<Seat> {
        Ok(Seat {
            flags: r.u32()?,
            label: r.string()?,
            marker_name: r.string()?,
            acceleration_scale: r.f32s()?,
            yaw_rate: r.f32()?,
            pitch_rate: r.f32()?,
            yaw_minimum: r.f32()?,
            yaw_maximum: r.f32()?,
        })
    }

    fn is_finite(&self) -> bool {
        [self.yaw_rate, self.pitch_rate, self.yaw_minimum, self.yaw_maximum]
            .iter()
            .chain(&self.acceleration_scale)
            .all(|v| v.is_finite())
    }
}

impl Vehicles {
    /// `"HCV1"`, the definitions (a `u16` count, each field by field), the
    /// placements and the three damages; little-endian, floats as their
    /// IEEE-754 bits.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = Writer(Vec::new());
        w.0.extend_from_slice(MAGIC);
        w.count(self.defs.len());
        for d in &self.defs {
            w.u16(d.tag_index);
            w.string(&d.name);
            w.u16(d.object_flags);
            w.f32(d.bounding_radius);
            w.f32s(&d.bounding_offset);
            w.u32(d.unit_flags);
            w.f32(d.child_damage_fraction);
            d.handling.write(&mut w);
            match &d.physics {
                None => w.u8(0),
                Some(p) => {
                    w.u8(1);
                    p.write(&mut w);
                }
            }
            w.count(d.seats.len());
            d.seats.iter().for_each(|s| s.write(&mut w));
            w.count(d.weapons.len());
            d.weapons.iter().for_each(|t| w.u16(*t));
            match &d.resistance {
                None => w.u8(0),
                Some(res) => {
                    w.u8(1);
                    res.write_to(&mut w.0);
                }
            }
        }
        w.count(self.placements.len());
        for p in &self.placements {
            // (a tag index is 16 bits, and 0xFFFF is the engine's "none")
            w.u16(p.unwrap_or(u16::MAX));
        }
        for d in [&self.hit_environment_damage, &self.killed_unit_damage, &self.collision_damage] {
            Damage::write_optional_to(d, &mut w.0);
        }
        w.0
    }

    /// Rebuild from [`Vehicles::to_bytes`]; bytes that are cut short, are
    /// followed by more, claim absurd lists, hold a number that is not one or
    /// name a vehicle or a physics index the data does not have are refused.
    pub fn from_bytes(bytes: &[u8]) -> Result<Vehicles> {
        let mut r = Reader(bytes);
        if &r.take::<4>()? != MAGIC {
            return malformed("vehicle data does not start with its signature");
        }
        let mut defs = Vec::new();
        for _ in 0..r.count()? {
            let tag_index = r.u16()?;
            let name = r.string()?;
            let object_flags = r.u16()?;
            let bounding_radius = r.f32()?;
            let bounding_offset = r.f32s()?;
            let unit_flags = r.u32()?;
            let child_damage_fraction = r.f32()?;
            let handling = Handling::read(&mut r)?;
            let physics = match r.u8()? {
                0 => None,
                1 => Some(Physics::read(&mut r)?),
                _ => return malformed("vehicle data has a physics that is neither there nor not"),
            };
            let mut seats = Vec::new();
            for _ in 0..r.count()? {
                seats.push(Seat::read(&mut r)?);
            }
            let mut weapons = Vec::new();
            for _ in 0..r.count()? {
                weapons.push(r.u16()?);
            }
            let resistance = match r.u8()? {
                0 => None,
                1 => Some(Resistance::read_from(&mut r.0)?),
                _ => return malformed("vehicle data has a resistance that is neither there nor not"),
            };
            defs.push(VehicleDef {
                tag_index,
                name,
                object_flags,
                bounding_radius,
                bounding_offset,
                unit_flags,
                child_damage_fraction,
                handling,
                physics,
                seats,
                weapons,
                resistance,
            });
        }
        let mut placements = Vec::new();
        for _ in 0..r.count()? {
            placements.push(match r.u16()? {
                u16::MAX => None,
                tag => Some(tag),
            });
        }
        let hit_environment_damage = Damage::read_optional_from(&mut r.0)?;
        let killed_unit_damage = Damage::read_optional_from(&mut r.0)?;
        let collision_damage = Damage::read_optional_from(&mut r.0)?;
        if !r.0.is_empty() {
            return malformed("vehicle data has bytes after its end");
        }
        let vehicles = Vehicles { defs, placements, hit_environment_damage, killed_unit_damage, collision_damage };
        vehicles.check()?;
        Ok(vehicles)
    }

    /// Whether the numbers can be used and the placements name vehicles the
    /// data has.
    fn check(&self) -> Result<()> {
        let finite = |vs: &[f32]| vs.iter().all(|v| v.is_finite());
        for d in &self.defs {
            let h = &d.handling;
            if !finite(&h.numbers())
                || !finite(&[d.bounding_radius, d.child_damage_fraction])
                || !finite(&d.bounding_offset)
                || !d.seats.iter().all(Seat::is_finite)
            {
                return malformed("vehicle data has a vehicle with a number that is not one");
            }
            if let Some(p) = &d.physics {
                p.check()?;
            }
            if let Some(res) = &d.resistance {
                res.check()?;
            }
        }
        if self.placements.iter().flatten().any(|t| self.def(*t).is_none()) {
            return malformed("vehicle data has a placement of a vehicle it does not have");
        }
        let damages = [&self.hit_environment_damage, &self.killed_unit_damage, &self.collision_damage];
        if damages.iter().any(|d| d.as_ref().is_some_and(|d| !d.is_finite())) {
            return malformed("vehicle data has a damage with a number that is not one");
        }
        Ok(())
    }

    /// Whether every weapon a vehicle carries is one of `combat`'s.
    pub fn check_against(&self, combat: &Combat) -> Result<()> {
        for d in &self.defs {
            if d.weapons.iter().any(|t| !combat.weapons.iter().any(|w| w.tag_index == *t)) {
                return malformed(format!("vehicle {} carries a weapon the map does not have", d.name));
            }
        }
        Ok(())
    }
}
