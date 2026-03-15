/// DAB receive pipeline.
///
/// Connects: SDR → OFDM → FIC decoder → MSC decoder → MP2 decode → audio out
///
/// # DAB Mode I frame layout
///
/// An OFDM frame has 75 data symbols (after the phase-reference symbol).
///   - Symbols  0–2  (3 symbols)       → FIC (Fast Information Channel)
///   - Symbols  3–74 (72 symbols = 4 CIFs × 18 symbols) → MSC
///
/// Each symbol carries NUM_CARRIERS × 2 = 3072 soft bits.
///
/// FIC decoding (per FIC symbol):
///   3072 soft bits + 24 tail erasures → Viterbi (rate 1/4) → 774 bits
///   Take first 768 bits = 96 bytes = 3 FIBs → FicHandler
///
/// MSC decoding (per CIF = 18 symbols = 55296 soft bits = 864 CUs):
///   Extract target subchannel (start_address … start_address+size CUs)
///   → EEP depuncture → Viterbi → pack bytes → MP2 decoder
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use audio::{DabPlusDecoder, Mp2Decoder};
use fec::ViterbiDecoder;
use ofdm::OfdmProcessor;
use protocol::{
    ensemble::{Component, ProtectionLevel},
    Ensemble, FicHandler,
};

// ─────────────────────────────────────────────────────────────────────────── //
//  Public API types                                                            //
// ─────────────────────────────────────────────────────────────────────────── //

/// Updates emitted by the pipeline background thread.
#[derive(Debug, Clone)]
pub enum PipelineUpdate {
    /// Ensemble info refreshed (new service labels etc.).
    Ensemble(Ensemble),
    /// Successfully started playing a service.
    Playing { label: String },
    /// Pipeline status message (for the status bar).
    Status(String),
}

/// Commands sent to the pipeline background thread.
pub enum PipelineCmd {
    /// Select and play a service by its SId.
    Play(u32),
    /// Stop playback (keep scanning FIC).
    Stop,
}

/// Handle to the running pipeline.  Drop to stop all background threads.
pub struct PipelineHandle {
    pub update_rx: mpsc::Receiver<PipelineUpdate>,
    pub cmd_tx: mpsc::SyncSender<PipelineCmd>,
}

// ─────────────────────────────────────────────────────────────────────────── //
//  Pipeline launch                                                             //
// ─────────────────────────────────────────────────────────────────────────── //

/// Start the receive pipeline from a pre-opened IQ stream.
pub fn start_with_stream(
    stream: sdr::SdrStream,
    audio_device: Option<String>,
) -> Result<PipelineHandle, String> {
    // Channel: background → TUI updates
    let (update_tx, update_rx) = mpsc::sync_channel::<PipelineUpdate>(32);
    // Channel: TUI → background commands
    let (cmd_tx, cmd_rx) = mpsc::sync_channel::<PipelineCmd>(8);
    // Shared command state so the inner SDR loop can check it.
    let cmd_rx = Arc::new(Mutex::new(cmd_rx));

    // AudioOutput contains cpal::Stream which is !Send, so it must be
    // constructed inside the pipeline thread rather than passed across threads.
    thread::Builder::new()
        .name("pipeline".into())
        .spawn(move || {
            let audio_out = audio::AudioOutput::open(audio_device.as_deref(), 48_000, 2)
                .map_err(|e| log::warn!("audio open failed: {e}"))
                .ok();
            if let Some(ref ao) = audio_out {
                ao.play();
            }
            run_pipeline(stream, audio_out, update_tx, cmd_rx);
        })
        .map_err(|e| e.to_string())?;

    Ok(PipelineHandle { update_rx, cmd_tx })
}

// ─────────────────────────────────────────────────────────────────────────── //
//  Pipeline main loop                                                          //
// ─────────────────────────────────────────────────────────────────────────── //

fn run_pipeline(
    stream: sdr::SdrStream,
    audio_out: Option<audio::AudioOutput>,
    update_tx: mpsc::SyncSender<PipelineUpdate>,
    cmd_rx: Arc<Mutex<mpsc::Receiver<PipelineCmd>>>,
) {
    let mut ofdm = OfdmProcessor::new();
    let mut fic = FicDecoder::new();
    let mut msc = MscDecoder::new();
    let mut mp2 = Mp2Decoder::new(1152); // ~3 MP2 frames before decode attempt
    let mut dab_plus = DabPlusDecoder::new(0); // size set when component is known

    // Currently selected SId (None = scan-only).
    let mut playing_sid: Option<u32> = None;
    let mut last_ens_label = String::new();
    let mut last_svc_count = 0usize;
    let mut last_svc_labels = String::new();
    let mut frame_count = 0u64;

    let _ = update_tx.try_send(PipelineUpdate::Status("Hunting for signal…".into()));

    for iq_buf in stream.rx.iter() {
        // Drain any pending commands.
        if let Ok(guard) = cmd_rx.try_lock() {
            while let Ok(cmd) = guard.try_recv() {
                match cmd {
                    PipelineCmd::Play(sid) => {
                        playing_sid = Some(sid);
                        msc.set_target_sid(sid);
                    }
                    PipelineCmd::Stop => {
                        playing_sid = None;
                        msc.clear_target();
                    }
                }
            }
        }

        // OFDM demodulation.
        for frame in ofdm.push_samples(&iq_buf) {
            frame_count += 1;
            log::debug!("Pipeline: OFDM frame #{}", frame_count);

            // ── FIC (symbols 0-2) ────────────────────────────────────────── //
            fic.begin_frame();
            let fic_symbols = frame.soft_bits.get(0..3).unwrap_or_default();
            for sym in fic_symbols {
                fic.process_symbol(sym);
            }

            // Propagate ensemble changes to the TUI.
            let ens = fic.handler.ensemble();
            // Build a fingerprint of service labels so we detect when labels
            // arrive (they come in separate FIG messages after services appear).
            let svc_labels: String = ens
                .services
                .iter()
                .map(|s| format!("{:04X}:{}", s.id, s.label))
                .collect::<Vec<_>>()
                .join(",");
            if ens.label != last_ens_label
                || ens.services.len() != last_svc_count
                || svc_labels != last_svc_labels
            {
                last_ens_label = ens.label.clone();
                last_svc_count = ens.services.len();
                last_svc_labels = svc_labels;
                log::info!(
                    "Ensemble: id={:04X} label={:?} services={}",
                    ens.id,
                    ens.label,
                    ens.services.len()
                );
                for svc in &ens.services {
                    log::info!(
                        "  Service: id={:04X} label={:?} dab+={} components={}",
                        svc.id,
                        svc.label,
                        svc.is_dab_plus,
                        svc.components.len()
                    );
                    for comp in &svc.components {
                        log::info!(
                            "    Component: subch={} start={} size={} prot={:?}",
                            comp.subchannel_id,
                            comp.start_address,
                            comp.size,
                            comp.protection
                        );
                    }
                }
                let _ = update_tx.try_send(PipelineUpdate::Ensemble(ens.clone()));
                let _ = update_tx.try_send(PipelineUpdate::Status(format!(
                    "Locked — {} services",
                    ens.services.len()
                )));
            }

            // Announce when we start playing.
            if let Some(sid) = playing_sid {
                if let Some(svc) = ens.services.iter().find(|s| s.id == sid) {
                    let _ = update_tx.try_send(PipelineUpdate::Playing {
                        label: svc.label.clone(),
                    });
                }
            }

            // ── MSC (symbols 3-74, 4 CIFs × 18 symbols) ─────────────────── //
            if playing_sid.is_some() {
                let ens_snap = ens.clone();
                let msc_symbols = frame.soft_bits.get(3..).unwrap_or_default();

                for (cif_idx, cif_syms) in msc_symbols.chunks(18).enumerate() {
                    if cif_syms.len() < 18 {
                        continue;
                    }
                    // Flatten CIF symbols → 55296 soft bits.
                    let cif_soft: Vec<f32> =
                        cif_syms.iter().flat_map(|s| s.iter().copied()).collect();

                    if let Some(sid) = playing_sid {
                        let component = find_component(&ens_snap, sid);
                        if component.is_none() && cif_idx == 0 {
                            log::debug!(
                                "MSC: no component found for SId {:04X} (service has {} components)",
                                sid,
                                ens_snap
                                    .services
                                    .iter()
                                    .find(|s| s.id == sid)
                                    .map_or(0, |s| s.components.len())
                            );
                        }
                        if let Some(component) = component {
                            if let Some(frame) = msc.process_cif(&cif_soft, component, cif_idx) {
                                log::debug!(
                                    "MSC: CIF {} subchannel {} → {} bytes ({})",
                                    cif_idx,
                                    frame.subchannel_id,
                                    frame.data.len(),
                                    if frame.is_dab_plus { "DAB+" } else { "DAB" }
                                );
                                // Set DAB+ superframe size from actual Viterbi output.
                                if frame.is_dab_plus
                                    && !frame.data.is_empty()
                                    && dab_plus.superframe_size != frame.data.len()
                                {
                                    log::info!(
                                        "DAB+: setting per-CIF size to {} bytes (was {})",
                                        frame.data.len(),
                                        dab_plus.superframe_size
                                    );
                                    dab_plus.set_superframe_size(frame.data.len());
                                }
                                let pcm = if frame.is_dab_plus {
                                    dab_plus.push(&frame.data)
                                } else {
                                    mp2.push(&frame.data)
                                };
                                if pcm.is_empty() {
                                    log::debug!(
                                        "MSC: audio decoder returned 0 PCM samples (buffering or decode error)"
                                    );
                                } else if let Some(ao) = &audio_out {
                                    let (min, max) =
                                        pcm.iter().fold((f32::MAX, f32::MIN), |(lo, hi), &s| {
                                            (lo.min(s), hi.max(s))
                                        });
                                    log::debug!(
                                        "MSC: writing {} PCM samples to audio device (range {:.4}..{:.4})",
                                        pcm.len(),
                                        min,
                                        max
                                    );
                                    ao.write_samples(&pcm);
                                } else {
                                    log::debug!(
                                        "MSC: {} PCM samples ready but no audio device",
                                        pcm.len()
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    log::info!("pipeline: IQ stream ended, thread exiting");
}

// ─────────────────────────────────────────────────────────────────────────── //
//  FIC decoder                                                                 //
// ─────────────────────────────────────────────────────────────────────────── //

/// Wraps the Viterbi decoder and FicHandler for the FIC channel.
///
/// The FIC is punctured: 3 OFDM symbols × 3 072 soft bits = 9 216 bits
/// are split into 4 FIC blocks of 2 304 punctured bits each.
/// Each block is depunctured (PI_16/PI_15/PI_X) → 3 096 mother-code bits
/// → Viterbi → 768 info bits = 96 bytes = 3 FIBs.
pub struct FicDecoder {
    viterbi: ViterbiDecoder,
    /// PRBS sequence for energy de-dispersal (768 bits = full CIF).
    ///
    /// ETSI EN 300 401 §12: the PRBS runs continuously across all 3 FIBs
    /// within a CIF (not reset per FIB).
    prbs_bits: Vec<u8>,
    pub handler: FicHandler,
    /// Accumulation buffer for FIC soft bits across OFDM symbols.
    fic_buf: Vec<f32>,
}

impl FicDecoder {
    pub fn new() -> Self {
        FicDecoder {
            viterbi: ViterbiDecoder::new(5 * fec::viterbi::K),
            prbs_bits: Self::generate_prbs(768),
            handler: FicHandler::new(),
            fic_buf: Vec::with_capacity(fec::FIC_PUNCTURED_BITS),
        }
    }

    /// Generate energy dispersal PRBS: polynomial x^9 + x^5 + 1, all-ones init.
    fn generate_prbs(len: usize) -> Vec<u8> {
        let mut reg: u16 = 0x1FF; // 9-bit register, all ones
        let mut out = Vec::with_capacity(len);
        for _ in 0..len {
            let bit = ((reg >> 8) ^ (reg >> 4)) & 1;
            out.push(bit as u8);
            reg = ((reg << 1) | bit) & 0x1FF;
        }
        out
    }

    /// Reset the accumulation buffer at the start of a new frame.
    pub fn begin_frame(&mut self) {
        self.fic_buf.clear();
    }

    /// Feed one FIC OFDM symbol (3 072 soft bits).
    ///
    /// Soft bits are accumulated and a FIC block is processed every
    /// 2 304 bits (the punctured block size).
    pub fn process_symbol(&mut self, soft: &[f32]) {
        log::debug!(
            "FIC: accumulating {} soft bits (buf={})",
            soft.len(),
            self.fic_buf.len()
        );

        for &s in soft {
            self.fic_buf.push(s);
            if self.fic_buf.len() >= fec::FIC_PUNCTURED_BITS {
                self.process_fic_block();
            }
        }
    }

    /// Process one complete FIC block (2 304 punctured soft bits).
    fn process_fic_block(&mut self) {
        const INFO_BITS: usize = 768;

        let block: Vec<f32> = self.fic_buf.drain(..fec::FIC_PUNCTURED_BITS).collect();

        // Normalize soft bits to ~[-1, +1] for Viterbi.
        let max_abs = block.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
        let scale = if max_abs > 0.0 { 1.0 / max_abs } else { 1.0 };
        let normalized: Vec<f32> = block.iter().map(|v| v * scale).collect();

        // Depuncture 2304 → 3096 using PI_16/PI_15/PI_X.
        let depunctured = fec::fic_depuncture(&normalized);

        let bits = self.viterbi.decode(&depunctured);
        let info = &bits[..bits.len().min(INFO_BITS)];

        // Pack bits MSB-first → 96 bytes.
        let mut fic_bytes = pack_bits(info);

        // Energy de-dispersal: apply continuous PRBS across all 3 FIBs.
        self.energy_dedispersal(&mut fic_bytes);

        // Log FIB CRC results.
        for fib_idx in 0..3 {
            let start = fib_idx * 32;
            if start + 32 <= fic_bytes.len() {
                let fib = &fic_bytes[start..start + 32];
                let crc_ok = fib_crc_check(fib);
                log::debug!(
                    "FIC: FIB {} CRC {} (first bytes: {:02X} {:02X} {:02X} {:02X})",
                    fib_idx,
                    if crc_ok { "OK" } else { "FAIL" },
                    fib[0],
                    fib[1],
                    fib[2],
                    fib[3]
                );
            }
        }

        self.handler.process_fic_bytes(&fic_bytes);
    }

    /// XOR FIC bytes (96 bytes = 3 FIBs) with the continuous PRBS.
    fn energy_dedispersal(&self, fic_bytes: &mut [u8]) {
        for (i, byte) in fic_bytes.iter_mut().enumerate() {
            let mut mask = 0u8;
            for bit in 0..8 {
                let idx = i * 8 + bit;
                if idx < self.prbs_bits.len() && self.prbs_bits[idx] != 0 {
                    mask |= 0x80 >> bit;
                }
            }
            *byte ^= mask;
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────── //
//  MSC decoder                                                                 //
// ─────────────────────────────────────────────────────────────────────────── //

/// Time interleaver permutation table (ETSI EN 300 401 §14.6, Table 31).
///
/// This is a 4-bit reversal permutation.  The transmitter delays bit position
/// `i` by `PI[i % 16]` CIFs.  The receiver (deinterleaver) must compensate by
/// picking bit `i` from the CIF that arrived `PI[i % 16]` positions ago
/// relative to the oldest buffered CIF.
const TIME_INTERLEAVE_PI: [usize; 16] = [0, 8, 4, 12, 2, 10, 6, 14, 1, 9, 5, 13, 3, 11, 7, 15];

pub struct MscDecoder {
    target_sid: Option<u32>,
    viterbi: ViterbiDecoder,
    /// Time deinterleaver: ring buffer of 16 CIFs of subchannel soft bits.
    deint_buf: Vec<Vec<f32>>,
    /// Total number of CIFs pushed into the deinterleaver.
    deint_count: usize,
    /// Expected subchannel soft-bit count per CIF (reset on subchannel change).
    deint_bits_per_cif: usize,
}

impl MscDecoder {
    pub fn new() -> Self {
        MscDecoder {
            target_sid: None,
            viterbi: ViterbiDecoder::new(128),
            deint_buf: Vec::new(),
            deint_count: 0,
            deint_bits_per_cif: 0,
        }
    }

    pub fn set_target_sid(&mut self, sid: u32) {
        self.target_sid = Some(sid);
        self.reset_deinterleaver();
    }

    pub fn clear_target(&mut self) {
        self.target_sid = None;
        self.reset_deinterleaver();
    }

    fn reset_deinterleaver(&mut self) {
        self.deint_buf.clear();
        self.deint_count = 0;
        self.deint_bits_per_cif = 0;
    }

    /// Decode one CIF (55296 soft bits) for the given component.
    ///
    /// Returns `None` if no target SId is set, if still filling the time
    /// deinterleaver (first 15 CIFs), or if the subchannel range is invalid.
    pub fn process_cif(
        &mut self,
        cif_soft: &[f32],
        component: &Component,
        _cif_idx: usize,
    ) -> Option<protocol::AudioFrame> {
        // Require an active target.
        self.target_sid?;

        // Extract subchannel soft bits.
        let start_bit = component.start_address as usize * 64;
        let end_bit = start_bit + component.size as usize * 64;

        if end_bit > cif_soft.len() {
            log::warn!(
                "MSC: subchannel range {}..{} exceeds CIF length {}",
                start_bit,
                end_bit,
                cif_soft.len()
            );
            return None;
        }

        let subchannel_soft = &cif_soft[start_bit..end_bit];
        let bits_per_cif = subchannel_soft.len();

        // Reset deinterleaver if subchannel size changed.
        if bits_per_cif != self.deint_bits_per_cif {
            self.deint_bits_per_cif = bits_per_cif;
            self.deint_buf = vec![vec![0.0f32; bits_per_cif]; 16];
            self.deint_count = 0;
        }

        // Store in ring buffer.
        let slot = self.deint_count % 16;
        self.deint_buf[slot] = subchannel_soft.to_vec();
        self.deint_count += 1;

        // Need 16 CIFs before the deinterleaver can produce output.
        if self.deint_count < 16 {
            log::debug!("MSC: time deinterleaver filling ({}/16)", self.deint_count);
            return None;
        }

        // Assemble deinterleaved soft bits.
        // Physical CIF just written = self.deint_count - 1.
        // We output the logical frame whose latest contribution just arrived.
        // For bit i: source physical CIF = (current - 15 + PI[i % 16]).
        let p = self.deint_count - 1;
        let deint_soft: Vec<f32> = (0..bits_per_cif)
            .map(|i| {
                let source_cif = p - 15 + TIME_INTERLEAVE_PI[i % 16];
                let source_slot = source_cif % 16;
                self.deint_buf[source_slot][i]
            })
            .collect();

        // Normalize soft bits to ~[-1, +1] for Viterbi (matches FIC path).
        let max_abs = deint_soft.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
        let scale = if max_abs > 0.0 { 1.0 / max_abs } else { 1.0 };
        let normalized: Vec<f32> = deint_soft.iter().map(|v| v * scale).collect();

        // Apply two-region EEP depuncturing (ETSI EN 300 401 Tables 8/9).
        let depunct = eep_depuncture(&normalized, component);

        // Viterbi decode.  Strip K−1 = 6 tail bits (forced-zero flush bits
        // appended by the encoder; they are not part of the information stream).
        let bits = self.viterbi.decode(&depunct);
        let info_len = bits.len().saturating_sub(6);

        // Pack to bytes.
        let mut data = pack_bits(&bits[..info_len]);

        // Energy de-dispersal: XOR with PRBS (ETSI EN 300 401 §12).
        energy_dedispersal(&mut data);

        log::debug!(
            "MSC: decoded {} bytes, first 4: [{:02X} {:02X} {:02X} {:02X}]",
            data.len(),
            data.first().copied().unwrap_or(0),
            data.get(1).copied().unwrap_or(0),
            data.get(2).copied().unwrap_or(0),
            data.get(3).copied().unwrap_or(0),
        );

        Some(protocol::AudioFrame {
            subchannel_id: component.subchannel_id,
            data,
            is_dab_plus: component.service_type == protocol::ServiceType::DabPlus,
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────── //
//  Helpers                                                                     //
// ─────────────────────────────────────────────────────────────────────────── //

/// Two-region EEP depuncturing for an MSC subchannel.
///
/// Computes (L1, L2, PI1, PI2) from the component's protection level
/// and subchannel size, then applies the proper two-region scheme
/// per ETSI EN 300 401 Tables 8/9.
fn eep_depuncture(soft: &[f32], component: &Component) -> Vec<f32> {
    let (l1, l2, pi1, pi2) = match &component.protection {
        ProtectionLevel::EepA(level) => fec::eep_a_params(component.size, *level),
        ProtectionLevel::EepB(level) => fec::eep_b_params(component.size, *level),
        ProtectionLevel::Uep(_level) => {
            // UEP has up to 4 regions; fall back to uniform depuncturing.
            // TODO: implement proper UEP multi-region (ETSI EN 300 401 Table 6).
            let n = component.size as usize / 6;
            (6 * n.max(1) - 3, 3, 7, 6)
        }
    };

    log::debug!(
        "MSC depuncture: size={} CUs, prot={:?}, L1={}, L2={}, PI{}+PI{}",
        component.size,
        component.protection,
        l1,
        l2,
        pi1 + 1,
        pi2 + 1
    );

    fec::msc_eep_depuncture(soft, l1, l2, pi1, pi2)
}

/// Find the first audio component for a service in the ensemble.
fn find_component(ens: &Ensemble, sid: u32) -> Option<&Component> {
    ens.services
        .iter()
        .find(|s| s.id == sid)?
        .components
        .first()
}

/// Quick FIB CRC-16/CCITT check for debug logging.
fn fib_crc_check(fib: &[u8]) -> bool {
    if fib.len() < 32 {
        return false;
    }
    let mut crc: u16 = 0xFFFF;
    for &byte in &fib[..30] {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    let computed = !crc;
    let stored = u16::from_be_bytes([fib[30], fib[31]]);
    computed == stored
}

/// Apply energy de-dispersal PRBS to MSC data bytes.
///
/// ETSI EN 300 401 §12: polynomial x^9 + x^5 + 1, all-ones initial state.
/// The PRBS is XORed with the data bytes to reverse the scrambling applied
/// at the transmitter.
fn energy_dedispersal(data: &mut [u8]) {
    let mut reg: u16 = 0x1FF; // 9-bit register, all ones
    for byte in data.iter_mut() {
        let mut mask = 0u8;
        for bit in 0..8 {
            let prbs_bit = ((reg >> 8) ^ (reg >> 4)) & 1;
            if prbs_bit != 0 {
                mask |= 0x80 >> bit;
            }
            reg = ((reg << 1) | prbs_bit) & 0x1FF;
        }
        *byte ^= mask;
    }
}

/// Pack a bit slice (MSB first) into bytes.
fn pack_bits(bits: &[u8]) -> Vec<u8> {
    let n = bits.len().div_ceil(8);
    let mut out = vec![0u8; n];
    for (i, &b) in bits.iter().enumerate() {
        if b != 0 {
            out[i / 8] |= 0x80 >> (i % 8);
        }
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────── //
//  Tests                                                                       //
// ─────────────────────────────────────────────────────────────────────────── //

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_bits_all_ones() {
        let bits = vec![1u8; 8];
        assert_eq!(pack_bits(&bits), vec![0xFF]);
    }

    #[test]
    fn pack_bits_all_zeros() {
        let bits = vec![0u8; 8];
        assert_eq!(pack_bits(&bits), vec![0x00]);
    }

    #[test]
    fn fic_decoder_constructs() {
        let _d = FicDecoder::new();
    }

    #[test]
    fn fic_decoder_handles_short_symbol() {
        let mut d = FicDecoder::new();
        // Should not panic on short input
        d.process_symbol(&[0.5f32; 100]);
    }

    #[test]
    fn fic_decoder_handles_full_symbol() {
        let mut d = FicDecoder::new();
        // Full 3072-element symbol of zeroed soft bits
        let sym = vec![0.0f32; 3072];
        d.process_symbol(&sym); // must not panic
    }

    /// Verify EEP depuncture + Viterbi round-trip with synthetic data.
    #[test]
    fn eep_depuncture_viterbi_roundtrip() {
        use protocol::ensemble::{Component, ProtectionLevel, ServiceType};

        let comp = Component {
            subchannel_id: 0,
            service_type: ServiceType::DabPlus,
            start_address: 0,
            size: 60,
            protection: ProtectionLevel::EepA(3),
        };

        // Encode known data through rate-1/4 convolutional encoder
        let (l1, l2, pi1, pi2) = fec::eep_a_params(comp.size, 3);
        let total_mother = (l1 + l2) * 128 + 24;
        let n_info = total_mother / 4; // 1926 info bits (including 6 tail)

        // Create a test pattern: alternating 01 bits
        let info_bits: Vec<u8> = (0..n_info).map(|i| (i % 2) as u8).collect();

        // Encode using the encoder from viterbi tests
        let transitions = {
            let _vdec = ViterbiDecoder::new(35);
            // We need to encode manually since encode() is not public
            // Use the same approach as the viterbi test
            let polys: [u8; 4] = [109, 79, 83, 109];
            let mut state: usize = 0;
            let mut encoded = Vec::with_capacity(n_info * 4);
            for &bit in &info_bits {
                let next_state = ((state << 1) | bit as usize) & 63;
                for &poly in &polys {
                    let reg = ((state as u16) << 1) | (bit as u16);
                    let xored = reg as u8 & poly;
                    let out_bit = xored.count_ones() as u8 & 1;
                    // Map 0→+1.0, 1→-1.0
                    encoded.push(if out_bit == 0 { 1.0f32 } else { -1.0f32 });
                }
                state = next_state;
            }
            encoded
        };

        assert_eq!(transitions.len(), total_mother);

        // Puncture: keep only the bits where the pattern says 1
        let pi1_vec = &fec::depuncturer::PUNCT_VECTORS[pi1];
        let pi2_vec = &fec::depuncturer::PUNCT_VECTORS[pi2];
        let pi_x: [u8; 24] = [
            1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0,
        ];

        let mut punctured = Vec::new();
        let mut src = 0;
        // Region 1
        for _ in 0..l1 {
            for _ in 0..4 {
                for &keep in pi1_vec.iter() {
                    if keep == 1 {
                        punctured.push(transitions[src]);
                    }
                    src += 1;
                }
            }
        }
        // Region 2
        for _ in 0..l2 {
            for _ in 0..4 {
                for &keep in pi2_vec.iter() {
                    if keep == 1 {
                        punctured.push(transitions[src]);
                    }
                    src += 1;
                }
            }
        }
        // Tail
        for &keep in pi_x.iter() {
            if keep == 1 {
                punctured.push(transitions[src]);
            }
            src += 1;
        }

        assert_eq!(
            punctured.len(),
            comp.size as usize * 64,
            "punctured length should match subchannel size"
        );

        // Now depuncture + Viterbi
        let depunct = eep_depuncture(&punctured, &comp);
        assert_eq!(depunct.len(), total_mother);

        let viterbi = ViterbiDecoder::new(128);
        let bits = viterbi.decode(&depunct);
        let info_len = bits.len().saturating_sub(6);

        // Skip first K-1=6 bits (trellis start edge effect)
        let skip = 6;
        assert_eq!(
            bits[skip..info_len],
            info_bits[skip..info_len],
            "EEP depuncture + Viterbi should roundtrip correctly"
        );
    }

    #[test]
    fn msc_decoder_no_target_returns_none() {
        let mut dec = MscDecoder::new();
        use protocol::ensemble::{Component, ProtectionLevel, ServiceType};
        let comp = Component {
            subchannel_id: 0,
            service_type: ServiceType::Audio,
            start_address: 0,
            size: 4,
            protection: ProtectionLevel::EepA(2),
        };
        let cif = vec![1.0f32; 55296];
        assert!(dec.process_cif(&cif, &comp, 0).is_none());
    }

    #[test]
    fn energy_dedispersal_roundtrip() {
        // Apply dispersal twice = identity (XOR is self-inverse).
        let mut data = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x23, 0x45, 0x67];
        let original = data.clone();
        energy_dedispersal(&mut data);
        assert_ne!(data, original, "PRBS should change data");
        energy_dedispersal(&mut data);
        assert_eq!(data, original, "double XOR should restore original");
    }

    /// End-to-end test: IQ file → OFDM → FIC → discover services → MSC decode.
    ///
    /// Verifies that the pipeline produces non-empty audio frames from the
    /// test IQ recording.
    #[test]
    #[ignore] // slow: processes full IQ capture; run with --ignored
    fn msc_decode_from_iq_file() {
        use num_complex::Complex32;

        let iq_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../..",
            "/testdata/dab_13b_2min.raw"
        );
        if !std::path::Path::new(iq_path).exists() {
            eprintln!("Skipping MSC test: IQ file not found at {iq_path}");
            return;
        }

        let raw = std::fs::read(iq_path).unwrap();
        let samples: Vec<Complex32> = raw
            .chunks_exact(2)
            .map(|c| Complex32::new((c[0] as f32 - 127.5) / 127.5, (c[1] as f32 - 127.5) / 127.5))
            .collect();

        let mut ofdm = OfdmProcessor::new();
        let mut fic = FicDecoder::new();
        let mut msc = MscDecoder::new();

        let chunk_size = 65536;
        let max_samples = 30 * 2_048_000; // 30 seconds
        let limit = samples.len().min(max_samples);

        let mut ensemble_found = false;
        let mut first_service_sid: Option<u32> = None;
        let mut audio_frames_decoded = 0usize;
        let mut total_audio_bytes = 0usize;
        let mut frame_count = 0usize;
        let mut dab_plus_buf: Vec<u8> = Vec::new();

        for chunk_start in (0..limit).step_by(chunk_size) {
            let chunk_end = (chunk_start + chunk_size).min(limit);
            let chunk = &samples[chunk_start..chunk_end];

            for frame in ofdm.push_samples(chunk) {
                frame_count += 1;

                // FIC
                fic.begin_frame();
                for sym in frame.soft_bits.get(0..3).unwrap_or_default() {
                    fic.process_symbol(sym);
                }

                let ens = fic.handler.ensemble();
                if frame_count <= 5 || frame_count.is_multiple_of(20) {
                    eprintln!(
                        "Frame {frame_count}: ens={:?} services={}",
                        ens.label,
                        ens.services.len()
                    );
                }

                // Pick the first service with a valid (non-zero size) component.
                if !ens.services.is_empty() {
                    ensemble_found = true;
                }
                if first_service_sid.is_none() {
                    // Prefer DAB+ services for Firecode testing
                    for svc in &ens.services {
                        if let Some(comp) = svc.components.first() {
                            if comp.size > 0 && svc.is_dab_plus {
                                first_service_sid = Some(svc.id);
                                msc.set_target_sid(svc.id);
                                eprintln!(
                                    "Selected service (DAB+): {:04X} {:?} (start={}, size={}, prot={:?})",
                                    svc.id,
                                    svc.label,
                                    comp.start_address,
                                    comp.size,
                                    comp.protection
                                );
                                break;
                            }
                        }
                    }
                    // Fall back to any service if no DAB+ found
                    if first_service_sid.is_none() {
                        for svc in &ens.services {
                            if let Some(comp) = svc.components.first() {
                                if comp.size > 0 {
                                    first_service_sid = Some(svc.id);
                                    msc.set_target_sid(svc.id);
                                    eprintln!(
                                        "Selected service (DAB): {:04X} {:?} (start={}, size={}, prot={:?})",
                                        svc.id,
                                        svc.label,
                                        comp.start_address,
                                        comp.size,
                                        comp.protection
                                    );
                                    break;
                                }
                            }
                        }
                    }
                }

                // MSC
                if let Some(sid) = first_service_sid {
                    let msc_symbols = frame.soft_bits.get(3..).unwrap_or_default();
                    for (cif_idx, cif_syms) in msc_symbols.chunks(18).enumerate() {
                        if cif_syms.len() < 18 {
                            continue;
                        }
                        let cif_soft: Vec<f32> =
                            cif_syms.iter().flat_map(|s| s.iter().copied()).collect();
                        if let Some(component) = find_component(ens, sid) {
                            if let Some(af) = msc.process_cif(&cif_soft, component, cif_idx) {
                                audio_frames_decoded += 1;
                                total_audio_bytes += af.data.len();
                                dab_plus_buf.extend_from_slice(&af.data);
                            }
                        }
                    }
                }
            }
        }

        // Check Firecode CRC on accumulated DAB+ superframes.
        let per_cif = if total_audio_bytes > 0 && audio_frames_decoded > 0 {
            total_audio_bytes / audio_frames_decoded
        } else {
            0
        };
        let sf_size = per_cif * 5;
        let mut firecode_pass = 0usize;
        let mut firecode_fail = 0usize;
        if sf_size > 0 {
            // Try all 5 possible CIF alignments
            for offset in 0..5 {
                let start = offset * per_cif;
                let mut pos = start;
                let mut align_pass = 0usize;
                let mut align_fail = 0usize;
                while pos + sf_size <= dab_plus_buf.len() {
                    if audio::firecode_check(&dab_plus_buf[pos..pos + sf_size]) {
                        align_pass += 1;
                    } else {
                        align_fail += 1;
                    }
                    pos += sf_size;
                }
                eprintln!(
                    "  Alignment {}: pass={}, fail={} ({:.1}%)",
                    offset,
                    align_pass,
                    align_fail,
                    if align_pass + align_fail > 0 {
                        align_pass as f64 / (align_pass + align_fail) as f64 * 100.0
                    } else {
                        0.0
                    }
                );
                firecode_pass += align_pass;
                firecode_fail += align_fail;
            }
        }
        eprintln!(
            "Results: ensemble={ensemble_found}, audio_frames={audio_frames_decoded}, \
             total_bytes={total_audio_bytes}, per_cif={per_cif}"
        );
        eprintln!("Firecode: pass={firecode_pass}, fail={firecode_fail} (sf_size={sf_size})");
        // Print first few bytes of the buffer for inspection
        if dab_plus_buf.len() >= 20 {
            eprintln!("First 20 bytes: {:02X?}", &dab_plus_buf[..20]);
        }

        assert!(ensemble_found, "should discover at least one service");
        assert!(
            audio_frames_decoded > 0,
            "should decode at least one MSC audio frame"
        );
        assert!(total_audio_bytes > 0, "audio frames should contain data");
    }

    /// Diagnostic: MSC decode without time deinterleaving to isolate the bug.
    #[test]
    #[ignore]
    fn msc_no_time_deinterleave() {
        use num_complex::Complex32;

        let iq_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../..",
            "/testdata/dab_13b_2min.raw"
        );
        if !std::path::Path::new(iq_path).exists() {
            eprintln!("Skipping: IQ file not found");
            return;
        }

        let raw = std::fs::read(iq_path).unwrap();
        let samples: Vec<Complex32> = raw
            .chunks_exact(2)
            .map(|c| Complex32::new((c[0] as f32 - 127.5) / 127.5, (c[1] as f32 - 127.5) / 127.5))
            .collect();

        let mut ofdm = OfdmProcessor::new();
        let mut fic = FicDecoder::new();
        let viterbi = ViterbiDecoder::new(128);

        let chunk_size = 65536;
        let max_samples = 10 * 2_048_000; // 10 seconds
        let limit = samples.len().min(max_samples);

        let mut first_service_sid: Option<u32> = None;
        let mut first_component: Option<protocol::ensemble::Component> = None;
        let mut frame_count = 0usize;

        // Collect raw (no time deinterleave) CIF outputs
        let mut raw_cif_outputs: Vec<Vec<u8>> = Vec::new();

        for chunk_start in (0..limit).step_by(chunk_size) {
            let chunk_end = (chunk_start + chunk_size).min(limit);
            let chunk = &samples[chunk_start..chunk_end];

            for frame in ofdm.push_samples(chunk) {
                frame_count += 1;

                // FIC
                fic.begin_frame();
                for sym in frame.soft_bits.get(0..3).unwrap_or_default() {
                    fic.process_symbol(sym);
                }

                let ens = fic.handler.ensemble();

                // Pick first service
                if first_service_sid.is_none() {
                    for svc in &ens.services {
                        if let Some(comp) = svc.components.first() {
                            if comp.size > 0 {
                                first_service_sid = Some(svc.id);
                                first_component = Some(comp.clone());
                                eprintln!(
                                    "Selected: {:04X} {:?} (start={}, size={}, prot={:?})",
                                    svc.id,
                                    svc.label,
                                    comp.start_address,
                                    comp.size,
                                    comp.protection
                                );
                                break;
                            }
                        }
                    }
                }

                // MSC: decode each CIF WITHOUT time deinterleaving
                if let Some(ref component) = first_component {
                    let msc_symbols = frame.soft_bits.get(3..).unwrap_or_default();
                    for (cif_idx, cif_syms) in msc_symbols.chunks(18).enumerate() {
                        if cif_syms.len() < 18 {
                            continue;
                        }
                        let cif_soft: Vec<f32> =
                            cif_syms.iter().flat_map(|s| s.iter().copied()).collect();

                        let start_bit = component.start_address as usize * 64;
                        let end_bit = start_bit + component.size as usize * 64;
                        if end_bit > cif_soft.len() {
                            continue;
                        }
                        let subchannel_soft = &cif_soft[start_bit..end_bit];

                        // Normalize
                        let max_abs = subchannel_soft
                            .iter()
                            .map(|v| v.abs())
                            .fold(0.0f32, f32::max);
                        let scale = if max_abs > 0.0 { 1.0 / max_abs } else { 1.0 };
                        let normalized: Vec<f32> =
                            subchannel_soft.iter().map(|v| v * scale).collect();

                        // Depuncture
                        let depunct = eep_depuncture(&normalized, component);

                        // Viterbi
                        let bits = viterbi.decode(&depunct);
                        let info_len = bits.len().saturating_sub(6);
                        let mut data = pack_bits(&bits[..info_len]);

                        // Energy de-dispersal
                        energy_dedispersal(&mut data);

                        if cif_idx == 0 && frame_count <= 5 {
                            eprintln!(
                                "Frame {} CIF {} raw (no deint): {} bytes, first 8: {:02X?}",
                                frame_count,
                                cif_idx,
                                data.len(),
                                &data[..data.len().min(8)]
                            );
                        }

                        raw_cif_outputs.push(data);
                    }
                }
            }
        }

        // Check Firecode on raw (no deinterleave) outputs
        let per_cif = if let Some(d) = raw_cif_outputs.first() {
            d.len()
        } else {
            0
        };
        let sf_size = per_cif * 5;
        eprintln!(
            "Raw CIF outputs: {}, per_cif={} bytes, sf_size={}",
            raw_cif_outputs.len(),
            per_cif,
            sf_size
        );

        let mut pass_raw = 0usize;
        let mut fail_raw = 0usize;
        // Concatenate all raw outputs and check Firecode at all 5 alignments
        let all_raw: Vec<u8> = raw_cif_outputs
            .iter()
            .flat_map(|d| d.iter().copied())
            .collect();
        if sf_size > 0 {
            for offset in 0..5 {
                let start = offset * per_cif;
                let mut pos = start;
                while pos + sf_size <= all_raw.len() {
                    if audio::firecode_check(&all_raw[pos..pos + sf_size]) {
                        pass_raw += 1;
                    } else {
                        fail_raw += 1;
                    }
                    pos += sf_size;
                }
            }
        }
        eprintln!("Firecode (NO deinterleave): pass={pass_raw}, fail={fail_raw}");

        // Also check: what if we just try interleaved bit layout for each symbol?
        // Convert split [Re(0..1535), Im(0..1535)] to interleaved [Im(0),Re(0),Im(1),Re(1),...]
        // and redo the MSC decode.
        let mut interleaved_cif_outputs: Vec<Vec<u8>> = Vec::new();
        let mut ofdm2 = OfdmProcessor::new();
        let mut fic2 = FicDecoder::new();
        let mut comp2: Option<protocol::ensemble::Component> = None;

        for chunk_start in (0..limit).step_by(chunk_size) {
            let chunk_end = (chunk_start + chunk_size).min(limit);
            let chunk = &samples[chunk_start..chunk_end];

            for frame in ofdm2.push_samples(chunk) {
                fic2.begin_frame();
                for sym in frame.soft_bits.get(0..3).unwrap_or_default() {
                    fic2.process_symbol(sym);
                }
                let ens = fic2.handler.ensemble();
                if comp2.is_none() {
                    for svc in &ens.services {
                        if let Some(c) = svc.components.first() {
                            if c.size > 0 {
                                comp2 = Some(c.clone());
                                break;
                            }
                        }
                    }
                }

                if let Some(ref component) = comp2 {
                    let msc_symbols = frame.soft_bits.get(3..).unwrap_or_default();
                    for cif_syms in msc_symbols.chunks(18) {
                        if cif_syms.len() < 18 {
                            continue;
                        }

                        // Convert each symbol from split to interleaved layout
                        let cif_soft: Vec<f32> = cif_syms
                            .iter()
                            .flat_map(|sym| {
                                // sym is [Re(0)..Re(1535), Im(0)..Im(1535)]
                                // Convert to [Im(0),Re(0),Im(1),Re(1),...]
                                let (re, im) = sym.split_at(1536);
                                re.iter()
                                    .zip(im.iter())
                                    .flat_map(|(&r, &i)| [i, r])
                                    .collect::<Vec<f32>>()
                            })
                            .collect();

                        let start_bit = component.start_address as usize * 64;
                        let end_bit = start_bit + component.size as usize * 64;
                        if end_bit > cif_soft.len() {
                            continue;
                        }
                        let subchannel_soft = &cif_soft[start_bit..end_bit];

                        let max_abs = subchannel_soft
                            .iter()
                            .map(|v| v.abs())
                            .fold(0.0f32, f32::max);
                        let scale = if max_abs > 0.0 { 1.0 / max_abs } else { 1.0 };
                        let normalized: Vec<f32> =
                            subchannel_soft.iter().map(|v| v * scale).collect();

                        let depunct = eep_depuncture(&normalized, component);
                        let bits = viterbi.decode(&depunct);
                        let info_len = bits.len().saturating_sub(6);
                        let mut data = pack_bits(&bits[..info_len]);
                        energy_dedispersal(&mut data);

                        interleaved_cif_outputs.push(data);
                    }
                }
            }
        }

        // Check Firecode on interleaved-layout outputs
        let all_interleaved: Vec<u8> = interleaved_cif_outputs
            .iter()
            .flat_map(|d| d.iter().copied())
            .collect();
        let mut pass_interleaved = 0usize;
        let mut fail_interleaved = 0usize;
        if sf_size > 0 {
            for offset in 0..5 {
                let start = offset * per_cif;
                let mut pos = start;
                while pos + sf_size <= all_interleaved.len() {
                    if audio::firecode_check(&all_interleaved[pos..pos + sf_size]) {
                        pass_interleaved += 1;
                    } else {
                        fail_interleaved += 1;
                    }
                    pos += sf_size;
                }
            }
        }
        eprintln!(
            "Firecode (interleaved layout, no deinterleave): pass={pass_interleaved}, fail={fail_interleaved}"
        );

        // Test D: interleaved layout WITH time deinterleaving
        let mut ofdm3 = OfdmProcessor::new();
        let mut fic3 = FicDecoder::new();
        let mut comp3: Option<protocol::ensemble::Component> = None;
        let mut deint_buf3: Vec<Vec<f32>> = Vec::new();
        let mut deint_count3 = 0usize;
        let mut deint_bits_per_cif3 = 0usize;
        let mut interleaved_deint_outputs: Vec<Vec<u8>> = Vec::new();

        for chunk_start in (0..limit).step_by(chunk_size) {
            let chunk_end = (chunk_start + chunk_size).min(limit);
            let chunk = &samples[chunk_start..chunk_end];

            for frame in ofdm3.push_samples(chunk) {
                fic3.begin_frame();
                for sym in frame.soft_bits.get(0..3).unwrap_or_default() {
                    fic3.process_symbol(sym);
                }
                let ens = fic3.handler.ensemble();
                if comp3.is_none() {
                    for svc in &ens.services {
                        if let Some(c) = svc.components.first() {
                            if c.size > 0 {
                                comp3 = Some(c.clone());
                                break;
                            }
                        }
                    }
                }

                if let Some(ref component) = comp3 {
                    let msc_symbols = frame.soft_bits.get(3..).unwrap_or_default();
                    for cif_syms in msc_symbols.chunks(18) {
                        if cif_syms.len() < 18 {
                            continue;
                        }

                        // Interleaved CIF
                        let cif_soft: Vec<f32> = cif_syms
                            .iter()
                            .flat_map(|sym| {
                                let (re, im) = sym.split_at(1536);
                                re.iter()
                                    .zip(im.iter())
                                    .flat_map(|(&r, &i)| [i, r])
                                    .collect::<Vec<f32>>()
                            })
                            .collect();

                        let start_bit = component.start_address as usize * 64;
                        let end_bit = start_bit + component.size as usize * 64;
                        if end_bit > cif_soft.len() {
                            continue;
                        }
                        let subchannel_soft = &cif_soft[start_bit..end_bit];
                        let bits_per_cif = subchannel_soft.len();

                        if bits_per_cif != deint_bits_per_cif3 {
                            deint_bits_per_cif3 = bits_per_cif;
                            deint_buf3 = vec![vec![0.0f32; bits_per_cif]; 16];
                            deint_count3 = 0;
                        }

                        let slot = deint_count3 % 16;
                        deint_buf3[slot] = subchannel_soft.to_vec();
                        deint_count3 += 1;

                        if deint_count3 < 16 {
                            continue;
                        }

                        let p = deint_count3 - 1;
                        let deint_soft: Vec<f32> = (0..bits_per_cif)
                            .map(|i| {
                                let source_cif = p - 15 + TIME_INTERLEAVE_PI[i % 16];
                                let source_slot = source_cif % 16;
                                deint_buf3[source_slot][i]
                            })
                            .collect();

                        let max_abs = deint_soft.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
                        let scale = if max_abs > 0.0 { 1.0 / max_abs } else { 1.0 };
                        let normalized: Vec<f32> = deint_soft.iter().map(|v| v * scale).collect();

                        let depunct = eep_depuncture(&normalized, component);
                        let bits = viterbi.decode(&depunct);
                        let info_len = bits.len().saturating_sub(6);
                        let mut data = pack_bits(&bits[..info_len]);
                        energy_dedispersal(&mut data);

                        interleaved_deint_outputs.push(data);
                    }
                }
            }
        }

        let all_id: Vec<u8> = interleaved_deint_outputs
            .iter()
            .flat_map(|d| d.iter().copied())
            .collect();
        let mut pass_id = 0usize;
        let mut fail_id = 0usize;
        if sf_size > 0 {
            for offset in 0..5 {
                let start = offset * per_cif;
                let mut pos = start;
                while pos + sf_size <= all_id.len() {
                    if audio::firecode_check(&all_id[pos..pos + sf_size]) {
                        pass_id += 1;
                    } else {
                        fail_id += 1;
                    }
                    pos += sf_size;
                }
            }
        }
        eprintln!(
            "Firecode (interleaved layout + time deinterleave): pass={pass_id}, fail={fail_id}"
        );

        // Diagnostic: check soft bit statistics per symbol index
        let mut ofdm4 = OfdmProcessor::new();
        let mut sym_stats: Vec<(f32, f32, usize)> = vec![(0.0, 0.0, 0); 75]; // (sum_abs, sum_sq, count)

        for chunk_start in (0..limit.min(5 * 2_048_000)).step_by(chunk_size) {
            let chunk_end = (chunk_start + chunk_size).min(limit);
            let chunk = &samples[chunk_start..chunk_end];

            for frame in ofdm4.push_samples(chunk) {
                for (sym_idx, sym) in frame.soft_bits.iter().enumerate() {
                    let mean_abs: f32 = sym.iter().map(|v| v.abs()).sum::<f32>() / sym.len() as f32;
                    let mean_sq: f32 = sym.iter().map(|v| v * v).sum::<f32>() / sym.len() as f32;
                    sym_stats[sym_idx].0 += mean_abs;
                    sym_stats[sym_idx].1 += mean_sq;
                    sym_stats[sym_idx].2 += 1;
                }
            }
        }

        eprintln!("\nSoft bit statistics per symbol index (first 5s):");
        eprintln!("Sym | Mean|soft| | RMS    | Count");
        for (i, (sum_abs, sum_sq, count)) in sym_stats.iter().enumerate() {
            if *count > 0 {
                let avg_abs = sum_abs / *count as f32;
                let rms = (sum_sq / *count as f32).sqrt();
                let label = if i < 3 { "FIC" } else { "MSC" };
                if !(6..72).contains(&i) || i % 18 == 3 {
                    eprintln!(
                        "{:3} | {:.4}   | {:.4} | {} ({})",
                        i, avg_abs, rms, count, label
                    );
                }
            }
        }

        // Compare Viterbi path metrics for FIC vs MSC
        let mut ofdm5 = OfdmProcessor::new();
        let mut fic_metrics: Vec<f32> = Vec::new();
        let mut msc_metrics: Vec<f32> = Vec::new();
        let vit = ViterbiDecoder::new(128);

        for chunk_start in (0..limit.min(5 * 2_048_000)).step_by(chunk_size) {
            let chunk_end = (chunk_start + chunk_size).min(limit);
            let chunk = &samples[chunk_start..chunk_end];

            for frame in ofdm5.push_samples(chunk) {
                // FIC: process first block (2304 soft bits → FIC depuncture → Viterbi)
                let fic_bits: Vec<f32> = frame
                    .soft_bits
                    .get(0..3)
                    .unwrap_or_default()
                    .iter()
                    .flat_map(|s| s.iter().copied())
                    .collect();
                if fic_bits.len() >= fec::FIC_PUNCTURED_BITS {
                    let block: Vec<f32> = fic_bits[..fec::FIC_PUNCTURED_BITS].to_vec();
                    let max_abs = block.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
                    let scale = if max_abs > 0.0 { 1.0 / max_abs } else { 1.0 };
                    let normalized: Vec<f32> = block.iter().map(|v| v * scale).collect();
                    let depunctured = fec::fic_depuncture(&normalized);
                    let (_, metric) = vit.decode_with_metric(&depunctured);
                    fic_metrics.push(metric);
                }

                // MSC: process first CIF's subchannel (if component known)
                if let Some(ref component) = comp3 {
                    let msc_syms = frame.soft_bits.get(3..21).unwrap_or_default();
                    if msc_syms.len() == 18 {
                        let cif_soft: Vec<f32> =
                            msc_syms.iter().flat_map(|s| s.iter().copied()).collect();
                        let start_bit = component.start_address as usize * 64;
                        let end_bit = start_bit + component.size as usize * 64;
                        if end_bit <= cif_soft.len() {
                            let subchannel = &cif_soft[start_bit..end_bit];
                            let max_abs = subchannel.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
                            let scale = if max_abs > 0.0 { 1.0 / max_abs } else { 1.0 };
                            let normalized: Vec<f32> =
                                subchannel.iter().map(|v| v * scale).collect();
                            let depunct = eep_depuncture(&normalized, component);
                            let (_, metric) = vit.decode_with_metric(&depunct);
                            msc_metrics.push(metric);
                        }
                    }
                }
            }
        }

        if !fic_metrics.is_empty() {
            let avg_fic: f32 = fic_metrics.iter().sum::<f32>() / fic_metrics.len() as f32;
            eprintln!(
                "\nViterbi path metric (FIC): avg={:.2}, min={:.2}, max={:.2} (n={})",
                avg_fic,
                fic_metrics.iter().cloned().fold(f32::MAX, f32::min),
                fic_metrics.iter().cloned().fold(f32::MIN, f32::max),
                fic_metrics.len()
            );
        }
        if !msc_metrics.is_empty() {
            let avg_msc: f32 = msc_metrics.iter().sum::<f32>() / msc_metrics.len() as f32;
            eprintln!(
                "Viterbi path metric (MSC, no deint): avg={:.2}, min={:.2}, max={:.2} (n={})",
                avg_msc,
                msc_metrics.iter().cloned().fold(f32::MAX, f32::min),
                msc_metrics.iter().cloned().fold(f32::MIN, f32::max),
                msc_metrics.len()
            );
        }

        // Also compute metric WITH time deinterleaving (from full pipeline)
        // Use the interleaved_deint_outputs test data to verify
        // For a cleaner test, process full pipeline MSC and get metrics
        let mut ofdm6 = OfdmProcessor::new();
        let mut fic6 = FicDecoder::new();
        let mut msc6 = MscDecoder::new();
        let mut comp6: Option<protocol::ensemble::Component> = None;
        for chunk_start in (0..limit.min(5 * 2_048_000)).step_by(chunk_size) {
            let chunk_end = (chunk_start + chunk_size).min(limit);
            let chunk = &samples[chunk_start..chunk_end];

            for frame in ofdm6.push_samples(chunk) {
                fic6.begin_frame();
                for sym in frame.soft_bits.get(0..3).unwrap_or_default() {
                    fic6.process_symbol(sym);
                }
                let ens = fic6.handler.ensemble();
                if comp6.is_none() {
                    for svc in &ens.services {
                        if svc.is_dab_plus {
                            if let Some(c) = svc.components.first() {
                                if c.size > 0 {
                                    comp6 = Some(c.clone());
                                    msc6.set_target_sid(svc.id);
                                    break;
                                }
                            }
                        }
                    }
                }
                if let Some(ref component) = comp6 {
                    let msc_symbols = frame.soft_bits.get(3..).unwrap_or_default();
                    for (ci, cif_syms) in msc_symbols.chunks(18).enumerate() {
                        if cif_syms.len() < 18 {
                            continue;
                        }
                        let cif_soft: Vec<f32> =
                            cif_syms.iter().flat_map(|s| s.iter().copied()).collect();
                        // Use process_cif to get time-deinterleaved data, but
                        // also compute metric on the deinterleaved soft bits
                        let start_bit = component.start_address as usize * 64;
                        let end_bit = start_bit + component.size as usize * 64;
                        if end_bit > cif_soft.len() {
                            continue;
                        }
                        // Feed into the MscDecoder to advance the deinterleaver
                        if let Some(_af) = msc6.process_cif(&cif_soft, component, ci) {
                            // The deinterleaver just produced output.
                            // Get the deinterleaved soft bits by re-extracting
                            // from the deinterleaver state.
                            // (Simplified: just compute metric on the output)
                            // We'll use the returned data to check Firecode AND
                            // compute Viterbi metric on the deinterleaved data.
                        }
                    }
                }
            }
        }

        // We can't easily get the deinterleaved soft bits from process_cif.
        // Instead, replicate the deinterleaver inline and compute metric.
        let mut ofdm7 = OfdmProcessor::new();
        let mut fic7 = FicDecoder::new();
        let mut comp7: Option<protocol::ensemble::Component> = None;
        let mut dbuf7: Vec<Vec<f32>> = Vec::new();
        let mut dc7 = 0usize;
        let mut dbpc7 = 0usize;
        let mut deint_metrics: Vec<f32> = Vec::new();

        for chunk_start in (0..limit.min(5 * 2_048_000)).step_by(chunk_size) {
            let chunk_end = (chunk_start + chunk_size).min(limit);
            let chunk = &samples[chunk_start..chunk_end];

            for frame in ofdm7.push_samples(chunk) {
                fic7.begin_frame();
                for sym in frame.soft_bits.get(0..3).unwrap_or_default() {
                    fic7.process_symbol(sym);
                }
                let ens = fic7.handler.ensemble();
                if comp7.is_none() {
                    for svc in &ens.services {
                        if svc.is_dab_plus {
                            if let Some(c) = svc.components.first() {
                                if c.size > 0 {
                                    comp7 = Some(c.clone());
                                    break;
                                }
                            }
                        }
                    }
                }
                if let Some(ref component) = comp7 {
                    let msc_symbols = frame.soft_bits.get(3..).unwrap_or_default();
                    for cif_syms in msc_symbols.chunks(18) {
                        if cif_syms.len() < 18 {
                            continue;
                        }
                        let cif_soft: Vec<f32> =
                            cif_syms.iter().flat_map(|s| s.iter().copied()).collect();
                        let start_bit = component.start_address as usize * 64;
                        let end_bit = start_bit + component.size as usize * 64;
                        if end_bit > cif_soft.len() {
                            continue;
                        }
                        let sub = &cif_soft[start_bit..end_bit];
                        let bpc = sub.len();
                        if bpc != dbpc7 {
                            dbpc7 = bpc;
                            dbuf7 = vec![vec![0.0f32; bpc]; 16];
                            dc7 = 0;
                        }
                        dbuf7[dc7 % 16] = sub.to_vec();
                        dc7 += 1;
                        if dc7 < 16 {
                            continue;
                        }
                        let p = dc7 - 1;
                        let deint_soft: Vec<f32> = (0..bpc)
                            .map(|i| {
                                let sc = p - 15 + TIME_INTERLEAVE_PI[i % 16];
                                dbuf7[sc % 16][i]
                            })
                            .collect();

                        let max_abs = deint_soft.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
                        let scale = if max_abs > 0.0 { 1.0 / max_abs } else { 1.0 };
                        let normalized: Vec<f32> = deint_soft.iter().map(|v| v * scale).collect();
                        let depunct = eep_depuncture(&normalized, component);
                        let (_, metric) = vit.decode_with_metric(&depunct);
                        deint_metrics.push(metric);
                    }
                }
            }
        }

        if !deint_metrics.is_empty() {
            let avg: f32 = deint_metrics.iter().sum::<f32>() / deint_metrics.len() as f32;
            eprintln!(
                "Viterbi path metric (MSC, WITH deint): avg={:.2}, min={:.2}, max={:.2} (n={})",
                avg,
                deint_metrics.iter().cloned().fold(f32::MAX, f32::min),
                deint_metrics.iter().cloned().fold(f32::MIN, f32::max),
                deint_metrics.len()
            );
        }
    }

    /// Brute-force diagnostic: try all protection levels, polarities,
    /// and deinterleave modes to find which combination produces valid
    /// Firecode CRCs.
    #[test]
    #[ignore]
    fn msc_brute_force_params() {
        use audio::firecode_check;
        use protocol::ensemble::{Component, ProtectionLevel, ServiceType};

        let iq_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../..",
            "/testdata/dab_13b_2min.raw"
        );
        if !std::path::Path::new(iq_path).exists() {
            eprintln!("SKIP: {iq_path} not found");
            return;
        }
        let raw = std::fs::read(iq_path).unwrap();
        let samples: Vec<num_complex::Complex32> = raw
            .chunks_exact(2)
            .map(|c| {
                num_complex::Complex32::new(
                    (c[0] as f32 - 127.5) / 127.5,
                    (c[1] as f32 - 127.5) / 127.5,
                )
            })
            .collect();

        let mut ofdm = ofdm::OfdmProcessor::new();
        let mut fic = FicDecoder::new();
        let vit = ViterbiDecoder::new(128);

        // Collect frames
        let mut all_frames = Vec::new();
        let mut first_component: Option<Component> = None;

        let chunk_size = 2048 * 100;
        for chunk_start in (0..samples.len()).step_by(chunk_size) {
            let chunk_end = (chunk_start + chunk_size).min(samples.len());
            let chunk = &samples[chunk_start..chunk_end];
            for frame in ofdm.push_samples(chunk) {
                fic.begin_frame();
                for sym in frame.soft_bits.get(0..3).unwrap_or_default() {
                    fic.process_symbol(sym);
                }
                let ens = fic.handler.ensemble();
                if first_component.is_none() {
                    // Prefer DAB+ service
                    for svc in &ens.services {
                        if let Some(comp) = svc.components.first() {
                            if comp.size > 0 && comp.service_type == ServiceType::DabPlus {
                                first_component = Some(comp.clone());
                                eprintln!(
                                    "Selected: {:04X} {:?} (start={}, size={}, prot={:?})",
                                    svc.id,
                                    svc.label,
                                    comp.start_address,
                                    comp.size,
                                    comp.protection
                                );
                                break;
                            }
                        }
                    }
                }
                all_frames.push(frame);
            }
        }

        let comp = first_component.expect("no DAB+ service found");
        eprintln!("Total frames: {}", all_frames.len());

        // Try each protection level (EEP-A 1-4) and polarity (normal/inverted)
        for eep_level in 1u8..=4 {
            // Check if subchannel size is compatible with this level
            let (l1, l2, _pi1, _pi2) = fec::eep_a_params(comp.size, eep_level);
            if l1 == 0 && l2 == 0 {
                continue;
            }

            for invert in [false, true] {
                let label = format!(
                    "EEP-A{} {}",
                    eep_level,
                    if invert { "INVERTED" } else { "normal" }
                );

                // Decode without time deinterleaving
                let mut pass = 0usize;
                let mut fail = 0usize;
                let mut dab_plus_buf = Vec::new();

                for frame in &all_frames {
                    let msc_symbols = frame.soft_bits.get(3..).unwrap_or_default();
                    for cif_syms in msc_symbols.chunks(18) {
                        if cif_syms.len() < 18 {
                            continue;
                        }
                        let cif_soft: Vec<f32> =
                            cif_syms.iter().flat_map(|s| s.iter().copied()).collect();

                        let start_bit = comp.start_address as usize * 64;
                        let end_bit = start_bit + comp.size as usize * 64;
                        if end_bit > cif_soft.len() {
                            continue;
                        }
                        let mut subchannel_soft: Vec<f32> = cif_soft[start_bit..end_bit].to_vec();

                        if invert {
                            for v in &mut subchannel_soft {
                                *v = -*v;
                            }
                        }

                        // Normalize
                        let max_abs = subchannel_soft
                            .iter()
                            .map(|v| v.abs())
                            .fold(0.0f32, f32::max);
                        let scale = if max_abs > 0.0 { 1.0 / max_abs } else { 1.0 };
                        let normalized: Vec<f32> =
                            subchannel_soft.iter().map(|v| v * scale).collect();

                        // Depuncture with this level
                        let test_comp = Component {
                            subchannel_id: comp.subchannel_id,
                            service_type: comp.service_type.clone(),
                            start_address: comp.start_address,
                            size: comp.size,
                            protection: ProtectionLevel::EepA(eep_level),
                        };
                        let depunct = eep_depuncture(&normalized, &test_comp);
                        let bits = vit.decode(&depunct);
                        let info_len = bits.len().saturating_sub(6);
                        let mut data = pack_bits(&bits[..info_len]);
                        energy_dedispersal(&mut data);

                        dab_plus_buf.extend_from_slice(&data);
                    }
                }

                // Check Firecode at all possible alignments
                let cif_size = {
                    let total_mother = (l1 + l2) * 128 + 24;
                    (total_mother / 4 - 6) / 8
                };
                if cif_size > 0 {
                    let sf_size = cif_size * 5;
                    for offset in 0..5 {
                        let start = offset * cif_size;
                        let mut local_pass = 0;
                        let mut local_fail = 0;
                        let mut idx = start;
                        while idx + sf_size <= dab_plus_buf.len() {
                            if firecode_check(&dab_plus_buf[idx..idx + sf_size]) {
                                local_pass += 1;
                            } else {
                                local_fail += 1;
                            }
                            idx += sf_size;
                        }
                        pass += local_pass;
                        fail += local_fail;
                    }
                }

                eprintln!("{}: Firecode pass={}, fail={}", label, pass, fail);
            }
        }

        // Also try: decode first few CIFs and print Viterbi metrics for each level
        eprintln!("\nViterbi metrics per protection level (first 20 CIFs):");
        for eep_level in 1u8..=4 {
            let mut metrics = Vec::new();
            let mut count = 0;
            for frame in &all_frames {
                let msc_symbols = frame.soft_bits.get(3..).unwrap_or_default();
                for cif_syms in msc_symbols.chunks(18) {
                    if cif_syms.len() < 18 || count >= 20 {
                        continue;
                    }
                    count += 1;
                    let cif_soft: Vec<f32> =
                        cif_syms.iter().flat_map(|s| s.iter().copied()).collect();

                    let start_bit = comp.start_address as usize * 64;
                    let end_bit = start_bit + comp.size as usize * 64;
                    if end_bit > cif_soft.len() {
                        continue;
                    }
                    let subchannel_soft = &cif_soft[start_bit..end_bit];

                    let max_abs = subchannel_soft
                        .iter()
                        .map(|v| v.abs())
                        .fold(0.0f32, f32::max);
                    let scale = if max_abs > 0.0 { 1.0 / max_abs } else { 1.0 };
                    let normalized: Vec<f32> = subchannel_soft.iter().map(|v| v * scale).collect();

                    let test_comp = Component {
                        subchannel_id: comp.subchannel_id,
                        service_type: comp.service_type.clone(),
                        start_address: comp.start_address,
                        size: comp.size,
                        protection: ProtectionLevel::EepA(eep_level),
                    };
                    let depunct = eep_depuncture(&normalized, &test_comp);
                    let (_, metric) = vit.decode_with_metric(&depunct);
                    metrics.push(metric);
                }
            }
            if !metrics.is_empty() {
                let avg: f32 = metrics.iter().sum::<f32>() / metrics.len() as f32;
                eprintln!(
                    "  EEP-A{}: avg_metric={:.2}, min={:.2}, max={:.2}",
                    eep_level,
                    avg,
                    metrics.iter().cloned().fold(f32::MAX, f32::min),
                    metrics.iter().cloned().fold(f32::MIN, f32::max),
                );
            }
        }
    }

    /// Test: does MSC work with interleaved layout?
    /// Converts symbols from split [Re(0..K), Im(0..K)] to interleaved
    /// [Im(0), Re(0), Im(1), Re(1), ...] before CIF assembly.
    #[test]
    #[ignore]
    fn msc_interleaved_layout() {
        use num_complex::Complex32;

        let iq_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../..",
            "/testdata/dab_13b_2min.raw"
        );
        if !std::path::Path::new(iq_path).exists() {
            eprintln!("SKIP: {iq_path} not found");
            return;
        }
        let raw = std::fs::read(iq_path).unwrap();
        let samples: Vec<Complex32> = raw
            .chunks_exact(2)
            .map(|c| Complex32::new((c[0] as f32 - 127.5) / 127.5, (c[1] as f32 - 127.5) / 127.5))
            .collect();

        let mut ofdm = OfdmProcessor::new();
        let mut fic = FicDecoder::new();
        let vit = ViterbiDecoder::new(128);

        let chunk_size = 65536;
        let max_samples = 10 * 2_048_000;
        let limit = samples.len().min(max_samples);

        let mut first_sid: Option<u32> = None;
        let mut comp_info: Option<protocol::ensemble::Component> = None;

        // Time deinterleaver state (manual, for interleaved-layout CIF)
        let mut deint_buf: Vec<Vec<f32>> = Vec::new();
        let mut deint_count: usize = 0;
        let mut deint_bits: usize = 0;

        let mut decoded_cifs: Vec<Vec<u8>> = Vec::new();
        let mut frame_count = 0usize;

        /// Convert one symbol from split to interleaved layout.
        fn split_to_interleaved(split: &[f32]) -> Vec<f32> {
            let k = split.len() / 2; // NUM_CARRIERS = 1536
            let mut out = Vec::with_capacity(split.len());
            for i in 0..k {
                out.push(split[k + i]); // Im(i) first  (= d_{2i})
                out.push(split[i]); // Re(i) second (= d_{2i+1})
            }
            out
        }

        for chunk_start in (0..limit).step_by(chunk_size) {
            let chunk_end = (chunk_start + chunk_size).min(limit);
            for frame in ofdm.push_samples(&samples[chunk_start..chunk_end]) {
                frame_count += 1;
                fic.begin_frame();
                for sym in frame.soft_bits.get(0..3).unwrap_or_default() {
                    fic.process_symbol(sym);
                }
                let ens = fic.handler.ensemble();

                if first_sid.is_none() {
                    for svc in &ens.services {
                        if let Some(comp) = svc.components.first() {
                            if comp.size > 0 && svc.is_dab_plus {
                                first_sid = Some(svc.id);
                                comp_info = Some(comp.clone());
                                eprintln!(
                                    "Selected: {:04X} {:?} (start={}, size={}, prot={:?})",
                                    svc.id,
                                    svc.label,
                                    comp.start_address,
                                    comp.size,
                                    comp.protection
                                );
                                break;
                            }
                        }
                    }
                }

                if let Some(ref component) = comp_info {
                    let msc_symbols = frame.soft_bits.get(3..).unwrap_or_default();
                    for cif_syms in msc_symbols.chunks(18) {
                        if cif_syms.len() < 18 {
                            continue;
                        }
                        // Convert each symbol to interleaved layout, then flatten into CIF
                        let cif_soft: Vec<f32> = cif_syms
                            .iter()
                            .flat_map(|s| split_to_interleaved(s))
                            .collect();

                        // Extract subchannel
                        let start_bit = component.start_address as usize * 64;
                        let end_bit = start_bit + component.size as usize * 64;
                        if end_bit > cif_soft.len() {
                            continue;
                        }
                        let subchannel_soft = &cif_soft[start_bit..end_bit];
                        let bits_per_cif = subchannel_soft.len();

                        // Time deinterleave (manual implementation)
                        if bits_per_cif != deint_bits {
                            deint_bits = bits_per_cif;
                            deint_buf = vec![vec![0.0f32; bits_per_cif]; 16];
                            deint_count = 0;
                        }
                        let slot = deint_count % 16;
                        deint_buf[slot] = subchannel_soft.to_vec();
                        deint_count += 1;

                        if deint_count < 16 {
                            continue;
                        }

                        let p = deint_count - 1;
                        let deint_soft: Vec<f32> = (0..bits_per_cif)
                            .map(|i| {
                                let source_cif = p - 15 + TIME_INTERLEAVE_PI[i % 16];
                                let source_slot = source_cif % 16;
                                deint_buf[source_slot][i]
                            })
                            .collect();

                        // Normalize
                        let max_abs = deint_soft.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
                        let scale = if max_abs > 0.0 { 1.0 / max_abs } else { 1.0 };
                        let normalized: Vec<f32> = deint_soft.iter().map(|v| v * scale).collect();

                        // Depuncture + Viterbi
                        let depunct = eep_depuncture(&normalized, component);
                        let bits = vit.decode(&depunct);
                        let info_len = bits.len().saturating_sub(6);
                        let mut data = pack_bits(&bits[..info_len]);
                        energy_dedispersal(&mut data);

                        decoded_cifs.push(data);
                    }
                }
            }
        }

        eprintln!(
            "Frames: {}, decoded CIFs: {}, per_cif: {} bytes",
            frame_count,
            decoded_cifs.len(),
            decoded_cifs.first().map_or(0, |c| c.len())
        );

        if decoded_cifs.is_empty() {
            eprintln!("No decoded CIFs!");
            return;
        }

        let per_cif = decoded_cifs[0].len();

        // Print first bytes of first 10 decoded CIFs
        for (i, cif) in decoded_cifs.iter().enumerate().take(10) {
            let hdr: Vec<String> = cif.iter().take(8).map(|b| format!("{:02X}", b)).collect();
            eprintln!("  CIF {}: [{}]", i, hdr.join(" "));
        }

        // Check Firecode on sliding windows of 5 consecutive CIFs
        let mut pass = 0;
        let mut fail = 0;
        for i in 0..decoded_cifs.len().saturating_sub(4) {
            let mut superframe = Vec::with_capacity(per_cif * 5);
            for j in 0..5 {
                superframe.extend_from_slice(&decoded_cifs[i + j]);
            }
            if audio::firecode_check(&superframe) {
                pass += 1;
                eprintln!(
                    "  FIRECODE PASS at CIF {}, bytes: {:02X} {:02X} {:02X} {:02X}",
                    i, superframe[0], superframe[1], superframe[2], superframe[3]
                );
            } else {
                fail += 1;
            }
        }
        eprintln!(
            "Firecode (INTERLEAVED layout): pass={}, fail={}",
            pass, fail
        );
    }

    /// Focused diagnostic: decode MSC with time deinterleaving and check
    /// Firecode CRC on sliding windows of 5 consecutive CIFs.
    #[test]
    #[ignore]
    fn msc_deinterleaved_firecode() {
        use num_complex::Complex32;

        let iq_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../..",
            "/testdata/dab_13b_2min.raw"
        );
        if !std::path::Path::new(iq_path).exists() {
            eprintln!("SKIP: {iq_path} not found");
            return;
        }
        let raw = std::fs::read(iq_path).unwrap();
        let samples: Vec<Complex32> = raw
            .chunks_exact(2)
            .map(|c| Complex32::new((c[0] as f32 - 127.5) / 127.5, (c[1] as f32 - 127.5) / 127.5))
            .collect();

        let mut ofdm = OfdmProcessor::new();
        let mut fic = FicDecoder::new();
        let mut msc = MscDecoder::new();

        let chunk_size = 65536;
        let max_samples = 10 * 2_048_000; // 10 seconds
        let limit = samples.len().min(max_samples);

        let mut first_sid: Option<u32> = None;
        let mut decoded_cifs: Vec<Vec<u8>> = Vec::new();
        let mut frame_count = 0usize;

        for chunk_start in (0..limit).step_by(chunk_size) {
            let chunk_end = (chunk_start + chunk_size).min(limit);
            for frame in ofdm.push_samples(&samples[chunk_start..chunk_end]) {
                frame_count += 1;
                fic.begin_frame();
                for sym in frame.soft_bits.get(0..3).unwrap_or_default() {
                    fic.process_symbol(sym);
                }
                let ens = fic.handler.ensemble();

                if first_sid.is_none() {
                    for svc in &ens.services {
                        if let Some(comp) = svc.components.first() {
                            if comp.size > 0 && svc.is_dab_plus {
                                first_sid = Some(svc.id);
                                msc.set_target_sid(svc.id);
                                eprintln!(
                                    "Selected: {:04X} {:?} (start={}, size={}, prot={:?})",
                                    svc.id,
                                    svc.label,
                                    comp.start_address,
                                    comp.size,
                                    comp.protection
                                );
                                break;
                            }
                        }
                    }
                }

                if let Some(sid) = first_sid {
                    let msc_symbols = frame.soft_bits.get(3..).unwrap_or_default();
                    for (cif_idx, cif_syms) in msc_symbols.chunks(18).enumerate() {
                        if cif_syms.len() < 18 {
                            continue;
                        }
                        let cif_soft: Vec<f32> =
                            cif_syms.iter().flat_map(|s| s.iter().copied()).collect();
                        if let Some(component) = find_component(ens, sid) {
                            if let Some(af) = msc.process_cif(&cif_soft, component, cif_idx) {
                                decoded_cifs.push(af.data);
                            }
                        }
                    }
                }
            }
        }

        eprintln!(
            "Frames: {}, decoded CIFs: {}, per_cif: {} bytes",
            frame_count,
            decoded_cifs.len(),
            decoded_cifs.first().map_or(0, |c| c.len())
        );

        if decoded_cifs.is_empty() {
            eprintln!("No decoded CIFs!");
            return;
        }

        let per_cif = decoded_cifs[0].len();

        // Print first bytes of first 20 decoded CIFs
        for (i, cif) in decoded_cifs.iter().enumerate().take(20) {
            let hdr: Vec<String> = cif.iter().take(8).map(|b| format!("{:02X}", b)).collect();
            eprintln!("  CIF {}: [{}]", i, hdr.join(" "));
        }

        // Check Firecode on sliding windows of 5 consecutive CIFs
        let mut pass = 0;
        let mut fail = 0;
        for i in 0..decoded_cifs.len().saturating_sub(4) {
            let mut superframe = Vec::with_capacity(per_cif * 5);
            for j in 0..5 {
                superframe.extend_from_slice(&decoded_cifs[i + j]);
            }
            let ok = audio::firecode_check(&superframe);
            if ok {
                pass += 1;
                eprintln!(
                    "  FIRECODE PASS at CIF offset {} (CIFs {}-{}), first bytes: {:02X} {:02X} {:02X} {:02X}",
                    i, i, i + 4,
                    superframe[0], superframe[1], superframe[2], superframe[3]
                );
            } else {
                fail += 1;
            }
        }
        eprintln!("Firecode sliding window: pass={}, fail={}", pass, fail);

        // Also try without energy de-dispersal: decode manually
        eprintln!("\n--- Trying without energy de-dispersal ---");
        let mut undispersed: Vec<Vec<u8>> = Vec::new();
        for cif in &decoded_cifs {
            let mut data = cif.clone();
            // XOR with PRBS again to undo de-dispersal (double application = identity)
            energy_dedispersal(&mut data);
            undispersed.push(data);
        }
        let mut pass2 = 0;
        for i in 0..undispersed.len().saturating_sub(4) {
            let mut superframe = Vec::with_capacity(per_cif * 5);
            for j in 0..5 {
                superframe.extend_from_slice(&undispersed[i + j]);
            }
            if audio::firecode_check(&superframe) {
                pass2 += 1;
                eprintln!("  FIRECODE PASS (no de-dispersal) at CIF offset {}", i);
            }
        }
        eprintln!("Without de-dispersal: pass={}", pass2);
    }

    /// End-to-end DAB+ audio decode: IQ file → OFDM → FIC → MSC → fdk-aac.
    ///
    /// Verifies that the full pipeline produces non-empty PCM audio from the
    /// test IQ recording using the fdk-aac HE-AAC v2 decoder.
    #[test]
    #[ignore] // slow: processes full IQ capture; run with --ignored --test-threads=1
    fn dab_plus_audio_decode_from_iq_file() {
        use audio::DabPlusDecoder;
        use num_complex::Complex32;

        let iq_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../..",
            "/testdata/dab_13b_2min.raw"
        );
        if !std::path::Path::new(iq_path).exists() {
            eprintln!("Skipping: IQ file not found at {iq_path}");
            return;
        }

        let raw = std::fs::read(iq_path).unwrap();
        let samples: Vec<Complex32> = raw
            .chunks_exact(2)
            .map(|c| Complex32::new((c[0] as f32 - 127.5) / 127.5, (c[1] as f32 - 127.5) / 127.5))
            .collect();
        eprintln!(
            "Loaded {} IQ samples ({:.1} s)",
            samples.len(),
            samples.len() as f64 / 2_048_000.0
        );

        let mut ofdm = OfdmProcessor::new();
        let mut fic = FicDecoder::new();
        let mut msc = MscDecoder::new();
        let mut dab_plus_dec: Option<DabPlusDecoder> = None;

        let chunk_size = 65536;
        let limit = samples.len();

        let mut first_service_sid: Option<u32> = None;
        let mut total_pcm_samples = 0usize;
        let mut superframes_decoded = 0usize;
        let mut frame_count = 0usize;

        for chunk_start in (0..limit).step_by(chunk_size) {
            let chunk_end = (chunk_start + chunk_size).min(limit);
            let chunk = &samples[chunk_start..chunk_end];

            for frame in ofdm.push_samples(chunk) {
                frame_count += 1;

                // FIC
                fic.begin_frame();
                for sym in frame.soft_bits.get(0..3).unwrap_or_default() {
                    fic.process_symbol(sym);
                }

                let ens = fic.handler.ensemble();

                // Pick METRO (SId=0xF695) for testing.
                if first_service_sid.is_none() {
                    for svc in &ens.services {
                        if svc.id == 0xF695 {
                            if let Some(comp) = svc.components.first() {
                                if comp.size > 0 {
                                    first_service_sid = Some(svc.id);
                                    msc.set_target_sid(svc.id);
                                    eprintln!(
                                        "Selected DAB+ service: {:04X} {:?} (start={}, size={}, prot={:?})",
                                        svc.id, svc.label, comp.start_address, comp.size, comp.protection
                                    );
                                }
                            }
                            break;
                        }
                    }
                }

                // MSC → DAB+ audio decode
                if let Some(sid) = first_service_sid {
                    let msc_symbols = frame.soft_bits.get(3..).unwrap_or_default();
                    for (cif_idx, cif_syms) in msc_symbols.chunks(18).enumerate() {
                        if cif_syms.len() < 18 {
                            continue;
                        }
                        let cif_soft: Vec<f32> =
                            cif_syms.iter().flat_map(|s| s.iter().copied()).collect();
                        if let Some(component) = find_component(ens, sid) {
                            if let Some(af) = msc.process_cif(&cif_soft, component, cif_idx) {
                                let dec = dab_plus_dec
                                    .get_or_insert_with(|| DabPlusDecoder::new(af.data.len()));

                                let pcm = dec.push(&af.data);
                                if !pcm.is_empty() {
                                    superframes_decoded += 1;
                                    total_pcm_samples += pcm.len();

                                    let max_abs =
                                        pcm.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
                                    assert!(
                                        max_abs <= 1.5,
                                        "PCM values out of range: max_abs={}",
                                        max_abs
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }

        eprintln!(
            "Results: frames={}, superframes_decoded={}, total_pcm_samples={}",
            frame_count, superframes_decoded, total_pcm_samples
        );

        assert!(
            first_service_sid.is_some(),
            "should discover at least one DAB+ service"
        );
        assert!(
            superframes_decoded > 0,
            "should decode at least one DAB+ superframe to PCM audio"
        );
        assert!(
            total_pcm_samples > 10000,
            "should produce substantial PCM output (got {})",
            total_pcm_samples
        );
    }
}
