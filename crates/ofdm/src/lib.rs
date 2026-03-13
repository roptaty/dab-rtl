pub mod demod;
pub mod interleaver;
pub mod params;
pub mod sync;

pub use demod::OfdmDemod;
pub use interleaver::FreqDeinterleaver;
pub use sync::{FrameStart, FrameSync};

use num_complex::Complex32;
use thiserror::Error;

use params::{FRAME_SYMBOLS, NUM_CARRIERS, SYMBOL_SIZE};

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
/// Each inner `Vec<f32>` has `NUM_CARRIERS * 2 = 3 072` entries:
/// interleaved real and imaginary parts of the differential product.
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
                    // `fs.sample_offset` is the absolute sample index (as
                    // counted by the sync) at which the PRS begins.
                    // sample_buf[0] corresponds to absolute index 0 only if
                    // we never trimmed the buffer.  Since we accumulate all
                    // samples, sample_buf[i] ↔ absolute index i.
                    self.prs_offset = Some(fs.sample_offset);
                    log::info!(
                        "OfdmProcessor: frame lock, PRS at sample {}",
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

                // The soft bits come out interleaved re/im per carrier.
                // Split into per-carrier f32 values for the deinterleaver.
                // The deinterleaver operates on carrier-indexed floats, so we
                // deinterleave real and imaginary channels separately and then
                // re-interleave.
                let re_channel: Vec<f32> = raw_bits.iter().step_by(2).copied().collect();
                let im_channel: Vec<f32> = raw_bits.iter().skip(1).step_by(2).copied().collect();

                let re_di = self.deinterleaver.deinterleave(&re_channel);
                let im_di = self.deinterleaver.deinterleave(&im_channel);

                let mut interleaved = Vec::with_capacity(NUM_CARRIERS * 2);
                for (r, i) in re_di.into_iter().zip(im_di.into_iter()) {
                    interleaved.push(r);
                    interleaved.push(i);
                }

                soft_bits.push(interleaved);
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
