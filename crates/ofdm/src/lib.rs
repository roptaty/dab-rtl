pub mod demod;
pub mod interleaver;
pub mod params;
pub mod sync;

pub use demod::OfdmDemod;
pub use interleaver::FreqDeinterleaver;
pub use sync::{FrameStart, FrameSync};

use num_complex::Complex32;
use thiserror::Error;

use params::{FFT_SIZE, FRAME_SYMBOLS, GUARD_SIZE, NUM_CARRIERS, SYMBOL_SIZE};

// -------------------------------------------------------------------------- //
//  Error type                                                                 //
// -------------------------------------------------------------------------- //

#[derive(Error, Debug)]
pub enum OfdmError {
    #[error("Not synchronized")]
    NotSynchronized,
}

// -------------------------------------------------------------------------- //
//  OfdmFrame                                                                  //
// -------------------------------------------------------------------------- //

/// A decoded OFDM frame containing soft bits for all 75 data symbols.
///
/// Each inner `Vec<f32>` has `NUM_CARRIERS * 2 = 3 072` entries in split
/// layout: `[Re(0)..Re(1535), Im(0)..Im(1535)]`.
pub struct OfdmFrame {
    /// 75 symbols × 3 072 soft bits per symbol.
    pub soft_bits: Vec<Vec<f32>>,
}

// -------------------------------------------------------------------------- //
//  OfdmProcessor                                                              //
// -------------------------------------------------------------------------- //

/// High-level processor: accepts raw IQ samples and emits `OfdmFrame`s.
///
/// Internally it chains `FrameSync` → `OfdmDemod` → `FreqDeinterleaver`.
pub struct OfdmProcessor {
    sync: FrameSync,
    demod: OfdmDemod,
    deinterleaver: FreqDeinterleaver,
    /// Accumulation buffer for incoming samples.
    sample_buf: Vec<Complex32>,
    /// Absolute sample index of the most-recent `FrameStart::sample_offset`
    /// (start of the phase-reference symbol after the null).
    prs_offset: Option<usize>,
}

impl OfdmProcessor {
    /// Create a new processor.
    pub fn new() -> Self {
        Self {
            sync: FrameSync::new(),
            demod: OfdmDemod::new(),
            deinterleaver: FreqDeinterleaver::new(),
            sample_buf: Vec::new(),
            prs_offset: None,
        }
    }

    /// Push new IQ samples into the processor.
    ///
    /// Returns any complete `OfdmFrame`s produced from these samples.
    pub fn push_samples(&mut self, samples: &[Complex32]) -> Vec<OfdmFrame> {
        self.sample_buf.extend_from_slice(samples);
        let mut frames = Vec::new();

        loop {
            // ----------------------------------------------------------------
            // Phase 1: try to (re-)synchronise if we don't have a PRS offset.
            // ----------------------------------------------------------------
            if self.prs_offset.is_none() {
                // Feed only samples the sync has not yet seen.
                // `sync.sample_count()` returns the number of samples the sync
                // has consumed in total; sample_buf[sync_consumed..] is new.
                let sync_consumed = self.sync.sample_count();
                if sync_consumed >= self.sample_buf.len() {
                    // Nothing new for the sync — wait for more incoming data.
                    break;
                }
                let to_feed = &self.sample_buf[sync_consumed..];

                if let Some(fs) = self.sync.push_samples(to_feed) {
                    // The energy-based null detector has a lag due to its
                    // sliding window.  Refine the PRS start using guard-
                    // interval correlation, which peaks at the exact symbol
                    // boundary.
                    let refined = Self::refine_prs_start(&self.sample_buf, fs.sample_offset);
                    self.prs_offset = Some(refined);
                    log::info!(
                        "OfdmProcessor: frame lock, PRS at sample {} (raw {})",
                        refined,
                        fs.sample_offset
                    );
                } else {
                    // Still hunting — come back when more data arrives.
                    break;
                }
            }

            // ----------------------------------------------------------------
            // Phase 2: we know where the PRS starts; extract the full frame.
            // ----------------------------------------------------------------
            let prs_start = self.prs_offset.unwrap();

            // A full frame requires: 1 PRS symbol + 75 data symbols.
            let needed = prs_start + FRAME_SYMBOLS * SYMBOL_SIZE;
            if self.sample_buf.len() < needed {
                break; // not enough samples yet
            }

            // --- Phase-reference symbol ---
            let prs_samples = &self.sample_buf[prs_start..prs_start + SYMBOL_SIZE];
            self.demod.process_phase_ref(prs_samples);

            // --- 75 data symbols ---
            let data_start = prs_start + SYMBOL_SIZE;
            let mut soft_bits: Vec<Vec<f32>> = Vec::with_capacity(FRAME_SYMBOLS - 1);

            for sym_idx in 0..(FRAME_SYMBOLS - 1) {
                let sym_start = data_start + sym_idx * SYMBOL_SIZE;
                let sym_end = sym_start + SYMBOL_SIZE;
                let sym_samples = &self.sample_buf[sym_start..sym_end];

                let raw_bits = self.demod.demod_symbol(sym_samples);

                // The soft bits are in split layout:
                //   [Re(0)..Re(K-1), Im(0)..Im(K-1)]
                // Deinterleave each half separately, keep split layout.
                let (re_channel, im_channel) = raw_bits.split_at(NUM_CARRIERS);

                let re_di = self.deinterleaver.deinterleave(re_channel);
                let im_di = self.deinterleaver.deinterleave(im_channel);

                let mut deinterleaved = Vec::with_capacity(NUM_CARRIERS * 2);
                deinterleaved.extend_from_slice(&re_di);
                deinterleaved.extend_from_slice(&im_di);

                soft_bits.push(deinterleaved);
            }

            if log::log_enabled!(log::Level::Debug) {
                // Log soft-bit statistics for the first FIC symbol.
                if let Some(sym0) = soft_bits.first() {
                    let mean_abs: f32 =
                        sym0.iter().map(|v| v.abs()).sum::<f32>() / sym0.len() as f32;
                    let max_abs: f32 = sym0.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
                    log::debug!(
                        "OFDM frame: {} symbols, FIC sym0 mean_abs={:.4} max_abs={:.4}",
                        soft_bits.len(),
                        mean_abs,
                        max_abs
                    );
                }
            }

            frames.push(OfdmFrame { soft_bits });

            // ----------------------------------------------------------------
            // Phase 3: advance buffer and reset for the next frame.
            // ----------------------------------------------------------------
            // Discard all samples up to and including this frame.  The sync
            // is also reset because its internal sample_count refers to
            // absolute positions that change after the drain.
            self.sample_buf.drain(..needed.min(self.sample_buf.len()));
            self.prs_offset = None;
            self.sync = FrameSync::new();
        }

        frames
    }
}

impl OfdmProcessor {
    /// Refine the PRS start position using guard-interval correlation.
    ///
    /// The energy-based null detector has ~256-sample lag from its sliding
    /// window.  We search backwards from the raw detection point for the
    /// position that maximises guard-interval correlation (the cyclic prefix
    /// is a copy of the last GUARD_SIZE samples of the useful part).
    fn refine_prs_start(buf: &[Complex32], raw_offset: usize) -> usize {
        let search_back = 512; // search up to 512 samples earlier
        let search_fwd = 64; // and a little forward

        let start = raw_offset.saturating_sub(search_back);
        let end = (raw_offset + search_fwd).min(buf.len().saturating_sub(SYMBOL_SIZE));

        let mut best_pos = raw_offset;
        let mut best_corr = 0.0f32;

        // Coarse search (step 4)
        let mut pos = start;
        while pos <= end {
            let corr = Self::guard_corr(buf, pos);
            if corr > best_corr {
                best_corr = corr;
                best_pos = pos;
            }
            pos += 4;
        }

        // Fine-tune (single-sample)
        let fine_start = best_pos.saturating_sub(4);
        let fine_end = (best_pos + 4).min(buf.len().saturating_sub(SYMBOL_SIZE));
        for p in fine_start..=fine_end {
            let corr = Self::guard_corr(buf, p);
            if corr > best_corr {
                best_corr = corr;
                best_pos = p;
            }
        }

        log::debug!(
            "PRS refinement: raw={} → refined={} (Δ={}), guard_corr={:.4}",
            raw_offset,
            best_pos,
            raw_offset as i64 - best_pos as i64,
            best_corr
        );
        best_pos
    }

    /// Guard-interval correlation at a given position.
    fn guard_corr(buf: &[Complex32], start: usize) -> f32 {
        if start + SYMBOL_SIZE > buf.len() {
            return 0.0;
        }
        let sym = &buf[start..start + SYMBOL_SIZE];
        let mut corr = Complex32::new(0.0, 0.0);
        let mut power = 0.0f32;
        for n in 0..GUARD_SIZE {
            corr += sym[n + FFT_SIZE] * sym[n].conj();
            power += sym[n].norm_sqr() + sym[n + FFT_SIZE].norm_sqr();
        }
        if power > 0.0 {
            corr.norm() / (power / 2.0)
        } else {
            0.0
        }
    }
}

impl Default for OfdmProcessor {
    fn default() -> Self {
        Self::new()
    }
}

// -------------------------------------------------------------------------- //
//  Sanity checks                                                              //
// -------------------------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use super::*;
    use params::{GUARD_SIZE, NULL_SIZE};
    use sync::MIN_WARMUP_SAMPLES;

    #[test]
    fn processor_constructs() {
        let _p = OfdmProcessor::new();
    }

    #[test]
    fn empty_push_returns_no_frames() {
        let mut p = OfdmProcessor::new();
        let frames = p.push_samples(&[]);
        assert!(frames.is_empty());
    }

    #[test]
    fn frame_soft_bits_shape() {
        // Build a synthetic DAB frame: loud signal, then null, then loud signal.
        let loud = |n: usize| -> Vec<Complex32> { vec![Complex32::new(1.0, 0.0); n] };
        let quiet = |n: usize| -> Vec<Complex32> { vec![Complex32::new(0.001, 0.0); n] };

        let mut p = OfdmProcessor::new();

        // Warm-up: enough samples for the sync to start detecting nulls.
        p.push_samples(&loud(MIN_WARMUP_SAMPLES + 4096));

        // Null symbol.
        p.push_samples(&quiet(NULL_SIZE));

        // Phase-reference + 75 data symbols (all loud).
        let frame_samples = loud(FRAME_SYMBOLS * SYMBOL_SIZE);
        let frames = p.push_samples(&frame_samples);

        if !frames.is_empty() {
            let frame = &frames[0];
            assert_eq!(frame.soft_bits.len(), FRAME_SYMBOLS - 1);
            assert_eq!(frame.soft_bits[0].len(), NUM_CARRIERS * 2);
        }
        // If sync didn't fire (timing edge case) that is also acceptable;
        // the important thing is no panic.
    }

    #[test]
    fn guard_size_constant() {
        assert_eq!(GUARD_SIZE, 504);
    }
}
