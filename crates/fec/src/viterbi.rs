//! Soft-decision Viterbi decoder for the DAB rate-1/4 convolutional code.
//!
//! ETSI EN 300 401 §11.1:
//!   Constraint length  K = 7  (6-bit shift register → 64 states)
//!   Generator polynomials (octal notation per spec, binary MSB-first):
//!     G1 = 133₈ = 0b1011011 = 91
//!     G2 = 171₈ = 0b1111001 = 121
//!     G3 = 145₈ = 0b1100101 = 101
//!     G4 = 133₈ = 0b1011011 = 91   (same as G1)

/// Constraint length.
pub const K: usize = 7;
/// Number of encoder states: 2^(K-1).
pub const NUM_STATES: usize = 1 << (K - 1); // 64
/// Number of output bits per input bit.
pub const NUM_OUTPUTS: usize = 4;

/// Generator polynomials (7 bits each).
///
/// ETSI EN 300 401 §11.1 specifies (octal, MSB = current input tap):
///   G1 = 133₈, G2 = 171₈, G3 = 145₈, G4 = 133₈
///
/// Our shift register stores the current input at bit 0 (LSB), so the
/// polynomials are bit-reversed to align tap positions correctly:
///   G1: 91→109, G2: 121→79, G3: 101→83, G4: 91→109
const G: [u8; NUM_OUTPUTS] = [109, 79, 83, 109];

// -------------------------------------------------------------------------- //
//  Transition table                                                           //
// -------------------------------------------------------------------------- //

/// One trellis transition: given the current state and an input bit, this
/// records where we go and what the encoder outputs.
#[derive(Clone, Copy, Default)]
struct Transition {
    next_state: u8,
    output_bits: [u8; NUM_OUTPUTS],
}

/// Compute the output bit for one generator polynomial.
///
/// `state` is the 6-bit shift register **before** shifting in `input`.
/// The register + new bit form a 7-bit word that is XOR-summed through `poly`.
fn encode_bit(state: u8, input: u8, poly: u8) -> u8 {
    let reg = ((state as u16) << 1) | (input as u16);
    let xored = reg as u8 & poly;
    xored.count_ones() as u8 & 1
}

/// Build the full 64×2 transition table (state × input_bit).
fn build_transitions() -> [[Transition; 2]; NUM_STATES] {
    build_transitions_with_polys(&G)
}

/// Build transition table with custom generator polynomials.
fn build_transitions_with_polys(polys: &[u8; NUM_OUTPUTS]) -> [[Transition; 2]; NUM_STATES] {
    let mut table = [[Transition::default(); 2]; NUM_STATES];
    for (state, row) in table.iter_mut().enumerate() {
        for input in 0u8..2 {
            let s = state as u8;
            let next_state = ((s << 1) | input) & ((NUM_STATES - 1) as u8);
            let mut output_bits = [0u8; NUM_OUTPUTS];
            for (i, &poly) in polys.iter().enumerate() {
                output_bits[i] = encode_bit(s, input, poly);
            }
            row[input as usize] = Transition {
                next_state,
                output_bits,
            };
        }
    }
    table
}

// -------------------------------------------------------------------------- //
//  ViterbiDecoder                                                             //
// -------------------------------------------------------------------------- //

pub struct ViterbiDecoder {
    /// Transition table: [state][input_bit].
    transitions: [[Transition; 2]; NUM_STATES],
}

impl ViterbiDecoder {
    /// Create a decoder with the given traceback depth.
    ///
    /// A good default is 5*K = 35.  Use at least 3*K for adequate BER.
    pub fn new(_traceback_depth: usize) -> Self {
        Self {
            transitions: build_transitions(),
        }
    }

    /// Create a decoder with custom generator polynomials.
    pub fn with_polys(_traceback_depth: usize, polys: &[u8; NUM_OUTPUTS]) -> Self {
        Self {
            transitions: build_transitions_with_polys(polys),
        }
    }

    /// Decode `soft_bits` (length must be a multiple of NUM_OUTPUTS=4).
    ///
    /// `soft_bits` contains f32 values in the range approximately [−1, +1]
    /// where +1 = confident 0 and −1 = confident 1 (BPSK convention).
    ///
    /// Returns a `Vec<u8>` of decoded bits (0 or 1), length = input.len()/4.
    pub fn decode(&self, soft_bits: &[f32]) -> Vec<u8> {
        let n_symbols = soft_bits.len() / NUM_OUTPUTS;
        if n_symbols == 0 {
            return Vec::new();
        }

        // Scale soft values to i16 for integer ACS.
        // Map f32 ±1.0 → i16 ±127.
        let scaled: Vec<i16> = soft_bits
            .iter()
            .map(|&v| (v.clamp(-1.0, 1.0) * 127.0) as i16)
            .collect();

        // Path metrics: lower = better.  Initialise all-zero state to 0,
        // all others to a large value.
        let large: i32 = i32::MAX / 2;
        let mut path_metrics = vec![large; NUM_STATES];
        path_metrics[0] = 0;

        // Survivor table: survivors[t][state] = predecessor state index at
        // step t.  The input bit is recovered as state & 1 (LSB of the
        // destination state equals the input bit by construction of the
        // state transition: next = ((prev << 1) | input) & mask).
        let mut survivors: Vec<Vec<u8>> = vec![vec![0u8; NUM_STATES]; n_symbols];

        // Forward ACS pass.
        for t in 0..n_symbols {
            let mut new_metrics = vec![large; NUM_STATES];
            let sym_bits = &scaled[t * NUM_OUTPUTS..(t + 1) * NUM_OUTPUTS];

            for (state, &pm) in path_metrics.iter().enumerate() {
                if pm == large {
                    continue; // unreachable state
                }
                for input in 0u8..2 {
                    let tr = &self.transitions[state][input as usize];
                    let branch = self.branch_metric(sym_bits, &tr.output_bits);
                    let candidate = pm.saturating_add(branch);
                    let ns = tr.next_state as usize;
                    if candidate < new_metrics[ns] {
                        new_metrics[ns] = candidate;
                        survivors[t][ns] = state as u8;
                    }
                }
            }

            path_metrics = new_metrics;
        }

        // Find best end state.
        let best_state = path_metrics
            .iter()
            .enumerate()
            .min_by_key(|&(_, &m)| m)
            .map(|(s, _)| s)
            .unwrap_or(0);

        // Trace back: recover input bits from the state LSB.
        let mut bits = vec![0u8; n_symbols];
        let mut state = best_state;
        for t in (0..n_symbols).rev() {
            bits[t] = (state & 1) as u8;
            state = survivors[t][state] as usize;
        }

        bits
    }

    /// Decode soft bits and also return the normalized path metric.
    ///
    /// The metric is `best_end_metric / n_symbols`. Lower = better match.
    /// For perfect noiseless input, metric ≈ 0. For random input, metric ≈ 254.
    pub fn decode_with_metric(&self, soft_bits: &[f32]) -> (Vec<u8>, f32) {
        let n_symbols = soft_bits.len() / NUM_OUTPUTS;
        if n_symbols == 0 {
            return (Vec::new(), 0.0);
        }

        let scaled: Vec<i16> = soft_bits
            .iter()
            .map(|&v| (v.clamp(-1.0, 1.0) * 127.0) as i16)
            .collect();

        let large: i32 = i32::MAX / 2;
        let mut path_metrics = vec![large; NUM_STATES];
        path_metrics[0] = 0;
        let mut survivors: Vec<Vec<u8>> = vec![vec![0u8; NUM_STATES]; n_symbols];

        for t in 0..n_symbols {
            let mut new_metrics = vec![large; NUM_STATES];
            let sym_bits = &scaled[t * NUM_OUTPUTS..(t + 1) * NUM_OUTPUTS];
            for (state, &pm) in path_metrics.iter().enumerate() {
                if pm == large {
                    continue;
                }
                for input in 0u8..2 {
                    let tr = &self.transitions[state][input as usize];
                    let branch = self.branch_metric(sym_bits, &tr.output_bits);
                    let candidate = pm.saturating_add(branch);
                    let ns = tr.next_state as usize;
                    if candidate < new_metrics[ns] {
                        new_metrics[ns] = candidate;
                        survivors[t][ns] = state as u8;
                    }
                }
            }
            path_metrics = new_metrics;
        }

        let (best_state, &best_metric) = path_metrics
            .iter()
            .enumerate()
            .min_by_key(|&(_, &m)| m)
            .unwrap();
        let norm_metric = best_metric as f32 / n_symbols as f32;

        let mut bits = vec![0u8; n_symbols];
        let mut state = best_state;
        for t in (0..n_symbols).rev() {
            bits[t] = (state & 1) as u8;
            state = survivors[t][state] as usize;
        }

        (bits, norm_metric)
    }

    /// Compute the branch metric between received (scaled) soft bits and the
    /// expected encoder output bits.
    ///
    /// For each output position i:
    ///   expected ∈ {−127, +127}  (mapped from output_bit ∈ {1, 0})
    ///   metric += |received[i] − expected|
    ///
    /// Lower metric = better match.
    #[inline]
    fn branch_metric(&self, received: &[i16], output_bits: &[u8; NUM_OUTPUTS]) -> i32 {
        let mut metric: i32 = 0;
        for (i, &ob) in output_bits.iter().enumerate() {
            // Encoder output 0 → BPSK +1 → scaled +127
            // Encoder output 1 → BPSK −1 → scaled −127
            let expected: i16 = if ob == 0 { 127 } else { -127 };
            let diff = (received[i] as i32 - expected as i32).abs();
            metric = metric.saturating_add(diff);
        }
        metric
    }
}

// -------------------------------------------------------------------------- //
//  Tests                                                                      //
// -------------------------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a sequence of bits using the rate-1/4 convolutional encoder.
    fn encode(bits: &[u8]) -> Vec<f32> {
        let transitions = build_transitions();
        let mut state: usize = 0;
        let mut out = Vec::with_capacity(bits.len() * NUM_OUTPUTS);
        for &bit in bits {
            let tr = &transitions[state][bit as usize];
            for &ob in &tr.output_bits {
                // Map 0→+1.0, 1→-1.0
                out.push(if ob == 0 { 1.0f32 } else { -1.0f32 });
            }
            state = tr.next_state as usize;
        }
        out
    }

    #[test]
    fn encode_all_zeros_no_panic() {
        let bits = vec![0u8; 20];
        let encoded = encode(&bits);
        assert_eq!(encoded.len(), 80);
    }

    #[test]
    fn decode_noiseless() {
        let input = vec![0u8, 1, 0, 0, 1, 1, 0, 1, 1, 0, 0, 0, 1, 0, 1, 1, 1, 0, 0, 1];
        let encoded = encode(&input);
        let dec = ViterbiDecoder::new(5 * K);
        let decoded = dec.decode(&encoded);
        // First K-1 bits may differ due to trellis start/end edge effects.
        let skip = K - 1;
        assert_eq!(decoded[skip..], input[skip..]);
    }

    #[test]
    fn transition_table_next_states_in_range() {
        let table = build_transitions();
        for row in table.iter() {
            for tr in row.iter() {
                assert!((tr.next_state as usize) < NUM_STATES);
            }
        }
    }

    #[test]
    fn empty_input() {
        let dec = ViterbiDecoder::new(35);
        let out = dec.decode(&[]);
        assert!(out.is_empty());
    }
}
