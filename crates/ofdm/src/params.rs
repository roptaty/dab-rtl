//! DAB Transmission Mode I constants per ETSI EN 300 401.

/// Sample rate: 2.048 MHz
pub const SAMPLE_RATE: u32 = 2_048_000;

/// OFDM FFT size (number of sub-carriers + DC)
pub const FFT_SIZE: usize = 2048;

/// Cyclic prefix (guard interval) length in samples
pub const GUARD_SIZE: usize = 504;

/// Total OFDM symbol duration: guard + FFT window
pub const SYMBOL_SIZE: usize = FFT_SIZE + GUARD_SIZE; // 2552

/// Null symbol duration in samples
pub const NULL_SIZE: usize = 2656;

/// Number of OFDM symbols per DAB frame (1 phase-reference + 75 data symbols)
pub const FRAME_SYMBOLS: usize = 76;

/// Number of active sub-carriers (bins -768..-1 and +1..+768, DC excluded)
pub const NUM_CARRIERS: usize = 1536;

/// Lowest sub-carrier index (inclusive)
pub const CARRIER_MIN: i32 = -768;

/// Highest sub-carrier index (inclusive)
pub const CARRIER_MAX: i32 = 768;

/// Total samples in one DAB transmission frame
pub const FRAME_SIZE: usize = NULL_SIZE + FRAME_SYMBOLS * SYMBOL_SIZE; // 196 952

/// Convert a DAB sub-carrier index k (−768..−1, +1..+768) to its FFT bin index.
///
/// The FFT output for a length-N transform places negative frequencies at
/// bins N/2..N-1, so we fold using modulo.
#[inline]
pub fn carrier_to_fft_bin(k: i32) -> usize {
    ((k + FFT_SIZE as i32) as usize) % FFT_SIZE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_size_matches_spec() {
        assert_eq!(FRAME_SIZE, 196_952);
    }

    #[test]
    fn symbol_size_correct() {
        assert_eq!(SYMBOL_SIZE, 2552);
    }

    #[test]
    fn carrier_to_bin_positive() {
        // k=1  → bin 1
        assert_eq!(carrier_to_fft_bin(1), 1);
        // k=768 → bin 768
        assert_eq!(carrier_to_fft_bin(768), 768);
    }

    #[test]
    fn carrier_to_bin_negative() {
        // k=-1  → bin 2047
        assert_eq!(carrier_to_fft_bin(-1), 2047);
        // k=-768 → bin 1280
        assert_eq!(carrier_to_fft_bin(-768), 1280);
    }
}
