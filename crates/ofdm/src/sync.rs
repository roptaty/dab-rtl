use num_complex::Complex32;
/// Frame synchroniser for DAB Mode I using energy-based null symbol detection.
///
/// The null symbol has very low energy compared to normal OFDM symbols.
/// We maintain a short-term energy estimate and declare a null when the
/// instantaneous energy drops below `threshold_factor * short_term_average`.
use std::collections::VecDeque;

use crate::params::{FRAME_SYMBOLS, NULL_SIZE, SYMBOL_SIZE};

/// Short sliding window for edge detection (much smaller than NULL_SIZE).
const WINDOW_SIZE: usize = 256;

/// Minimum samples before null detection is enabled, so the long-term energy
/// estimate has time to converge.
pub const MIN_WARMUP_SAMPLES: usize = 8192;

/// State machine for the synchroniser.
#[derive(Debug, Clone, PartialEq)]
pub enum SyncState {
    /// Searching for the first null symbol.
    Hunting,
    /// Null symbol energy dip detected at `null_start` (absolute sample index).
    NullFound { null_start: usize },
    /// Frame lock achieved.
    Locked,
}

/// Signals to the caller where a DAB frame begins.
#[derive(Debug, Clone)]
pub struct FrameStart {
    /// Absolute sample index at which the null symbol started.
    pub null_start: usize,
    /// Absolute sample index of the first sample after the null (= start of
    /// the phase-reference symbol).
    pub sample_offset: usize,
}

/// Energy-based frame synchroniser.
pub struct FrameSync {
    /// Sliding window of per-sample squared magnitudes.
    energy_buf: VecDeque<f32>,
    /// Window length (NULL_SIZE samples).
    window_size: usize,
    /// Declare null when energy < threshold_factor * long_term_avg.
    threshold_factor: f32,
    /// Current state.
    pub state: SyncState,
    /// Running sum of the energy window (for O(1) mean updates).
    window_energy: f64,
    /// Long-term average energy (exponential moving average).
    long_term_avg: f64,
    /// Absolute sample counter (wraps every usize::MAX).
    sample_count: usize,
    /// Samples counted while inside the current null dip.
    null_sample_count: usize,
    /// Set by `reset_for_resync()` to bypass the warmup check.
    warmup_done: bool,
}

impl FrameSync {
    /// Create a new synchroniser in the `Hunting` state.
    pub fn new() -> Self {
        Self {
            energy_buf: VecDeque::with_capacity(WINDOW_SIZE),
            window_size: WINDOW_SIZE,
            threshold_factor: 0.3,
            state: SyncState::Hunting,
            window_energy: 0.0,
            long_term_avg: 0.0,
            sample_count: 0,
            null_sample_count: 0,
            warmup_done: false,
        }
    }

    /// Feed new IQ samples into the synchroniser.
    ///
    /// Returns `Some(FrameStart)` the moment we have enough information to
    /// pinpoint the start of a frame (i.e. just after we have consumed all
    /// NULL_SIZE null samples and know the null ended).
    pub fn push_samples(&mut self, samples: &[Complex32]) -> Option<FrameStart> {
        for &s in samples {
            let energy = s.norm_sqr(); // |s|²
            self.update_window(energy);
            self.sample_count = self.sample_count.wrapping_add(1);

            let window_mean = if !self.energy_buf.is_empty() {
                self.window_energy / self.energy_buf.len() as f64
            } else {
                0.0
            };

            match &self.state.clone() {
                SyncState::Hunting | SyncState::Locked => {
                    // Update long-term average only when not in a null.
                    self.long_term_avg = 0.999 * self.long_term_avg + 0.001 * (window_mean);

                    // Periodic status log so we can verify the sync is running.
                    if self.sample_count.is_multiple_of(500_000) {
                        log::debug!(
                            "Sync: state={:?} samples={} avg={:.6} win_mean={:.6} thr={:.6}",
                            self.state,
                            self.sample_count,
                            self.long_term_avg,
                            window_mean,
                            self.threshold_factor as f64 * self.long_term_avg
                        );
                    }

                    // Detect energy dip (only after warm-up so the average
                    // has converged to the actual signal level).
                    if (self.warmup_done || self.sample_count >= MIN_WARMUP_SAMPLES)
                        && self.long_term_avg > 0.0
                        && window_mean < (self.threshold_factor as f64) * self.long_term_avg
                    {
                        let null_start = self.sample_count.wrapping_sub(self.energy_buf.len());
                        self.state = SyncState::NullFound { null_start };
                        self.null_sample_count = 1;
                        log::debug!(
                            "Null detected at sample {null_start}, \
                             window_mean={window_mean:.4}, avg={:.4}",
                            self.long_term_avg
                        );
                    }
                }

                SyncState::NullFound { null_start } => {
                    self.null_sample_count += 1;

                    // Still in the null?
                    let still_null = self.long_term_avg > 0.0
                        && window_mean < (self.threshold_factor as f64) * self.long_term_avg;

                    if still_null {
                        // Keep accumulating.
                    } else {
                        // Null ended — energy rose back.
                        // The phase-reference symbol starts right here.
                        let frame_start = FrameStart {
                            null_start: *null_start,
                            sample_offset: self.sample_count.wrapping_sub(1),
                        };
                        log::info!(
                            "Frame start: null_start={}, prs_offset={}, \
                             null_len={}",
                            frame_start.null_start,
                            frame_start.sample_offset,
                            self.null_sample_count
                        );
                        self.state = SyncState::Locked;
                        // Return immediately on first detection so the
                        // caller can process this frame before we skip
                        // ahead to a later null in the same buffer.
                        return Some(frame_start);
                    }
                }
            }
        }

        None
    }

    /// Update the sliding energy window with a new sample energy value.
    fn update_window(&mut self, energy: f32) {
        if self.energy_buf.len() == self.window_size {
            let old = self.energy_buf.pop_front().unwrap_or(0.0);
            self.window_energy -= old as f64;
        }
        self.energy_buf.push_back(energy);
        self.window_energy += energy as f64;
    }

    /// Reset the synchroniser for fast re-sync after losing frame tracking.
    ///
    /// Unlike `new()`, this preserves the converged `long_term_avg` so that
    /// null detection works immediately without needing 8192 warmup samples.
    pub fn reset_for_resync(&mut self) {
        self.state = SyncState::Hunting;
        // Keep energy_buf and window_energy intact — they hold valid signal
        // energy from the end of the previous frame.  This prevents false
        // null detections in the first ~256 samples after reset (the window
        // needs to fill with real null-energy samples before triggering).
        self.null_sample_count = 0;
        // Keep long_term_avg — it's already converged.
        // Reset sample_count to 0 so push_samples indices align with the
        // caller's buffer, but mark warmup as already done.
        self.sample_count = self.energy_buf.len();
        self.warmup_done = true;
    }

    /// Total samples consumed so far.
    pub fn sample_count(&self) -> usize {
        self.sample_count
    }

    /// Expected number of samples in a complete DAB frame (null + 76 symbols).
    pub fn frame_size() -> usize {
        NULL_SIZE + FRAME_SYMBOLS * SYMBOL_SIZE
    }
}

impl Default for FrameSync {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_samples(count: usize, amplitude: f32) -> Vec<Complex32> {
        vec![Complex32::new(amplitude, 0.0); count]
    }

    #[test]
    fn detects_null_after_warm_up() {
        let mut sync = FrameSync::new();
        // Warm up with loud signal (must exceed MIN_WARMUP_SAMPLES).
        let loud = make_samples(MIN_WARMUP_SAMPLES + 4096, 1.0);
        sync.push_samples(&loud);

        // Feed a null.
        let null = make_samples(NULL_SIZE, 0.01);
        sync.push_samples(&null);

        // Then a normal symbol so the null ends.
        let normal = make_samples(SYMBOL_SIZE, 1.0);
        let result = sync.push_samples(&normal);
        assert!(result.is_some(), "expected FrameStart to be returned");
    }

    #[test]
    fn state_transitions_to_locked() {
        let mut sync = FrameSync::new();
        let loud = make_samples(MIN_WARMUP_SAMPLES + 4096, 1.0);
        sync.push_samples(&loud);
        let null = make_samples(NULL_SIZE, 0.01);
        sync.push_samples(&null);
        let normal = make_samples(SYMBOL_SIZE, 1.0);
        sync.push_samples(&normal);
        assert_eq!(sync.state, SyncState::Locked);
    }
}
