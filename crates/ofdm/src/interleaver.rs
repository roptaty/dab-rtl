/// Frequency de-interleaver for DAB Mode I.
///
/// ETSI EN 300 401 §14.6 specifies a frequency interleaving based on the
/// 11-bit bit-reversal permutation.  The table maps interleaved carrier
/// positions back to their natural (sub-carrier index) order.

use crate::params::NUM_CARRIERS;

/// Reverse the lower `BITS` bits of `v`.
fn bit_reverse_11(v: usize) -> usize {
    const BITS: u32 = 11;
    let mut result = 0usize;
    let mut x = v;
    for _ in 0..BITS {
        result = (result << 1) | (x & 1);
        x >>= 1;
    }
    result
}

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
    /// The interleaving permutation π is defined by:
    ///
    ///   π(i) = bit_reverse_11(i)   for i = 0..2047
    ///
    /// but only the 1 536 entries whose result is in [1, 1536] (after the
    /// +1 offset) are used.  We replicate the welle.io approach:
    ///
    ///   for i in 1..=2047:
    ///     let r = bit_reverse_11(i);
    ///     if r > 0 { table[r - 1] = i - 1; }
    ///
    /// This fills all 1 536 entries of a table indexed 0..1535 (r-1 ranges
    /// over 0..1535 for valid r values after the FFT-size permutation).
    pub fn new() -> Self {
        let mut table = vec![0usize; NUM_CARRIERS];
        for i in 1..2048usize {
            let r = bit_reverse_11(i);
            if r > 0 && (r - 1) < NUM_CARRIERS {
                table[r - 1] = i - 1;
            }
        }
        Self { table }
    }

    /// Reorder `carriers` (length 1 536) from interleaved to logical order.
    ///
    /// `carriers` must have exactly `NUM_CARRIERS` elements; extra elements
    /// are ignored, missing ones produce a zero-padded output.
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
    fn deinterleave_length() {
        let d = FreqDeinterleaver::new();
        let input = vec![0.0f32; NUM_CARRIERS];
        let out = d.deinterleave(&input);
        assert_eq!(out.len(), NUM_CARRIERS);
    }
}
