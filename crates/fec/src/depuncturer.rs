//! DAB puncturing patterns and de-puncturing utilities.
//!
//! ETSI EN 300 401 §11.2 defines 24 puncturing vectors (PI_1 … PI_24).
//! Each vector has 32 entries where 1 = transmitted bit, 0 = erased bit.
//! The de-puncturer inserts 0.0 soft values for erased positions so that
//! the Viterbi decoder receives a full-rate (rate-1/4) stream.

// -------------------------------------------------------------------------- //
//  Puncturing vectors PI_1 … PI_24                                           //
// -------------------------------------------------------------------------- //

/// All 24 standard DAB puncturing vectors per ETSI EN 300 401 Table 31.
///
/// Index 0 = PI_1 (9 ones — heaviest puncturing)
/// …
/// Index 23 = PI_24 (32 ones — no puncturing)
///
/// Higher PI number = more bits kept = less puncturing = lower code rate.
pub const PUNCT_VECTORS: [[u8; 32]; 24] = [
    // PI_1: 9 ones per 32
    [
        1, 1, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1, 0,
        0, 0,
    ],
    // PI_2: 10 ones per 32
    [
        1, 1, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1, 1, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1, 0,
        0, 0,
    ],
    // PI_3: 11 ones per 32
    [
        1, 1, 0, 0, 1, 0, 0, 0, 1, 1, 0, 0, 1, 0, 0, 0, 1, 1, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1, 0,
        0, 0,
    ],
    // PI_4: 12 ones per 32
    [
        1, 1, 0, 0, 1, 0, 0, 0, 1, 1, 0, 0, 1, 0, 0, 0, 1, 1, 0, 0, 1, 0, 0, 0, 1, 1, 0, 0, 1, 0,
        0, 0,
    ],
    // PI_5: 13 ones per 32
    [
        1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 0, 0, 0, 1, 1, 0, 0, 1, 0, 0, 0, 1, 1, 0, 0, 1, 0,
        0, 0,
    ],
    // PI_6: 14 ones per 32
    [
        1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 0, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 0,
        0, 0,
    ],
    // PI_7: 15 ones per 32
    [
        1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 0,
        0, 0,
    ],
    // PI_8: 16 ones per 32
    [
        1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1,
        0, 0,
    ],
    // PI_9: 17 ones per 32
    [
        1, 1, 1, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1,
        0, 0,
    ],
    // PI_10: 18 ones per 32
    [
        1, 1, 1, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 1, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1,
        0, 0,
    ],
    // PI_11: 19 ones per 32
    [
        1, 1, 1, 0, 1, 1, 0, 0, 1, 1, 1, 0, 1, 1, 0, 0, 1, 1, 1, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1,
        0, 0,
    ],
    // PI_12: 20 ones per 32
    [
        1, 1, 1, 0, 1, 1, 0, 0, 1, 1, 1, 0, 1, 1, 0, 0, 1, 1, 1, 0, 1, 1, 0, 0, 1, 1, 1, 0, 1, 1,
        0, 0,
    ],
    // PI_13: 21 ones per 32
    [
        1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 0, 0, 1, 1, 1, 0, 1, 1, 0, 0, 1, 1, 1, 0, 1, 1,
        0, 0,
    ],
    // PI_14: 22 ones per 32
    [
        1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 0, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1,
        0, 0,
    ],
    // PI_15: 23 ones per 32
    [
        1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1,
        0, 0,
    ],
    // PI_16: 24 ones per 32
    [
        1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1,
        1, 0,
    ],
    // PI_17: 25 ones per 32
    [
        1, 1, 1, 1, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1,
        1, 0,
    ],
    // PI_18: 26 ones per 32
    [
        1, 1, 1, 1, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 1, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1,
        1, 0,
    ],
    // PI_19: 27 ones per 32
    [
        1, 1, 1, 1, 1, 1, 1, 0, 1, 1, 1, 1, 1, 1, 1, 0, 1, 1, 1, 1, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1,
        1, 0,
    ],
    // PI_20: 28 ones per 32
    [
        1, 1, 1, 1, 1, 1, 1, 0, 1, 1, 1, 1, 1, 1, 1, 0, 1, 1, 1, 1, 1, 1, 1, 0, 1, 1, 1, 1, 1, 1,
        1, 0,
    ],
    // PI_21: 29 ones per 32
    [
        1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 1, 1, 1, 1, 1, 1, 1, 0, 1, 1, 1, 1, 1, 1,
        1, 0,
    ],
    // PI_22: 30 ones per 32
    [
        1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
        1, 0,
    ],
    // PI_23: 31 ones per 32
    [
        1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
        1, 0,
    ],
    // PI_24: 32 ones per 32 — no puncturing
    [
        1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
        1, 1,
    ],
];

// -------------------------------------------------------------------------- //
//  FIC puncturing definition                                                  //
// -------------------------------------------------------------------------- //

// -------------------------------------------------------------------------- //
//  Public API                                                                 //
// -------------------------------------------------------------------------- //

/// De-puncture a block of soft bits using the given pattern.
///
/// `punctured` contains only the transmitted (non-erased) soft bits.
/// The function cycles through `pattern` and inserts `0.0` wherever
/// `pattern[i % 32] == 0`.
///
/// The output length is:
///   `punctured.len() / ones_in_pattern * 32`
pub fn depuncture(punctured: &[f32], pattern: &[u8; 32]) -> Vec<f32> {
    let ones: usize = pattern.iter().map(|&b| b as usize).sum();
    if ones == 0 {
        // Pathological: pattern has no kept bits; nothing to output.
        return Vec::new();
    }

    // Number of complete 32-bit pattern cycles needed.
    // We expand until all punctured bits are consumed.
    let mut out = Vec::with_capacity(punctured.len() * 32 / ones.max(1) + 32);
    let mut src_idx = 0usize;

    loop {
        for &keep in pattern.iter() {
            if keep == 1 {
                if src_idx >= punctured.len() {
                    return out;
                }
                out.push(punctured[src_idx]);
                src_idx += 1;
            } else {
                out.push(0.0f32); // erasure
            }
        }
        if src_idx >= punctured.len() {
            break;
        }
    }

    out
}

/// Tail-bits puncturing pattern for FIC (PI_X).
///
/// ETSI EN 300 401 §11.1.2: the 24 tail coded bits use this pattern
/// (12 ones out of 24).
const PI_X: [u8; 24] = [
    1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0,
];

/// Number of punctured soft bits per FIC block.
pub const FIC_PUNCTURED_BITS: usize = 2304;

/// De-puncture a FIC (Fast Information Channel) block for Viterbi decoding.
///
/// ETSI EN 300 401 §11.1.2, Table 30:
///   - 21 blocks of PI_16 (24/32 ones, repeated 4× = 128 positions per block)
///   - 3 blocks of PI_15  (23/32 ones, repeated 4× = 128 positions per block)
///   - 24 tail positions with PI_X (12/24 ones)
///
/// Input:  2 304 punctured soft bits (one FIC block).
/// Output: 3 096 mother-code soft bits ready for Viterbi.
pub fn fic_depuncture(soft: &[f32]) -> Vec<f32> {
    let pi_16 = &PUNCT_VECTORS[15]; // PI_16 = index 15 (0-based)
    let pi_15 = &PUNCT_VECTORS[14]; // PI_15 = index 14

    let mut out = Vec::with_capacity(3096);
    let mut src = 0usize;

    // Helper: apply one block (4 repetitions of a 32-element pattern).
    let apply_block = |pattern: &[u8; 32], out: &mut Vec<f32>, src: &mut usize| {
        for _ in 0..4 {
            for &keep in pattern.iter() {
                if keep == 1 {
                    out.push(if *src < soft.len() { soft[*src] } else { 0.0 });
                    *src += 1;
                } else {
                    out.push(0.0);
                }
            }
        }
    };

    // 21 blocks of PI_16.
    for _ in 0..21 {
        apply_block(pi_16, &mut out, &mut src);
    }

    // 3 blocks of PI_15.
    for _ in 0..3 {
        apply_block(pi_15, &mut out, &mut src);
    }

    // Tail: 24 positions with PI_X.
    for &keep in PI_X.iter() {
        if keep == 1 {
            out.push(if src < soft.len() { soft[src] } else { 0.0 });
            src += 1;
        } else {
            out.push(0.0);
        }
    }

    debug_assert_eq!(out.len(), 3096);
    out
}

// -------------------------------------------------------------------------- //
//  Tests                                                                      //
// -------------------------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pi24_no_erasures() {
        // PI_24 (index 23) = all ones = no puncturing.
        let input: Vec<f32> = (0..64).map(|i| i as f32).collect();
        let out = depuncture(&input, &PUNCT_VECTORS[23]);
        assert_eq!(out, input, "PI_24 should be a no-op");
    }

    #[test]
    fn pi1_length_correct() {
        // PI_1 has 9 ones per 32 → 9 input bits produce 32 output bits.
        let input = vec![1.0f32; 9];
        let out = depuncture(&input, &PUNCT_VECTORS[0]);
        assert_eq!(out.len(), 32);
    }

    #[test]
    fn pi1_erasures_are_zero() {
        // PI_1: [1,1,0,0, 1,0,0,0, ...] — position 2 should be erased.
        let input = vec![1.0f32; 9];
        let out = depuncture(&input, &PUNCT_VECTORS[0]);
        assert_eq!(out[2], 0.0f32);
        assert_eq!(out[3], 0.0f32);
    }

    #[test]
    fn pi1_kept_bits_preserved() {
        let input: Vec<f32> = (0..9).map(|i| i as f32 + 1.0).collect();
        let out = depuncture(&input, &PUNCT_VECTORS[0]);
        // PI_1 positions 0,1 kept, 2,3 erased, 4 kept, 5,6,7 erased, 8 kept, ...
        assert_eq!(out[0], 1.0);
        assert_eq!(out[1], 2.0);
        assert_eq!(out[2], 0.0); // erased
        assert_eq!(out[3], 0.0); // erased
        assert_eq!(out[4], 3.0);
    }

    #[test]
    fn fic_depuncture_correct_size() {
        let input = vec![1.0f32; FIC_PUNCTURED_BITS]; // one FIC block (2304 bits)
        let out = fic_depuncture(&input);
        assert_eq!(out.len(), 3096, "depunctured FIC block should be 3096 bits");
    }

    #[test]
    fn fic_depuncture_erasures_are_zero() {
        let input = vec![1.0f32; FIC_PUNCTURED_BITS];
        let out = fic_depuncture(&input);
        // Every erased position (pattern=0) should be 0.0.
        let erased_count = out.iter().filter(|&&v| v == 0.0).count();
        // 3096 - 2304 = 792 erasures.
        assert_eq!(erased_count, 3096 - FIC_PUNCTURED_BITS);
    }

    #[test]
    fn fic_depuncture_kept_bits_nonzero() {
        let input: Vec<f32> = (1..=FIC_PUNCTURED_BITS as i32).map(|i| i as f32).collect();
        let out = fic_depuncture(&input);
        let kept_count = out.iter().filter(|&&v| v != 0.0).count();
        assert_eq!(kept_count, FIC_PUNCTURED_BITS);
    }

    #[test]
    fn punct_vectors_correct_length() {
        for (i, pv) in PUNCT_VECTORS.iter().enumerate() {
            assert_eq!(pv.len(), 32, "PI_{} wrong length", i + 1);
        }
    }

    #[test]
    fn punct_vectors_ones_monotonic() {
        // ETSI PI_1..PI_24: ones per 32 increase monotonically (9..32).
        let mut prev_ones = 0usize;
        for (i, pv) in PUNCT_VECTORS.iter().enumerate() {
            let ones: usize = pv.iter().map(|&b| b as usize).sum();
            assert!(
                ones > prev_ones,
                "PI_{} has {} ones, expected more than {}",
                i + 1,
                ones,
                prev_ones
            );
            prev_ones = ones;
        }
        // PI_24 should have all 32 ones.
        assert_eq!(prev_ones, 32);
    }

    #[test]
    fn punct_vectors_only_zero_one() {
        for (i, pv) in PUNCT_VECTORS.iter().enumerate() {
            for &b in pv.iter() {
                assert!(b == 0 || b == 1, "PI_{} contains invalid value {b}", i + 1);
            }
        }
    }
}
