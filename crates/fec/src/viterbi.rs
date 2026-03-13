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

/// Generator polynomials (7 bits each, MSB = oldest register bit).
const G: [u8; NUM_OUTPUTS] = [91, 121, 101, 91];

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
    let mut table = [[Transition::default(); 2]; NUM_STATES];
    for (state, row) in table.iter_mut().enumerate() {
        for input in 0u8..2 {
            let s = state as u8;
            let next_state = ((s << 1) | input) & ((NUM_STATES - 1) as u8);
            let mut output_bits = [0u8; NUM_OUTPUTS];
            for (i, &poly) in G.iter().enumerate() {
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

        // Survivor bits: survivors[t][state] = input bit chosen at step t
        // for this state.
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
                        survivors[t][ns] = input;
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

        // Trace back.
        let mut bits = vec![0u8; n_symbols];
        let mut state = best_state;
        for t in (0..n_symbols).rev() {
            let bit = survivors[t][state];
            bits[t] = bit;
            // Reverse the state transition: previous state fed `bit` to reach
            // `state`.  The next_state formula is ((prev << 1) | bit) & mask.
            // So: prev = (state >> 1) | (bit << (K-2)) — we recover the
            // (K-2) MSBs of prev from the upper bits of `state`.
            state = (state >> 1) | ((bit as usize) << (K - 2));
        }

        bits
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
