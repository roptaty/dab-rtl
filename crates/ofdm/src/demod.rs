/// OFDM demodulator for DAB Mode I.
///
/// Performs:
///   1. Fine frequency offset estimation (guard interval correlation).
///   2. Fine frequency correction (phase de-rotation of time-domain samples).
///   3. Guard interval (cyclic prefix) removal.
///   4. 2048-point FFT.
///   5. Coarse frequency offset estimation (DQPSK constellation quality search).
///   6. Active sub-carrier extraction (bins ±1..±768, DC skipped, offset-corrected).
///   7. Differential demodulation (π/4-DQPSK): z[k] = cur[k] · conj(prev[k]).
///   8. Soft-bit output: interleaved real/imaginary parts of z[k].
use std::f32::consts::PI;
use std::sync::Arc;

use num_complex::Complex32;
use rustfft::{num_complex::Complex, FftPlanner};

use crate::params::{
    carrier_to_fft_bin, CARRIER_MAX, CARRIER_MIN, FFT_SIZE, GUARD_SIZE, NUM_CARRIERS,
};

/// Maximum coarse frequency offset to search (±bins).
const COARSE_SEARCH_RANGE: i32 = 30;

pub struct OfdmDemod {
    fft: Arc<dyn rustfft::Fft<f32>>,
    /// Scratch buffer reused each call to avoid allocation.
    fft_buf: Vec<Complex<f32>>,
    /// Saved PRS FFT output for coarse offset estimation.
    prs_fft: Vec<Complex<f32>>,
    /// Carriers from the previous symbol (phase-reference or last data symbol).
    phase_ref: Vec<Complex32>,
    /// True once `process_phase_ref` has been called.
    has_ref: bool,
    /// True once the coarse offset has been determined.
    coarse_locked: bool,
    /// Coarse frequency offset in FFT bins.
    coarse_freq_offset: i32,
    /// Fine frequency offset as a fraction of the sub-carrier spacing.
    fine_freq_offset: f32,
    /// Residual inter-symbol phase correction factor.
    ///
    /// After per-sample fine frequency correction within the FFT window,
    /// there is still a constant phase offset between consecutive symbols
    /// of φ = 2π·ε·SYMBOL_SIZE/FFT_SIZE.  The differential product picks
    /// this up and we must de-rotate it.
    residual_correction: Complex32,
}

impl OfdmDemod {
    /// Create a new demodulator.
    pub fn new() -> Self {
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        Self {
            fft,
            fft_buf: vec![Complex::new(0.0, 0.0); FFT_SIZE],
            prs_fft: vec![Complex::new(0.0, 0.0); FFT_SIZE],
            phase_ref: vec![Complex32::new(0.0, 0.0); NUM_CARRIERS],
            has_ref: false,
            coarse_locked: false,
            coarse_freq_offset: 0,
            fine_freq_offset: 0.0,
            residual_correction: Complex32::new(1.0, 0.0),
        }
    }

    /// Process the phase-reference symbol.
    ///
    /// Estimates fine frequency offset and stores the FFT output.
    /// Coarse offset is determined on the first data symbol (needs both
    /// PRS and data FFT outputs for DQPSK quality metric).
    pub fn process_phase_ref(&mut self, symbol_samples: &[Complex32]) {
        // Estimate fine frequency offset from guard interval correlation.
        self.fine_freq_offset = Self::estimate_fine_freq_offset(symbol_samples);

        // Compute residual inter-symbol phase correction.
        //
        // After per-sample correction exp(-j·2π·ε·i/N) within each FFT window,
        // a constant phase φ = 2π·ε·SYMBOL_SIZE/FFT_SIZE accumulates between
        // consecutive symbols.  The differential product picks this up as an
        // extra rotation that must be removed.
        let residual_phase =
            2.0 * PI * self.fine_freq_offset * crate::params::SYMBOL_SIZE as f32 / FFT_SIZE as f32;
        self.residual_correction = Complex32::new((-residual_phase).cos(), (-residual_phase).sin());

        log::debug!(
            "Fine frequency offset: {:.4} sub-carrier spacings ({:.1} Hz), \
             residual phase: {:.1}°",
            self.fine_freq_offset,
            self.fine_freq_offset * (crate::params::SAMPLE_RATE as f32 / FFT_SIZE as f32),
            residual_phase.to_degrees()
        );

        // FFT with fine correction (no coarse applied yet).
        self.do_fft(symbol_samples);

        // Save the PRS FFT for coarse offset estimation.
        self.prs_fft.copy_from_slice(&self.fft_buf);

        // Extract PRS carriers (offset applied if already locked).
        let carriers = self.extract_carriers_with_offset(self.coarse_freq_offset);
        self.phase_ref.copy_from_slice(&carriers);
        self.has_ref = true;
        log::debug!("Phase reference symbol processed");
    }

    /// Demodulate one data symbol.
    ///
    /// On the first call (coarse not yet locked), searches for the best
    /// coarse offset using the power-spectrum metric, then re-extracts
    /// PRS carriers at the correct offset.
    ///
    /// Returns 3 072 soft bits in split layout `[Re(0)..Re(K-1), Im(0)..Im(K-1)]`
    /// or empty if PRS not yet processed.
    pub fn demod_symbol(&mut self, symbol_samples: &[Complex32]) -> Vec<f32> {
        if !self.has_ref {
            log::warn!("demod_symbol called before phase reference is available");
            return Vec::new();
        }

        // FFT the data symbol with fine correction.
        self.do_fft(symbol_samples);

        // If coarse offset not yet locked, search now using PRS + this data symbol.
        if !self.coarse_locked {
            self.coarse_freq_offset = self.search_coarse_offset();
            self.coarse_locked = true;
            log::info!(
                "Coarse frequency offset locked: {} bins ({:.0} Hz)",
                self.coarse_freq_offset,
                self.coarse_freq_offset as f32
                    * (crate::params::SAMPLE_RATE as f32 / FFT_SIZE as f32)
            );

            // Re-extract PRS carriers with correct offset.
            let prs_carriers = self.extract_from_buf(&self.prs_fft, self.coarse_freq_offset);
            self.phase_ref.copy_from_slice(&prs_carriers);
        }

        // Extract data carriers with coarse offset.
        let current = self.extract_carriers_with_offset(self.coarse_freq_offset);

        // Differential product: z[k] = current[k] * conj(prev[k]) * residual_correction
        //
        // The residual_correction removes the accumulated inter-symbol phase
        // caused by fine frequency offset.
        //
        // ETSI EN 300 401 §14.4, Table 42 (Gray-coded DQPSK):
        //   (0,0) → π/4    → I>0, Q>0
        //   (0,1) → 3π/4   → I<0, Q>0
        //   (1,1) → -3π/4  → I<0, Q<0
        //   (1,0) → -π/4   → I>0, Q<0
        // b0 = d_{2k}   → sign of Q (im): positive = 0
        // b1 = d_{2k+1} → sign of I (re): positive = 0
        let correction = self.residual_correction;
        let mut soft_bits = Vec::with_capacity(NUM_CARRIERS * 2);

        // Split layout: first all real parts, then all imaginary parts.
        // This matches welle.io's output format where the FIC accumulator
        // collects 2304 bits at a time across symbol boundaries.
        for (&cur, &prev) in current.iter().zip(self.phase_ref.iter()) {
            let z = (cur * prev.conj()) * correction;
            soft_bits.push(z.re); // I axis (first half)
        }
        for (&cur, &prev) in current.iter().zip(self.phase_ref.iter()) {
            let z = (cur * prev.conj()) * correction;
            soft_bits.push(z.im); // Q axis (second half)
        }

        // Update previous carriers for the next symbol.
        self.phase_ref.copy_from_slice(&current);

        soft_bits
    }

    // ------------------------------------------------------------------ //
    //  Frequency offset estimation                                        //
    // ------------------------------------------------------------------ //

    /// Estimate fine (fractional-bin) frequency offset from guard interval
    /// correlation.
    ///
    /// The guard interval is a copy of the last GUARD_SIZE samples of the
    /// useful part.  A frequency offset ε (in sub-carrier spacings) causes
    /// a phase shift of 2π·ε between corresponding samples separated by
    /// FFT_SIZE.  We estimate ε = arg(Σ s[n+FFT_SIZE]·conj(s[n])) / (2π).
    fn estimate_fine_freq_offset(symbol_samples: &[Complex32]) -> f32 {
        if symbol_samples.len() < FFT_SIZE + GUARD_SIZE {
            return 0.0;
        }
        let mut corr = Complex32::new(0.0, 0.0);
        for n in 0..GUARD_SIZE {
            let a = symbol_samples[n];
            let b = symbol_samples[n + FFT_SIZE];
            corr += b * a.conj();
        }
        corr.arg() / (2.0 * PI)
    }

    /// Search for the coarse frequency offset by finding where the DAB
    /// signal power is located in the PRS FFT spectrum.
    ///
    /// The DAB Mode I signal occupies 1536 carriers centred on DC.  A
    /// coarse frequency offset shifts the entire spectrum by Δ bins.
    /// We sum the power in the expected active carrier bins for each
    /// candidate offset; the offset with the highest in-band power wins.
    ///
    /// Unlike the previous DQPSK-metric approach, this correctly detects
    /// integer-bin offsets because it measures *where* the power is, not
    /// just *how well* the phases cluster.
    fn search_coarse_offset(&self) -> i32 {
        let mut best_offset = 0i32;
        let mut best_power = 0.0f64;

        for offset in -COARSE_SEARCH_RANGE..=COARSE_SEARCH_RANGE {
            let mut power = 0.0f64;
            for k in CARRIER_MIN..=CARRIER_MAX {
                if k == 0 {
                    continue;
                }
                let base_bin = carrier_to_fft_bin(k) as i32;
                let bin = ((base_bin + offset + FFT_SIZE as i32) as usize) % FFT_SIZE;
                let c = &self.prs_fft[bin];
                power += (c.re * c.re + c.im * c.im) as f64;
            }
            if power > best_power {
                best_power = power;
                best_offset = offset;
            }
        }

        log::debug!("Coarse offset search (power): best={best_offset} bins, power={best_power:.1}");
        best_offset
    }

    // ------------------------------------------------------------------ //
    //  Private helpers                                                     //
    // ------------------------------------------------------------------ //

    /// Remove guard interval, apply fine frequency correction, run FFT.
    /// Result is left in `self.fft_buf`.
    fn do_fft(&mut self, symbol_samples: &[Complex32]) {
        let start = if symbol_samples.len() >= FFT_SIZE + GUARD_SIZE {
            GUARD_SIZE
        } else if symbol_samples.len() >= FFT_SIZE {
            symbol_samples.len() - FFT_SIZE
        } else {
            0
        };
        let window = &symbol_samples[start..];
        let window = &window[..FFT_SIZE.min(window.len())];

        let fine_offset = self.fine_freq_offset;
        let phase_step = -2.0 * PI * fine_offset / FFT_SIZE as f32;
        for (i, (dst, &src)) in self.fft_buf.iter_mut().zip(window.iter()).enumerate() {
            if fine_offset.abs() > 1e-6 {
                let phase = phase_step * i as f32;
                let correction = Complex32::new(phase.cos(), phase.sin());
                let corrected = src * correction;
                *dst = Complex::new(corrected.re, corrected.im);
            } else {
                *dst = Complex::new(src.re, src.im);
            }
        }
        for dst in self.fft_buf[window.len()..].iter_mut() {
            *dst = Complex::new(0.0, 0.0);
        }

        self.fft.process(&mut self.fft_buf);
    }

    /// Extract carriers from `self.fft_buf` with a given bin offset.
    fn extract_carriers_with_offset(&self, offset: i32) -> Vec<Complex32> {
        self.extract_from_buf(&self.fft_buf, offset)
    }

    /// Extract active carriers from an arbitrary FFT buffer with offset.
    fn extract_from_buf(&self, buf: &[Complex<f32>], offset: i32) -> Vec<Complex32> {
        let mut carriers = Vec::with_capacity(NUM_CARRIERS);
        for k in CARRIER_MIN..=CARRIER_MAX {
            if k == 0 {
                continue;
            }
            let base_bin = carrier_to_fft_bin(k) as i32;
            let bin = ((base_bin + offset + FFT_SIZE as i32) as usize) % FFT_SIZE;
            let c = buf[bin];
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

    #[test]
    fn fine_freq_offset_zero_for_zero_signal() {
        let sym = zero_symbol();
        let offset = OfdmDemod::estimate_fine_freq_offset(&sym);
        assert!(offset.abs() < 1e-6, "expected ~0, got {offset}");
    }
}
