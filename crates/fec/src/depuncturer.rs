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

/// Tail bits appended to the FIC encoded stream (6 encoder-flush zeros × 4
/// generator polynomials).
const FIC_TAIL_BITS: usize = 24;

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

/// Prepare a FIC (Fast Information Channel) soft-bit stream for Viterbi
/// decoding.
///
/// Each FIC symbol carries 3 072 soft bits at the mother-code rate of 1/4
/// (essentially unpunctured).  These encode 768 information bits + 6 tail
/// bits = 774 input bits × 4 = 3 096 coded bits.  The last 24 coded bits
/// (the tail) are not transmitted, so we append 24 zero-valued erasures
/// to reconstruct the full 3 096-element input for the Viterbi decoder.
pub fn fic_depuncture(soft: &[f32]) -> Vec<f32> {
    let mut out = Vec::with_capacity(soft.len() + FIC_TAIL_BITS);
    out.extend_from_slice(soft);
    out.resize(out.len() + FIC_TAIL_BITS, 0.0f32);
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
    fn fic_depuncture_appends_tail() {
        let input = vec![1.0f32; 3072]; // one FIC symbol
        let out = fic_depuncture(&input);
        // 3072 soft bits + 24 tail erasures = 3096.
        assert_eq!(out.len(), 3072 + FIC_TAIL_BITS);
        // Last FIC_TAIL_BITS entries should be 0.
        for &v in out.iter().rev().take(FIC_TAIL_BITS) {
            assert_eq!(v, 0.0f32);
        }
        // Original data preserved.
        assert!(out[..3072].iter().all(|&v| v == 1.0));
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
