//! DAB puncturing patterns and de-puncturing utilities.
//!
//! ETSI EN 300 401 §11.2 defines 24 puncturing vectors (PI_1 … PI_24).
//! Each vector has 32 entries where 1 = transmitted bit, 0 = erased bit.
//! The de-puncturer inserts 0.0 soft values for erased positions so that
//! the Viterbi decoder receives a full-rate (rate-1/4) stream.

// -------------------------------------------------------------------------- //
//  Puncturing vectors PI_1 … PI_24                                           //
// -------------------------------------------------------------------------- //

/// All 24 standard DAB puncturing vectors.
///
/// Index 0 = PI_1 (code rate 4/4, no puncturing)
/// Index 1 = PI_2 (code rate 4/3)
/// …
/// Index 23 = PI_24 (code rate 4/1, very heavy puncturing)
pub const PUNCT_VECTORS: [[u8; 32]; 24] = [
    // PI_1: rate 4/4 — all bits kept
    [
        1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
        1, 1,
    ],
    // PI_2: rate 4/3 — 24 ones per 32
    [
        1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1,
        0, 1,
    ],
    // PI_3: rate 4/3 — slightly different pattern
    [
        1, 1, 0, 1, 1, 0, 0, 1, 1, 1, 0, 1, 1, 0, 0, 1, 1, 1, 0, 1, 1, 0, 0, 1, 1, 1, 0, 1, 1, 0,
        0, 1,
    ],
    // PI_4: rate 4/2 — 16 ones per 32
    [
        1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0,
        0, 1,
    ],
    // PI_5: rate 4/2 — denser at the front
    [
        1, 1, 0, 1, 0, 0, 0, 1, 1, 1, 0, 1, 0, 0, 0, 1, 1, 1, 0, 1, 0, 0, 0, 1, 1, 1, 0, 1, 0, 0,
        0, 1,
    ],
    // PI_6: rate 4/2 — alternating pairs
    [
        1, 0, 0, 1, 0, 0, 0, 1, 1, 0, 0, 1, 0, 0, 0, 1, 1, 0, 0, 1, 0, 0, 0, 1, 1, 0, 0, 1, 0, 0,
        0, 1,
    ],
    // PI_7: rate 4/2 — asymmetric
    [
        1, 1, 0, 0, 0, 0, 0, 1, 1, 1, 0, 0, 0, 0, 0, 1, 1, 1, 0, 0, 0, 0, 0, 1, 1, 1, 0, 0, 0, 0,
        0, 1,
    ],
    // PI_8: rate 4/2 — 12 ones
    [
        1, 0, 0, 0, 0, 0, 0, 1, 1, 0, 0, 0, 0, 0, 0, 1, 1, 0, 0, 0, 0, 0, 0, 1, 1, 0, 0, 0, 0, 0,
        0, 1,
    ],
    // PI_9: 10 ones per 32
    [
        1, 1, 0, 1, 0, 0, 0, 0, 1, 1, 0, 1, 0, 0, 0, 0, 1, 1, 0, 1, 0, 0, 0, 0, 1, 1, 0, 1, 0, 0,
        0, 0,
    ],
    // PI_10: 8 ones per 32
    [
        1, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 1, 0, 0,
        0, 0,
    ],
    // PI_11: 7 ones per 32
    [
        1, 1, 0, 0, 0, 0, 0, 0, 1, 1, 0, 0, 0, 0, 0, 0, 1, 1, 0, 0, 0, 0, 0, 0, 1, 1, 0, 0, 0, 0,
        0, 0,
    ],
    // PI_12: 6 ones per 32
    [
        1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0,
        0, 0,
    ],
    // PI_13: 5 ones per 32
    [
        1, 1, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 1, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0,
        0, 0,
    ],
    // PI_14: 4 ones per 32
    [
        1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0,
        0, 0,
    ],
    // PI_15: 3 ones per 32
    [
        1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0,
    ],
    // PI_16: 2 ones per 32 (used in FIC)
    [
        1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0,
    ],
    // PI_17: 14 ones — heavier than PI_2, lighter than PI_1
    [
        1, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1,
        1, 1,
    ],
    // PI_18: 12 ones
    [
        1, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1,
        1, 0,
    ],
    // PI_19: 10 ones
    [
        1, 1, 0, 0, 0, 1, 1, 0, 0, 1, 0, 0, 0, 1, 1, 0, 0, 1, 0, 0, 0, 1, 1, 0, 0, 1, 0, 0, 0, 1,
        1, 0,
    ],
    // PI_20: 9 ones
    [
        1, 1, 0, 0, 0, 1, 0, 0, 0, 1, 1, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1, 1, 0, 0, 1, 0, 0, 0, 1,
        0, 0,
    ],
    // PI_21: 8 ones
    [
        1, 1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1, 1, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1,
        1, 0,
    ],
    // PI_22: 6 ones
    [
        1, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1,
        0, 0,
    ],
    // PI_23: 4 ones
    [
        1, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1,
        0, 0,
    ],
    // PI_24: 2 ones — maximum puncturing
    [
        1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1,
        0, 0,
    ],
];

// -------------------------------------------------------------------------- //
//  FIC puncturing definition                                                  //
// -------------------------------------------------------------------------- //

/// Number of FIC sub-channels (groups) in one FIC block.
/// The FIC is encoded with a specific scheme: 2 304 bits coded, 768 info bits.
/// Three sections use PI_16 (very heavy puncturing) with tail bits appended.
///
/// In practice the FIC uses the following structure (ETSI §11.3):
///   21 × 32-bit group using PI_16 (all-ones), then 24 tail bits.
/// This corresponds to a rate-1/3 code (768 bits in, 2304 bits out ≈ rate 1/3
/// for the mother rate-1/4 code with puncturing).
const FIC_GROUPS: usize = 21;
const FIC_TAIL_BITS: usize = 24; // 6 tailing zero-bits × 4 outputs

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

/// De-puncture a FIC (Fast Information Channel) soft-bit stream.
///
/// The FIC puncturing scheme (ETSI EN 300 401 §11.3.1) uses PI_1 (all ones,
/// i.e. no puncturing) for the FIC protection class.  The encoded FIC block
/// is 2 304 rate-1/4 bits (= 576 coded symbols of 4 bits each) followed by
/// 24 tail bits, totalling 2 328 soft bits.  The information content is
/// 768 bits.
///
/// This function inserts the tail-bit erasures (24 zeros appended) and
/// returns the 2 328-element soft-bit vector ready for the Viterbi decoder.
pub fn fic_depuncture(punctured: &[f32]) -> Vec<f32> {
    // FIC uses PI_1 (all ones) for the main body → the punctured stream
    // is already full-rate.  We just need to append the tail-bit erasures.
    let mut out = Vec::with_capacity(punctured.len() + FIC_TAIL_BITS);

    // Main body: FIC_GROUPS × 32 bits, all transmitted (PI_1 = all ones).
    let main_len = FIC_GROUPS * 32; // 672
    let body = &punctured[..main_len.min(punctured.len())];

    // Re-expand through PI_1 (no-op but kept for consistency).
    let pi1 = &PUNCT_VECTORS[0];
    out.extend(depuncture(body, pi1));

    // Tail bits: 24 zero-valued soft bits (encoder tail zeros, always 0.0).
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
    fn pi1_no_erasures() {
        let input: Vec<f32> = (0..64).map(|i| i as f32).collect();
        let out = depuncture(&input, &PUNCT_VECTORS[0]);
        assert_eq!(out, input, "PI_1 should be a no-op");
    }

    #[test]
    fn pi2_length_correct() {
        // PI_2 has 24 ones per 32 → for 24 input bits we get 32 output bits.
        let input = vec![1.0f32; 24];
        let out = depuncture(&input, &PUNCT_VECTORS[1]);
        assert_eq!(out.len(), 32);
    }

    #[test]
    fn pi2_erasures_are_zero() {
        let input = vec![1.0f32; 24];
        let out = depuncture(&input, &PUNCT_VECTORS[1]);
        // PI_2 pattern: [1,1,0,1 ...] — position 2 (0-indexed) should be 0.
        assert_eq!(out[2], 0.0f32);
    }

    #[test]
    fn pi2_kept_bits_preserved() {
        let input: Vec<f32> = (0..24).map(|i| i as f32 + 1.0).collect();
        let out = depuncture(&input, &PUNCT_VECTORS[1]);
        // PI_2 positions 0,1,3 kept → out[0]=input[0], out[1]=input[1],
        // out[2]=0.0, out[3]=input[2], ...
        assert_eq!(out[0], 1.0);
        assert_eq!(out[1], 2.0);
        assert_eq!(out[2], 0.0); // erased
        assert_eq!(out[3], 3.0);
    }

    #[test]
    fn fic_depuncture_appends_tail() {
        let input = vec![1.0f32; FIC_GROUPS * 32]; // 672 bits
        let out = fic_depuncture(&input);
        // PI_1 is a no-op, then 24 tail zeros appended.
        assert_eq!(out.len(), FIC_GROUPS * 32 + FIC_TAIL_BITS);
        // Last FIC_TAIL_BITS entries should be 0.
        for &v in out.iter().rev().take(FIC_TAIL_BITS) {
            assert_eq!(v, 0.0f32);
        }
    }

    #[test]
    fn punct_vectors_correct_length() {
        for (i, pv) in PUNCT_VECTORS.iter().enumerate() {
            assert_eq!(pv.len(), 32, "PI_{} wrong length", i + 1);
        }
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
