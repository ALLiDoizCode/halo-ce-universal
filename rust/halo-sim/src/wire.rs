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
//! ```
//!
//! all little-endian, 22 bytes a record, floats as their IEEE-754 bits.

use alloc::vec::Vec;

use crate::step::PlayerInput;

/// Bytes per input in a batch.
pub const INPUT_SIZE: usize = 2 + 5 * 4;

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
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_batch_round_trips_bit_for_bit() {
        let inputs = [
            PlayerInput { player: 0, position: [1.5, -2.25, 3.0], yaw: 0.5, pitch: -0.25 },
            PlayerInput { player: 65535, position: [f32::NAN, f32::INFINITY, -0.0], yaw: 1e-30, pitch: f32::MAX },
        ];
        let bytes = encode_inputs(&inputs);
        assert_eq!(bytes.len(), 2 * INPUT_SIZE);
        let back = decode_inputs(&bytes).unwrap();
        // NaN != NaN, so compare the bits
        assert_eq!(encode_inputs(&back), bytes);
        assert_eq!(back[0], inputs[0]);
    }

    #[test]
    fn an_empty_batch_is_no_inputs() {
        assert_eq!(decode_inputs(&[]).unwrap(), []);
    }

    #[test]
    fn a_batch_cut_mid_record_is_refused() {
        let bytes = encode_inputs(&[PlayerInput { player: 1, position: [0.0; 3], yaw: 0.0, pitch: 0.0 }]);
        assert_eq!(decode_inputs(&bytes[..INPUT_SIZE - 1]), Err(BadBatchLength(INPUT_SIZE - 1)));
    }

    #[test]
    fn the_record_layout_is_fixed() {
        let bytes = encode_inputs(&[PlayerInput { player: 0x0102, position: [1.0, 2.0, 3.0], yaw: 4.0, pitch: 5.0 }]);
        assert_eq!(&bytes[..6], [0x02, 0x01, 0, 0, 0x80, 0x3f]);
        assert_eq!(&bytes[18..], 5.0f32.to_le_bytes());
    }
}
