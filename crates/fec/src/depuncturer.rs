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
//  MSC two-region EEP depuncturing                                            //
// -------------------------------------------------------------------------- //

/// Two-region EEP depuncturing for MSC subchannels (ETSI EN 300 401 §11.3.1).
///
/// The subchannel is split into two regions:
///   - Region 1: `l1` fragments of 128 mother-code bits, punctured with `PUNCT_VECTORS[pi1_idx]`
///   - Region 2: `l2` fragments of 128 mother-code bits, punctured with `PUNCT_VECTORS[pi2_idx]`
///   - Tail: 24 mother-code bits, punctured with PI_X
///
/// Each fragment is 4 repetitions of a 32-element PI vector = 128 positions.
///
/// Use [`eep_a_params`] or [`eep_b_params`] to compute (l1, l2, pi1_idx, pi2_idx)
/// from the subchannel size and protection level.
pub fn msc_eep_depuncture(
    soft: &[f32],
    l1: usize,
    l2: usize,
    pi1_idx: usize,
    pi2_idx: usize,
) -> Vec<f32> {
    let total_mother = (l1 + l2) * 128 + 24;
    let mut out = Vec::with_capacity(total_mother);
    let mut src = 0usize;

    let pi1 = &PUNCT_VECTORS[pi1_idx];
    let pi2 = &PUNCT_VECTORS[pi2_idx];

    // Region 1: L1 fragments × 4 repetitions of PI1.
    for _ in 0..l1 {
        for _ in 0..4 {
            for &keep in pi1.iter() {
                if keep == 1 {
                    out.push(if src < soft.len() { soft[src] } else { 0.0 });
                    src += 1;
                } else {
                    out.push(0.0);
                }
            }
        }
    }

    // Region 2: L2 fragments × 4 repetitions of PI2.
    for _ in 0..l2 {
        for _ in 0..4 {
            for &keep in pi2.iter() {
                if keep == 1 {
                    out.push(if src < soft.len() { soft[src] } else { 0.0 });
                    src += 1;
                } else {
                    out.push(0.0);
                }
            }
        }
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

    out
}

/// Compute two-region EEP-A parameters from subchannel size and protection level.
///
/// Returns `(L1, L2, pi1_idx, pi2_idx)` where pi indices are 0-based into `PUNCT_VECTORS`.
///
/// ETSI EN 300 401 Table 8.  `n = bitRate/8` is derived from the subchannel size
/// and the code rate for each protection level.
pub fn eep_a_params(subchannel_size: u16, level: u8) -> (usize, usize, usize, usize) {
    let size = subchannel_size as usize;

    match level {
        1 => {
            // n = size / 12, L1 = 6n − 3, L2 = 3, PI_24 / PI_23
            let n = size / 12;
            (6 * n - 3, 3, 23, 22)
        }
        2 => {
            // n = size / 8, PI_14 / PI_13
            let n = size / 8;
            if n <= 1 {
                (5 * n.max(1) - 3, n.saturating_sub(3).max(1), 13, 12)
            } else {
                (2 * n - 3, 4 * n + 3, 13, 12)
            }
        }
        3 => {
            // n = size / 6, L1 = 6n − 3, L2 = 3, PI_8 / PI_7
            let n = size / 6;
            (6 * n - 3, 3, 7, 6)
        }
        4 => {
            // n = size × 8 / 31, PI_3 / PI_2
            let n = (size * 8) / 31;
            if n <= 1 {
                (5 * n.max(1) - 3, n.saturating_sub(3).max(1), 2, 1)
            } else {
                (2 * n - 3, 4 * n + 3, 2, 1)
            }
        }
        // Default to level 3 (rate ~1/2)
        _ => {
            let n = size / 6;
            (6 * n.max(1) - 3, 3, 7, 6)
        }
    }
}

/// Compute two-region EEP-B parameters from subchannel size and protection level.
///
/// Returns `(L1, L2, pi1_idx, pi2_idx)` where pi indices are 0-based into `PUNCT_VECTORS`.
///
/// ETSI EN 300 401 Table 9.
pub fn eep_b_params(subchannel_size: u16, level: u8) -> (usize, usize, usize, usize) {
    let size = subchannel_size as usize;

    let (divisor, pi1, pi2) = match level {
        1 => (27, 9, 8), // PI_10 / PI_9
        2 => (21, 5, 4), // PI_6  / PI_5
        3 => (18, 3, 2), // PI_4  / PI_3
        4 => (15, 1, 0), // PI_2  / PI_1
        _ => (18, 3, 2),
    };

    let n = size / divisor;
    let l1 = 24 * n.max(1) - 3;
    let l2 = 3;
    (l1, l2, pi1, pi2)
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

    /// Helper: count ones in a PI vector (4 repetitions of 32).
    fn ones_per_fragment(pi_idx: usize) -> usize {
        4 * PUNCT_VECTORS[pi_idx]
            .iter()
            .map(|&b| b as usize)
            .sum::<usize>()
    }

    /// Verify that EEP-A parameters produce total punctured bits == subchannel × 64.
    #[test]
    fn eep_a_params_match_subchannel_size() {
        // Test with realistic subchannel sizes for each level.
        let cases: &[(u16, u8)] = &[
            // (subchannel_size_CUs, protection_level)
            (96, 1), // 64 kbps at level 1 (n=8)
            (48, 1), // 32 kbps at level 1 (n=4)
            (48, 2), // 48 kbps at level 2 (n=6)
            (24, 3), // 32 kbps at level 3 (n=4)
            (48, 3), // 64 kbps at level 3 (n=8)
        ];
        for &(size, level) in cases {
            let (l1, l2, pi1, pi2) = eep_a_params(size, level);
            let total_punctured = l1 * ones_per_fragment(pi1) + l2 * ones_per_fragment(pi2) + 12;
            assert_eq!(
                total_punctured,
                size as usize * 64,
                "EEP-A level {} size={}: L1={} L2={} PI{}+PI{} → {} bits, expected {}",
                level,
                size,
                l1,
                l2,
                pi1 + 1,
                pi2 + 1,
                total_punctured,
                size as usize * 64
            );
        }
    }

    /// Verify EEP-B parameters.
    #[test]
    fn eep_b_params_match_subchannel_size() {
        let cases: &[(u16, u8)] = &[
            (27, 1),
            (54, 1),
            (21, 2),
            (42, 2),
            (18, 3),
            (36, 3),
            (15, 4),
            (30, 4),
        ];
        for &(size, level) in cases {
            let (l1, l2, pi1, pi2) = eep_b_params(size, level);
            let total_punctured = l1 * ones_per_fragment(pi1) + l2 * ones_per_fragment(pi2) + 12;
            assert_eq!(
                total_punctured,
                size as usize * 64,
                "EEP-B level {} size={}: L1={} L2={} PI{}+PI{} → {} bits, expected {}",
                level,
                size,
                l1,
                l2,
                pi1 + 1,
                pi2 + 1,
                total_punctured,
                size as usize * 64
            );
        }
    }

    /// Verify msc_eep_depuncture output length matches expected mother-code bits.
    #[test]
    fn msc_eep_depuncture_output_length() {
        let (l1, l2, pi1, pi2) = eep_a_params(48, 3); // 64 kbps, level 3
        let total_punctured = l1 * ones_per_fragment(pi1) + l2 * ones_per_fragment(pi2) + 12;
        let input = vec![1.0f32; total_punctured];
        let out = msc_eep_depuncture(&input, l1, l2, pi1, pi2);
        let expected_mother = (l1 + l2) * 128 + 24;
        assert_eq!(out.len(), expected_mother);
    }
}
