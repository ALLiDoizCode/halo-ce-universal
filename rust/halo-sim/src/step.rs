use alloc::collections::BTreeSet;
use alloc::vec::Vec;

use crate::map::MapData;
use crate::movement::validate;
use crate::rng::Rng;
use crate::state::{Player, PlayerId, Store, CLIENT_FLAGS, FLAG_AIRBORNE};

/// What one player reports for one tick. Each client decides its own player's
/// movement; the step only accepts or rejects it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerInput {
    pub player: PlayerId,
    /// Where the player says they are now.
    pub position: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
    /// What else the client says about the player: [`crate::FLAG_CROUCHED`], the shot
    /// counter and [`crate::FLAG_RELOADING`] (the others see them: [`CLIENT_FLAGS`]).
    /// Other bits are ignored; whether the player is in the air is the server's to judge.
    pub flags: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// No player has this id.
    UnknownPlayer,
    /// A reported number is NaN or infinite.
    NotFinite,
    /// The move covers more than the speed bound allows in the ticks since the
    /// player's last accepted move (one tick's worth if they moved last tick).
    TooFast,
    /// The straight path from the old position to the new one crosses a surface.
    ThroughSurface,
    /// The new position is not on the ground, and is not where a jump or a
    /// fall that began where the player last stood could have brought them
    /// (see the airborne rule in `halo_sim::step`'s validation).
    OffGround,
    /// The player already had an input this tick; only the first counts, so
    /// that several moves cannot add up to more than the speed bound.
    DuplicateInput,
}

/// What happened during a step, in input order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    MoveAccepted {
        player: PlayerId,
    },
    /// The player stays where they were.
    MoveRejected {
        player: PlayerId,
        reason: RejectReason,
    },
}

/// Advance the state by one tick (1/30 s). Each input is applied in order (a player's second input of the tick is rejected): an
/// accepted move updates the player's position and facing, a rejected one
/// leaves the player untouched. `rng` is not consumed yet; it is part of the
/// interface for the rules that need chance.
pub fn step(store: &mut impl Store, inputs: &[PlayerInput], map: &MapData, _rng: &mut Rng) -> Vec<Event> {
    let mut events = Vec::with_capacity(inputs.len());
    let mut seen = BTreeSet::new();
    for input in inputs {
        if !seen.insert(input.player) {
            events.push(Event::MoveRejected { player: input.player, reason: RejectReason::DuplicateInput });
            continue;
        }
        let Some(player) = store.player(input.player) else {
            events.push(Event::MoveRejected { player: input.player, reason: RejectReason::UnknownPlayer });
            continue;
        };
        match validate(map, &player, input, store.ticks_since_move(input.player)) {
            Ok(air) => {
                let flags = (input.flags & CLIENT_FLAGS) | if air.ticks > 0 { FLAG_AIRBORNE } else { 0 };
                store.set_player(Player {
                    position: input.position,
                    yaw: input.yaw,
                    pitch: input.pitch,
                    flags,
                    air_ticks: air.ticks,
                    air_z: air.z,
                    free_ticks: air.free_ticks,
                    free_z: air.free_z,
                    ..player
                });
                events.push(Event::MoveAccepted { player: input.player });
            }
            Err(reason) => events.push(Event::MoveRejected { player: input.player, reason }),
        }
    }
    events
}
