/// OFDM demodulator for DAB Mode I.
///
/// Performs:
///   1. Guard interval (cyclic prefix) removal.
///   2. 2048-point FFT.
///   3. Active sub-carrier extraction (bins ±1..±768, DC skipped).
///   4. Differential demodulation (π/4-DQPSK): z[k] = cur[k] · conj(prev[k]).
///   5. Soft-bit output: interleaved real/imaginary parts of z[k].
use std::sync::Arc;

use num_complex::Complex32;
use rustfft::{num_complex::Complex, FftPlanner};

use crate::params::{
    carrier_to_fft_bin, CARRIER_MAX, CARRIER_MIN, FFT_SIZE, GUARD_SIZE, NUM_CARRIERS,
};

pub struct OfdmDemod {
    fft: Arc<dyn rustfft::Fft<f32>>,
    /// Scratch buffer reused each call to avoid allocation.
    fft_buf: Vec<Complex<f32>>,
    /// Carriers from the previous symbol (phase-reference or last data symbol).
    phase_ref: Vec<Complex32>,
    /// True once `process_phase_ref` has been called.
    has_ref: bool,
}

impl OfdmDemod {
    /// Create a new demodulator.
    pub fn new() -> Self {
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        Self {
            fft,
            fft_buf: vec![Complex::new(0.0, 0.0); FFT_SIZE],
            phase_ref: vec![Complex32::new(0.0, 0.0); NUM_CARRIERS],
            has_ref: false,
        }
    }

    /// Process the phase-reference symbol.
    ///
    /// Strips the guard interval, runs the FFT, and stores the 1 536 active
    /// carrier values for use as the differential reference for the first data
    /// symbol.
    pub fn process_phase_ref(&mut self, symbol_samples: &[Complex32]) {
        let carriers = self.fft_and_extract(symbol_samples);
        self.phase_ref.copy_from_slice(&carriers);
        self.has_ref = true;
        log::debug!("Phase reference symbol processed");
    }

    /// Demodulate one data symbol.
    ///
    /// * Strips guard interval.
    /// * Runs FFT.
    /// * Extracts 1 536 active carriers.
    /// * Differentially demodulates against the previous symbol's carriers.
    /// * Returns 3 072 soft bits (interleaved real, imag per carrier).
    ///
    /// Returns an empty vector if the phase reference has not been stored yet.
    pub fn demod_symbol(&mut self, symbol_samples: &[Complex32]) -> Vec<f32> {
        if !self.has_ref {
            log::warn!("demod_symbol called before phase reference is available");
            return Vec::new();
        }

        let current = self.fft_and_extract(symbol_samples);

        // Differential product: z[k] = current[k] * conj(prev[k])
        //
        // ETSI EN 300 401 §14.4, Table 42 maps coded bits (b0, b1) to the
        // QPSK symbol I + jQ.  b0 is carried by Q (imaginary) and b1 by I
        // (real).  The coded bit stream expects b0 first, so we output
        // (im, re) per carrier to maintain the correct bit order.
        let mut soft_bits = Vec::with_capacity(NUM_CARRIERS * 2);
        for (k, (&cur, &prev)) in current.iter().zip(self.phase_ref.iter()).enumerate() {
            let z = cur * prev.conj();
            soft_bits.push(z.im); // b0 = d_{2k}   (Q axis)
            soft_bits.push(z.re); // b1 = d_{2k+1}  (I axis)
            let _ = k; // index available for debugging
        }

        // Update previous carriers for the next symbol.
        self.phase_ref.copy_from_slice(&current);

        soft_bits
    }

    // ------------------------------------------------------------------ //
    //  Private helpers                                                     //
    // ------------------------------------------------------------------ //

    /// Remove guard interval, run FFT, return the 1 536 active carriers in
    /// sub-carrier index order (k = CARRIER_MIN..CARRIER_MAX, skipping 0).
    fn fft_and_extract(&mut self, symbol_samples: &[Complex32]) -> Vec<Complex32> {
        // Remove guard (cyclic prefix) — take the last FFT_SIZE samples.
        let start = if symbol_samples.len() >= FFT_SIZE + GUARD_SIZE {
            GUARD_SIZE
        } else if symbol_samples.len() >= FFT_SIZE {
            symbol_samples.len() - FFT_SIZE
        } else {
            0
        };
        let window = &symbol_samples[start..];
        let window = &window[..FFT_SIZE.min(window.len())];

        // Fill FFT buffer (zero-pad if short).
        for (dst, &src) in self.fft_buf.iter_mut().zip(window.iter()) {
            *dst = Complex::new(src.re, src.im);
        }
        for dst in self.fft_buf[window.len()..].iter_mut() {
            *dst = Complex::new(0.0, 0.0);
        }

        self.fft.process(&mut self.fft_buf);

        // Extract active carriers in sub-carrier index order.
        let mut carriers = Vec::with_capacity(NUM_CARRIERS);
        for k in CARRIER_MIN..=CARRIER_MAX {
            if k == 0 {
                continue; // DC carrier unused
            }
            let bin = carrier_to_fft_bin(k);
            let c = self.fft_buf[bin];
            carriers.push(Complex32::new(c.re, c.im));
        }

        carriers
    }
}

impl Default for OfdmDemod {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::SYMBOL_SIZE;

    fn zero_symbol() -> Vec<Complex32> {
        vec![Complex32::new(0.0, 0.0); SYMBOL_SIZE]
    }

    #[test]
    fn soft_bits_length() {
        let mut demod = OfdmDemod::new();
        demod.process_phase_ref(&zero_symbol());
        let bits = demod.demod_symbol(&zero_symbol());
        assert_eq!(bits.len(), NUM_CARRIERS * 2);
    }

    #[test]
    fn no_phase_ref_returns_empty() {
        let mut demod = OfdmDemod::new();
        let bits = demod.demod_symbol(&zero_symbol());
        assert!(bits.is_empty());
    }
}
