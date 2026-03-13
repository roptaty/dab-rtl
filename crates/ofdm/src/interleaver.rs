/// Frequency de-interleaver for DAB Mode I.
///
/// ETSI EN 300 401 §14.6 specifies frequency interleaving using a linear
/// congruential generator (LCG).  The table maps interleaved carrier
/// positions back to their natural (sub-carrier index) order.
use crate::params::{FFT_SIZE, NUM_CARRIERS};

/// Frequency de-interleaver.
///
/// After the FFT the 1 536 active carriers arrive in an interleaved order.
/// `deinterleave` reorders them into the logical sub-carrier sequence that
/// the channel coder expects.
pub struct FreqDeinterleaver {
    /// `table[logical_pos]` = interleaved position to read from.
    table: Vec<usize>,
}

impl FreqDeinterleaver {
    /// Build the de-interleaving permutation table.
    ///
    /// ETSI EN 300 401 §14.6 defines the interleaving permutation using
    /// a linear congruential sequence:
    ///
    ///   π(0) = 0
    ///   π(j) = (13 × π(j−1) + V1) mod T_u   for j = 1 … T_u−1
    ///
    /// For Mode I: T_u = 2048, V1 = 511.
    ///
    /// The LCG visits all 2048 sub-carrier positions in a scrambled order.
    /// We keep only the visits that land on active carriers (|k| ∈ [1, 768]).
    /// The m-th such visit tells us that logical coded bit m was placed at
    /// the corresponding physical carrier position.  The deinterleaver
    /// recovers: out[m] = in[carrier_idx_of(π(visit_m))].
    pub fn new() -> Self {
        const T_U: usize = FFT_SIZE; // 2048
        const V1: usize = 511; // Mode I constant per ETSI
        const CENTER: usize = T_U / 2; // 1024
        const HALF_K: usize = NUM_CARRIERS / 2; // 768
        const LOW: usize = CENTER - HALF_K; // 256
        const HIGH: usize = CENTER + HALF_K; // 1792

        // Generate LCG sequence.
        let mut pi = vec![0usize; T_U];
        for j in 1..T_U {
            pi[j] = (13 * pi[j - 1] + V1) % T_U;
        }

        // Build table: logical_pos → interleaved carrier index (0..1535).
        // The carrier array from the demod is ordered:
        //   index 0..767   = k = −768..−1 (negative frequencies)
        //   index 768..1535 = k = +1..+768 (positive frequencies)
        let mut table = Vec::with_capacity(NUM_CARRIERS);
        for &p in pi.iter().skip(1) {
            if (LOW..=HIGH).contains(&p) && p != CENTER {
                let carrier_idx = if p < CENTER {
                    p - LOW // 0..767 (negative freq carriers)
                } else {
                    p - CENTER - 1 + HALF_K // 768..1535 (positive freq carriers)
                };
                table.push(carrier_idx);
            }
        }

        debug_assert_eq!(table.len(), NUM_CARRIERS);
        Self { table }
    }

    /// Reorder `carriers` (length 1 536) from interleaved to logical order.
    ///
    /// `carriers` must have exactly `NUM_CARRIERS` elements; extra elements
    /// are ignored, missing ones produce a zero-padded output.
    ///
    /// The LCG sequence defines the order in which active carriers are
    /// visited.  `table[logical]` gives the interleaved (physical) index
    /// that carries the `logical`-th coded bit.  To recover coded-bit
    /// order we apply the **forward** permutation:
    ///   out[logical] = carriers[table[logical]]
    pub fn deinterleave(&self, carriers: &[f32]) -> Vec<f32> {
        let mut out = vec![0.0f32; NUM_CARRIERS];
        for (logical, &src) in self.table.iter().enumerate() {
            if src < carriers.len() && logical < NUM_CARRIERS {
                out[logical] = carriers[src];
            }
        }
        out
    }
}

impl Default for FreqDeinterleaver {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_length() {
        let d = FreqDeinterleaver::new();
        assert_eq!(d.table.len(), NUM_CARRIERS);
    }

    #[test]
    fn all_sources_in_range() {
        let d = FreqDeinterleaver::new();
        for &src in &d.table {
            assert!(src < NUM_CARRIERS, "source index {src} out of range");
        }
    }

    #[test]
    fn table_is_permutation() {
        let d = FreqDeinterleaver::new();
        let mut sorted = d.table.clone();
        sorted.sort();
        let expected: Vec<usize> = (0..NUM_CARRIERS).collect();
        assert_eq!(sorted, expected, "table must be a valid permutation");
    }

    #[test]
    fn deinterleave_length() {
        let d = FreqDeinterleaver::new();
        let input = vec![0.0f32; NUM_CARRIERS];
        let out = d.deinterleave(&input);
        assert_eq!(out.len(), NUM_CARRIERS);
    }

    #[test]
    fn deinterleave_preserves_identity_input() {
        let d = FreqDeinterleaver::new();
        // Each carrier gets a unique value.
        let input: Vec<f32> = (0..NUM_CARRIERS as u32).map(|i| i as f32).collect();
        let out = d.deinterleave(&input);
        // All values should be present (just reordered).
        let mut sorted_out: Vec<u32> = out.iter().map(|&v| v as u32).collect();
        sorted_out.sort();
        let expected: Vec<u32> = (0..NUM_CARRIERS as u32).collect();
        assert_eq!(sorted_out, expected);
    }
}
