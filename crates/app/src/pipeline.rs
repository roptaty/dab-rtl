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
use fec::depuncturer::PUNCT_VECTORS;
use fec::{depuncture, ViterbiDecoder};
use ofdm::OfdmProcessor;
use protocol::{
    ensemble::{Component, ProtectionLevel},
    Ensemble, FicHandler,
};
use sdr::DeviceConfig;

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

/// Start the receive pipeline in background threads.
///
/// Returns a `PipelineHandle` for the caller (TUI) to communicate with.
pub fn start(
    device_config: DeviceConfig,
    audio_device: Option<String>,
) -> Result<PipelineHandle, String> {
    // Channel: background → TUI updates
    let (update_tx, update_rx) = mpsc::sync_channel::<PipelineUpdate>(32);
    // Channel: TUI → background commands
    let (cmd_tx, cmd_rx) = mpsc::sync_channel::<PipelineCmd>(8);
    // Shared command state so the inner SDR loop can check it.
    let cmd_rx = Arc::new(Mutex::new(cmd_rx));

    // Open SDR stream (produces Vec<Complex32> buffers).
    let iq_rx = sdr::open_stream(device_config, 32_768).map_err(|e| e.to_string())?;

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
            run_pipeline(iq_rx, audio_out, update_tx, cmd_rx);
        })
        .map_err(|e| e.to_string())?;

    Ok(PipelineHandle { update_rx, cmd_tx })
}

// ─────────────────────────────────────────────────────────────────────────── //
//  Pipeline main loop                                                          //
// ─────────────────────────────────────────────────────────────────────────── //

fn run_pipeline(
    iq_rx: mpsc::Receiver<Vec<num_complex::Complex32>>,
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

    let _ = update_tx.try_send(PipelineUpdate::Status("Hunting for signal…".into()));

    for iq_buf in iq_rx.iter() {
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
            // ── FIC (symbols 0-2) ────────────────────────────────────────── //
            let fic_symbols = frame.soft_bits.get(0..3).unwrap_or_default();
            for sym in fic_symbols {
                fic.process_symbol(sym);
            }

            // Propagate ensemble changes to the TUI.
            let ens = fic.handler.ensemble();
            if ens.label != last_ens_label || ens.services.len() != last_svc_count {
                last_ens_label = ens.label.clone();
                last_svc_count = ens.services.len();
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
                        if let Some(component) = find_component(&ens_snap, sid) {
                            // Update DAB+ superframe size when component info arrives.
                            if component.service_type == protocol::ServiceType::DabPlus {
                                let sf_bytes = component.size as usize * 8; // CUs × 64 bits / 8
                                if dab_plus.superframe_size != sf_bytes && sf_bytes > 0 {
                                    dab_plus.set_superframe_size(sf_bytes);
                                }
                            }

                            if let Some(frame) = msc.process_cif(&cif_soft, component, cif_idx) {
                                let pcm = if frame.is_dab_plus {
                                    dab_plus.push(&frame.data)
                                } else {
                                    mp2.push(&frame.data)
                                };
                                if let (Some(ao), false) = (&audio_out, pcm.is_empty()) {
                                    ao.write_samples(&pcm);
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
pub struct FicDecoder {
    viterbi: ViterbiDecoder,
    pub handler: FicHandler,
}

impl FicDecoder {
    pub fn new() -> Self {
        FicDecoder {
            viterbi: ViterbiDecoder::new(5 * fec::viterbi::K),
            handler: FicHandler::new(),
        }
    }

    /// Decode one FIC OFDM symbol (3072 soft bits).
    ///
    /// Each FIC symbol encodes 3 FIBs (768 info bits) at mother code rate 1/4.
    /// We append 24 tail erasures (6 flush bits × 4 mother-code outputs) and
    /// run Viterbi, then take the first 768 decoded bits = 96 bytes = 3 FIBs.
    pub fn process_symbol(&mut self, soft: &[f32]) {
        const INFO_BITS: usize = 768;
        const TAIL_ERASURES: usize = 24; // 6 flush bits × 4 generator polynomials

        // Append tail erasures.
        let mut padded = Vec::with_capacity(soft.len() + TAIL_ERASURES);
        padded.extend_from_slice(soft);
        padded.extend(std::iter::repeat_n(0.0f32, TAIL_ERASURES));

        let bits = self.viterbi.decode(&padded);
        let info = &bits[..bits.len().min(INFO_BITS)];

        // Pack bits MSB-first → 96 bytes.
        let fic_bytes = pack_bits(info);
        self.handler.process_fic_bytes(&fic_bytes);
    }
}

// ─────────────────────────────────────────────────────────────────────────── //
//  MSC decoder                                                                 //
// ─────────────────────────────────────────────────────────────────────────── //

pub struct MscDecoder {
    target_sid: Option<u32>,
    viterbi: ViterbiDecoder,
}

impl MscDecoder {
    pub fn new() -> Self {
        MscDecoder {
            target_sid: None,
            viterbi: ViterbiDecoder::new(128),
        }
    }

    pub fn set_target_sid(&mut self, sid: u32) {
        self.target_sid = Some(sid);
    }

    pub fn clear_target(&mut self) {
        self.target_sid = None;
    }

    /// Decode one CIF (55296 soft bits) for the given component.
    ///
    /// Returns `None` if no target SId is set or if the subchannel range
    /// falls outside the CIF buffer.
    pub fn process_cif(
        &self,
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

        // Apply EEP depuncturing.
        let punct_vec = eep_punct_vector(&component.protection);
        let depunct = depuncture(subchannel_soft, punct_vec);

        // Viterbi decode.
        let bits = self.viterbi.decode(&depunct);

        // Pack to bytes.
        let data = pack_bits(&bits);

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

/// Return the EEP puncturing vector for a given protection level.
///
/// This uses a single-region approximation.  The ETSI two-region scheme
/// (ETSI EN 300 401 Table 8a/8b) can be substituted once verified.
///
/// ETSI protection levels: 1 = strongest (lowest code rate, most redundancy),
/// 4 = weakest (highest code rate, least redundancy).
/// Higher PI number = more bits kept = less puncturing.
fn eep_punct_vector(protection: &ProtectionLevel) -> &'static [u8; 32] {
    match protection {
        // Level 1 (strongest) → PI_24 (no puncturing, full rate-1/4)
        ProtectionLevel::EepA(1) | ProtectionLevel::EepB(1) => &PUNCT_VECTORS[23], // PI_24
        // Level 2 → PI_14 (22 ones)
        ProtectionLevel::EepA(2) | ProtectionLevel::EepB(2) => &PUNCT_VECTORS[13], // PI_14
        // Level 3 → PI_8 (16 ones)
        ProtectionLevel::EepA(3) | ProtectionLevel::EepB(3) => &PUNCT_VECTORS[7], // PI_8
        // Level 4 (weakest) → PI_3 (11 ones)
        ProtectionLevel::EepA(4) | ProtectionLevel::EepB(4) => &PUNCT_VECTORS[2], // PI_3
        ProtectionLevel::Uep(level) => {
            // Map UEP levels 1-5 to reasonable PI indices
            let idx = match level {
                1 => 23, // strongest → PI_24
                2 => 15, // PI_16
                3 => 7,  // PI_8
                4 => 3,  // PI_4
                _ => 1,  // weakest → PI_2
            };
            &PUNCT_VECTORS[idx]
        }
        _ => &PUNCT_VECTORS[23], // safe default: no puncturing (full rate)
    }
}

/// Find the first audio component for a service in the ensemble.
fn find_component(ens: &Ensemble, sid: u32) -> Option<&Component> {
    ens.services
        .iter()
        .find(|s| s.id == sid)?
        .components
        .first()
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

    #[test]
    fn msc_decoder_no_target_returns_none() {
        let dec = MscDecoder::new();
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
}
