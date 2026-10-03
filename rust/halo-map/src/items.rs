//! What the map's tags say of items: the weapons and equipment a player can
//! pick up (their reach, and what a powerup does and for how long), the
//! scenario's netgame equipment (where items appear in a game, how often, and
//! which an item collection picks among), and how far a player reaches. As with
//! [`crate::Movement`] and [`crate::combat`], they are read from the map and
//! never retuned.
//!
//! The names are the tags' own (`source/items/equipment_definitions.h`,
//! `source/items/item_definitions.h`, `source/scenario/scenario_definitions.h`).
//! A weapon's own numbers (its magazines, its flags) are in [`crate::combat`];
//! only what is common to every item is here.
//!
//! [`Items::to_bytes`] and [`Items::from_bytes`] are the byte form a server
//! keeps with the map's other data (see `halo_sim::MapData::to_bytes`).

use crate::error::{malformed, Result};

/// `enum equipment_powerup_type`: what an equipment does when it is picked up.
pub mod powerup {
    pub const NONE: i16 = 0;
    pub const DOUBLE_SPEED: i16 = 1;
    pub const OVERSHIELD: i16 = 2;
    pub const ACTIVE_CAMOUFLAGE: i16 = 3;
    pub const FULL_SPECTRUM_VISION: i16 = 4;
    pub const HEALTH: i16 = 5;
    pub const GRENADE: i16 = 6;
}

/// `scenario_netgame_equipment.flags`: the item is made at rest where it is
/// placed (it does not fall).
pub const PLACEMENT_CREATED_AT_REST: u32 = 1;

/// A weapon or an equipment tag, as far as an item on the ground goes.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemDef {
    /// The tag's index among the map's tags: what the server and the client
    /// name the item by (the same for a weapon as in [`crate::combat::Weapon`]).
    pub tag_index: u16,
    /// A weapon (`weap`), or else an equipment (`eqip`).
    pub is_weapon: bool,
    /// As `name.weapon` or `name.equipment`.
    pub name: String,
    /// The object's bounding sphere, as `struct object_definition` has it:
    /// what a player touches to pick the item up.
    pub bounding_radius: f32,
    pub bounding_offset: [f32; 3],
    /// `item_definition.flags`.
    pub flags: u32,
    /// Equipment only: a [`powerup`] value, and what a grenade it is.
    pub powerup_type: i16,
    pub grenade_type: i16,
    /// Equipment only: seconds a powerup lasts.
    pub powerup_time: f32,
}

/// A netgame equipment spawn (`scenario_netgame_equipment`) with its item
/// collection read.
#[derive(Debug, Clone, PartialEq)]
pub struct Placement {
    pub flags: u32,
    /// `game_type` values.
    pub game_types: [i16; 4],
    /// Seconds between the item's spawns; 0 for the collection's.
    pub spawn_time: i16,
    pub position: [f32; 3],
    /// Radians.
    pub facing: f32,
    /// The collection's own spawn time, in seconds; 0 for none.
    pub collection_spawn_time: i16,
    /// What the collection holds: `(weight, the item's tag index)`.
    pub permutations: Vec<(f32, u16)>,
}

/// How a player reaches for an item: the bounding sphere of the multiplayer
/// player's biped. An item is within reach when the two spheres touch.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reach {
    pub bounding_radius: f32,
    pub bounding_offset: [f32; 3],
}

/// Everything the map says of items.
#[derive(Debug, Clone, PartialEq)]
pub struct Items {
    pub defs: Vec<ItemDef>,
    pub placements: Vec<Placement>,
    pub player: Reach,
}

impl Default for Items {
    /// No items at all.
    fn default() -> Items {
        Items { defs: Vec::new(), placements: Vec::new(), player: Reach { bounding_radius: 0.0, bounding_offset: [0.0; 3] } }
    }
}

impl Items {
    /// The item definition with this tag index.
    pub fn def(&self, tag_index: u16) -> Option<&ItemDef> {
        self.defs.iter().find(|d| d.tag_index == tag_index)
    }
}

// ---------- the byte form

const MAGIC: &[u8; 4] = b"HCI1";
/// The most elements any list of the byte form may claim.
const MAX_LIST: usize = 4096;

struct Writer(Vec<u8>);

impl Writer {
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
    fn count(&mut self, n: usize) {
        self.u16(n as u16);
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
            None => malformed("item data is cut short"),
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
            return malformed(format!("item data has a list of {n}"));
        }
        Ok(n)
    }
}

impl Items {
    /// `"HCI1"`, the reach of the player, the definitions (a `u16` count, each
    /// field by field) and the placements; little-endian, floats as their
    /// IEEE-754 bits.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = Writer(Vec::new());
        w.0.extend_from_slice(MAGIC);
        w.f32(self.player.bounding_radius);
        self.player.bounding_offset.iter().for_each(|v| w.f32(*v));
        w.count(self.defs.len());
        for d in &self.defs {
            w.u16(d.tag_index);
            w.0.push(d.is_weapon as u8);
            w.count(d.name.len());
            w.0.extend_from_slice(d.name.as_bytes());
            w.f32(d.bounding_radius);
            d.bounding_offset.iter().for_each(|v| w.f32(*v));
            w.u32(d.flags);
            w.i16(d.powerup_type);
            w.i16(d.grenade_type);
            w.f32(d.powerup_time);
        }
        w.count(self.placements.len());
        for p in &self.placements {
            w.u32(p.flags);
            p.game_types.iter().for_each(|t| w.i16(*t));
            w.i16(p.spawn_time);
            p.position.iter().for_each(|v| w.f32(*v));
            w.f32(p.facing);
            w.i16(p.collection_spawn_time);
            w.count(p.permutations.len());
            for (weight, tag) in &p.permutations {
                w.f32(*weight);
                w.u16(*tag);
            }
        }
        w.0
    }

    /// Rebuild from [`Items::to_bytes`]; bytes that are cut short, are
    /// followed by more, claim absurd lists or hold a number that is not one
    /// are refused.
    pub fn from_bytes(bytes: &[u8]) -> Result<Items> {
        let mut r = Reader(bytes);
        if &r.take::<4>()? != MAGIC {
            return malformed("item data does not start with its signature");
        }
        let player = Reach { bounding_radius: r.f32()?, bounding_offset: r.f32s()? };
        let mut defs = Vec::new();
        for _ in 0..r.count()? {
            let tag_index = r.u16()?;
            let is_weapon = r.u8()? != 0;
            let name_len = r.count()?;
            let Some((name, rest)) = r.0.split_at_checked(name_len) else {
                return malformed("item data is cut short");
            };
            let name = String::from_utf8_lossy(name).into_owned();
            r.0 = rest;
            defs.push(ItemDef {
                tag_index,
                is_weapon,
                name,
                bounding_radius: r.f32()?,
                bounding_offset: r.f32s()?,
                flags: r.u32()?,
                powerup_type: r.i16()?,
                grenade_type: r.i16()?,
                powerup_time: r.f32()?,
            });
        }
        let mut placements = Vec::new();
        for _ in 0..r.count()? {
            let flags = r.u32()?;
            let game_types = [r.i16()?, r.i16()?, r.i16()?, r.i16()?];
            let spawn_time = r.i16()?;
            let position = r.f32s()?;
            let facing = r.f32()?;
            let collection_spawn_time = r.i16()?;
            let mut permutations = Vec::new();
            for _ in 0..r.count()? {
                permutations.push((r.f32()?, r.u16()?));
            }
            placements.push(Placement {
                flags,
                game_types,
                spawn_time,
                position,
                facing,
                collection_spawn_time,
                permutations,
            });
        }
        if !r.0.is_empty() {
            return malformed("item data has bytes after its end");
        }
        let items = Items { defs, placements, player };
        items.check()?;
        Ok(items)
    }

    /// Whether the numbers can be used: finite, with weights that are not
    /// negative, and an item collection that names items the map has.
    fn check(&self) -> Result<()> {
        let finite = |vs: &[f32]| vs.iter().all(|v| v.is_finite());
        if !finite(&[self.player.bounding_radius]) || !finite(&self.player.bounding_offset) {
            return malformed("item data has a player reach that is not a number");
        }
        for d in &self.defs {
            if !finite(&[d.bounding_radius, d.powerup_time]) || !finite(&d.bounding_offset) {
                return malformed("item data has an item with a number that is not one");
            }
        }
        for p in &self.placements {
            if !finite(&p.position) || !finite(&[p.facing]) {
                return malformed("item data has a placement that is not a place");
            }
            if p.permutations.iter().any(|(w, tag)| !w.is_finite() || *w < 0.0 || self.def(*tag).is_none()) {
                return malformed("item data has a collection of items the map does not have");
            }
        }
        Ok(())
    }
}
