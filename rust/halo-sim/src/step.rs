use alloc::vec::Vec;

use crate::map::MapData;
use crate::movement::validate;
use crate::rng::Rng;
use crate::state::{Player, PlayerId, Store};

/// What one player reports for one tick. Each client decides its own player's
/// movement; the step only accepts or rejects it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerInput {
    pub player: PlayerId,
    /// Where the player says they are now.
    pub position: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// No player has this id.
    UnknownPlayer,
    /// A reported number is NaN or infinite.
    NotFinite,
    /// The move covers more than the speed bound allows in a tick.
    TooFast,
    /// The straight path from the old position to the new one crosses a surface.
    ThroughSurface,
    /// The new position is not on the ground.
    OffGround,
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

/// Advance the state by one tick (1/30 s). Each input is applied in order: an
/// accepted move updates the player's position and facing, a rejected one
/// leaves the player untouched. `rng` is not consumed yet; it is part of the
/// interface for the rules that need chance.
pub fn step(store: &mut impl Store, inputs: &[PlayerInput], map: &MapData, _rng: &mut Rng) -> Vec<Event> {
    let mut events = Vec::with_capacity(inputs.len());
    for input in inputs {
        let Some(player) = store.player(input.player) else {
            events.push(Event::MoveRejected { player: input.player, reason: RejectReason::UnknownPlayer });
            continue;
        };
        match validate(map, player.position, input) {
            Ok(()) => {
                store.set_player(Player { position: input.position, yaw: input.yaw, pitch: input.pitch, ..player });
                events.push(Event::MoveAccepted { player: input.player });
            }
            Err(reason) => events.push(Event::MoveRejected { player: input.player, reason }),
        }
    }
    events
}
