//! The byte form of a tick's inputs: what the gateway hands the match module
//! as one batch, and what the module decodes before the step.
//!
//! A batch is a plain concatenation of fixed-size records, so it needs no
//! header and a gateway can build it with one pass over its players:
//!
//! ```text
//! player  u16
//! x, y, z f32 x 3
//! yaw     f32
//! pitch   f32
//! flags   u8     bit 1: the player is crouched (halo_sim::FLAG_CROUCHED)
//! ```
//!
//! all little-endian, 23 bytes a record, floats as their IEEE-754 bits.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::step::PlayerInput;

/// Bytes per input in a batch.
pub const INPUT_SIZE: usize = 2 + 5 * 4 + 1;

/// A batch whose length is not a whole number of records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BadBatchLength(pub usize);

pub fn encode_inputs(inputs: &[PlayerInput]) -> Vec<u8> {
    let mut out = Vec::with_capacity(inputs.len() * INPUT_SIZE);
    for input in inputs {
        out.extend_from_slice(&input.player.to_le_bytes());
        for v in input.position.iter().chain([&input.yaw, &input.pitch]) {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.push(input.flags);
    }
    out
}

pub fn decode_inputs(batch: &[u8]) -> Result<Vec<PlayerInput>, BadBatchLength> {
    if !batch.len().is_multiple_of(INPUT_SIZE) {
        return Err(BadBatchLength(batch.len()));
    }
    Ok(batch
        .as_chunks::<INPUT_SIZE>()
        .0
        .iter()
        .map(|record| {
            let f = |i: usize| f32::from_le_bytes(record[2 + 4 * i..6 + 4 * i].try_into().unwrap());
            PlayerInput {
                player: u16::from_le_bytes([record[0], record[1]]),
                position: [f(0), f(1), f(2)],
                yaw: f(3),
                pitch: f(4),
                flags: record[22],
            }
        })
        .collect())
}

/// Bytes per hit report in a batch of them (see [`encode_hits`]).
pub const HIT_SIZE: usize = 2 + 2 + 2 + 4 + 6 * 4;

/// A batch of hit reports (a client's, which the server's `report_hits` takes
/// over the client's own, reliable connection: see `halo_wire`'s documentation
/// of where hit reports go and why), as fixed-size records:
///
/// ```text
/// target          u16
/// weapon          u16   the weapon's tag index
/// material        i16   the part of the target that was hit, -1 for none
/// host_tick       u32   the server tick the client had last heard of
/// origin          f32 x 3   where the shot hit
/// target_position f32 x 3   where the shooter saw the target
/// ```
///
/// all little-endian, 34 bytes a record, floats as their IEEE-754 bits.
pub fn encode_hits(hits: &[crate::combat::HitReport]) -> Vec<u8> {
    let mut out = Vec::with_capacity(hits.len() * HIT_SIZE);
    for h in hits {
        out.extend_from_slice(&h.target.to_le_bytes());
        out.extend_from_slice(&h.weapon.to_le_bytes());
        out.extend_from_slice(&h.material.to_le_bytes());
        out.extend_from_slice(&h.host_tick.to_le_bytes());
        for v in h.origin.iter().chain(&h.target_position) {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out
}

pub fn decode_hits(batch: &[u8]) -> Result<Vec<crate::combat::HitReport>, BadBatchLength> {
    if !batch.len().is_multiple_of(HIT_SIZE) {
        return Err(BadBatchLength(batch.len()));
    }
    Ok(batch
        .as_chunks::<HIT_SIZE>()
        .0
        .iter()
        .map(|r| {
            let f = |i: usize| f32::from_le_bytes(r[10 + 4 * i..14 + 4 * i].try_into().unwrap());
            crate::combat::HitReport {
                target: u16::from_le_bytes([r[0], r[1]]),
                weapon: u16::from_le_bytes([r[2], r[3]]),
                material: i16::from_le_bytes([r[4], r[5]]),
                host_tick: u32::from_le_bytes([r[6], r[7], r[8], r[9]]),
                origin: [f(0), f(1), f(2)],
                target_position: [f(3), f(4), f(5)],
            }
        })
        .collect())
}

/// The inputs of the batches that waited for this tick (oldest first) as one
/// list. A tick that ran late finds several batches, each with the player's
/// input of its own gateway tick: the newest says where the player is now, and
/// the step judges the move since their last accepted one over the ticks that
/// passed, so the older are dropped, not refused as a second input. (Two
/// inputs of one player *in one batch* are left for the step to refuse.)
pub fn collapse_batches(batches: Vec<Vec<PlayerInput>>) -> Vec<PlayerInput> {
    let mut inputs: Vec<PlayerInput> = Vec::new();
    // where each player's input from an earlier batch is in `inputs`
    let mut earlier: BTreeMap<crate::state::PlayerId, usize> = BTreeMap::new();
    for batch in batches {
        let mut this_batch = BTreeMap::new();
        for input in batch {
            match earlier.get(&input.player) {
                Some(&at) if !this_batch.contains_key(&input.player) => {
                    inputs[at] = input;
                    this_batch.insert(input.player, at);
                }
                _ => {
                    this_batch.entry(input.player).or_insert(inputs.len());
                    inputs.push(input);
                }
            }
        }
        earlier.extend(this_batch);
    }
    inputs
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    #[test]
    fn a_batch_round_trips_bit_for_bit() {
        let inputs = [
            PlayerInput { player: 0, position: [1.5, -2.25, 3.0], yaw: 0.5, pitch: -0.25, flags: 0 },
            PlayerInput {
                player: 65535,
                position: [f32::NAN, f32::INFINITY, -0.0],
                yaw: 1e-30,
                pitch: f32::MAX,
                flags: 0,
            },
        ];
        let bytes = encode_inputs(&inputs);
        assert_eq!(bytes.len(), 2 * INPUT_SIZE);
        let back = decode_inputs(&bytes).unwrap();
        // NaN != NaN, so compare the bits
        assert_eq!(encode_inputs(&back), bytes);
        assert_eq!(back[0], inputs[0]);
    }

    fn at(player: u16, x: f32) -> PlayerInput {
        PlayerInput { player, position: [x, 0.0, 0.0], yaw: 0.0, pitch: 0.0, flags: 0 }
    }

    #[test]
    fn a_tick_that_found_several_batches_takes_each_players_newest_input() {
        let inputs =
            collapse_batches(vec![vec![at(1, 1.0), at(2, 1.0)], vec![at(2, 2.0), at(3, 2.0)], vec![at(1, 3.0)]]);
        let got: Vec<(u16, f32)> = inputs.iter().map(|i| (i.player, i.position[0])).collect();
        assert_eq!(got, [(1, 3.0), (2, 2.0), (3, 2.0)]);
    }

    #[test]
    fn two_inputs_of_a_player_in_one_batch_are_both_kept_for_the_step_to_refuse() {
        let inputs = collapse_batches(vec![vec![at(1, 1.0), at(1, 2.0)]]);
        assert_eq!(inputs.len(), 2);
    }

    #[test]
    fn hit_reports_round_trip_bit_for_bit_and_have_a_fixed_layout() {
        let hits = [
            crate::combat::HitReport {
                target: 0x0102,
                weapon: 476,
                material: -1,
                host_tick: 0x0A0B0C0D,
                origin: [1.0, -2.5, 3.25],
                target_position: [f32::NAN, 0.0, -0.0],
            },
            crate::combat::HitReport {
                target: 7,
                weapon: 1,
                material: 3,
                host_tick: 5,
                origin: [0.0; 3],
                target_position: [9.0; 3],
            },
        ];
        let bytes = encode_hits(&hits);
        assert_eq!(bytes.len(), 2 * HIT_SIZE);
        assert_eq!(HIT_SIZE, 34);
        assert_eq!(&bytes[..10], [0x02, 0x01, 0xDC, 0x01, 0xFF, 0xFF, 0x0D, 0x0C, 0x0B, 0x0A]);
        assert_eq!(&bytes[10..14], 1.0f32.to_le_bytes());
        let back = decode_hits(&bytes).unwrap();
        assert_eq!(encode_hits(&back), bytes);
        assert_eq!(back[1], hits[1]);
        assert_eq!(decode_hits(&bytes[..HIT_SIZE - 1]), Err(BadBatchLength(HIT_SIZE - 1)));
        assert_eq!(decode_hits(&[]).unwrap(), []);
    }

    #[test]
    fn an_empty_batch_is_no_inputs() {
        assert_eq!(decode_inputs(&[]).unwrap(), []);
    }

    #[test]
    fn a_batch_cut_mid_record_is_refused() {
        let bytes = encode_inputs(&[PlayerInput { player: 1, position: [0.0; 3], yaw: 0.0, pitch: 0.0, flags: 0 }]);
        assert_eq!(decode_inputs(&bytes[..INPUT_SIZE - 1]), Err(BadBatchLength(INPUT_SIZE - 1)));
    }

    #[test]
    fn the_record_layout_is_fixed() {
        let bytes =
            encode_inputs(&[PlayerInput { player: 0x0102, position: [1.0, 2.0, 3.0], yaw: 4.0, pitch: 5.0, flags: 2 }]);
        assert_eq!(&bytes[..6], [0x02, 0x01, 0, 0, 0x80, 0x3f]);
        assert_eq!(&bytes[18..22], 5.0f32.to_le_bytes());
        assert_eq!(bytes[22], 2, "the flags come last");
    }
}
