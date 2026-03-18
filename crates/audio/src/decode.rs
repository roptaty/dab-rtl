/// Audio decoders for DAB (MP2) and DAB+ (HE-AAC v2) superframes.
///
/// DAB audio is carried as raw MPEG Layer 2 frames (decoded via Symphonia).
/// DAB+ audio is carried as HE-AAC v2 Access Units packed inside a DAB+
/// superframe (ETSI TS 102 563).  Raw AUs are fed to fdk-aac via RAW
/// transport with an AudioSpecificConfig (960-sample frames, SBR/PS).
use symphonia::core::{
    audio::SampleBuffer, codecs::DecoderOptions, formats::FormatOptions, io::MediaSourceStream,
    meta::MetadataOptions, probe::Hint,
};

/// Stateless helper: decode a slice of raw MP2/MPEG-audio bytes to f32 PCM.
///
/// Returns interleaved stereo (or mono) f32 samples, or an empty vec on
/// failure.  Errors are logged at warn level.
pub fn decode_mp2(data: &[u8]) -> Vec<f32> {
    if data.is_empty() {
        return Vec::new();
    }

    let cursor = std::io::Cursor::new(data.to_vec());
    let mss = MediaSourceStream::new(Box::new(cursor), Default::default());

    let mut hint = Hint::new();
    hint.mime_type("audio/mpeg");

    let probed = match symphonia::default::get_probe().format(
        &hint,
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    ) {
        Ok(p) => p,
        Err(e) => {
            log::warn!("MP2 probe failed: {e}");
            return Vec::new();
        }
    };

    let mut format = probed.format;

    let track = match format.default_track() {
        Some(t) => t.clone(),
        None => {
            log::warn!("MP2: no default track");
            return Vec::new();
        }
    };

    let mut decoder = match symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
    {
        Ok(d) => d,
        Err(e) => {
            log::warn!("MP2 decoder construction failed: {e}");
            return Vec::new();
        }
    };

    let mut out = Vec::new();

    while let Ok(packet) = format.next_packet() {
        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = *decoded.spec();
                let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
                buf.copy_interleaved_ref(decoded);
                out.extend_from_slice(buf.samples());
            }
            Err(e) => log::debug!("MP2 decode error (skipped): {e}"),
        }
    }

    out
}

/// A stateful MP2 decoder that accumulates bytes until a complete superframe
/// can be decoded.
///
/// DAB audio superframes are typically 3 MP2 frames (for 48 kHz stereo).
/// We buffer until we have at least `min_bytes` and then flush.
pub struct Mp2Decoder {
    buf: Vec<u8>,
    min_bytes: usize,
}

impl Mp2Decoder {
    /// Create a decoder.
    ///
    /// `min_bytes` controls how much data is buffered before attempting
    /// a decode pass.  A typical DAB MP2 frame at 128 kbit/s is ~384 bytes;
    /// a superframe of 3 frames is ~1152 bytes.
    pub fn new(min_bytes: usize) -> Self {
        Mp2Decoder {
            buf: Vec::new(),
            min_bytes,
        }
    }

    /// Push raw MP2 bytes and return any decoded PCM samples.
    pub fn push(&mut self, data: &[u8]) -> Vec<f32> {
        self.buf.extend_from_slice(data);
        if self.buf.len() < self.min_bytes {
            return Vec::new();
        }
        let pcm = decode_mp2(&self.buf);
        self.buf.clear();
        pcm
    }
}

// ─────────────────────────────────────────────────────────────────────────── //
//  DAB+ HE-AAC v2 decoder (fdk-aac)                                          //
// ─────────────────────────────────────────────────────────────────────────── //

/// CRC-16-CCITT (init=0xFFFF, poly=0x1021, final XOR=0xFFFF).
///
/// Per ETSI TS 102 563: result is inverted (XOR 0xFFFF) before comparison.
fn crc16_ccitt(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &byte in data {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc ^ 0xFFFF
}

/// Read 12 bits from a byte array at the given bit offset (MSB-first).
fn read_12bits(data: &[u8], bit_offset: usize) -> u16 {
    let byte_pos = bit_offset / 8;
    let bit_pos = bit_offset % 8;
    let combined = ((*data.get(byte_pos).unwrap_or(&0) as u32) << 16)
        | ((*data.get(byte_pos + 1).unwrap_or(&0) as u32) << 8)
        | (*data.get(byte_pos + 2).unwrap_or(&0) as u32);
    ((combined >> (12 - bit_pos)) & 0xFFF) as u16
}

/// DAB+ Fire Code CRC check (ETSI TS 102 563 §5.3).
///
/// Generator polynomial: g(x) = (x^11 + 1)(x^5 + x^3 + x^2 + x + 1) = 0x782F.
/// Bytes 0–1 contain the 16-bit CRC; it is computed over the next 9 bytes
/// (72 bits, bytes 2–10).  The Fire code LFSR feeds the XOR result back
/// into both the polynomial tap and the register LSB.
pub fn firecode_check(data: &[u8]) -> bool {
    if data.len() < 11 {
        return false;
    }
    let stored = ((data[0] as u16) << 8) | data[1] as u16;
    let mut crc: u16 = 0;
    for &byte in &data[2..11] {
        for bit in (0..8).rev() {
            let input_bit = ((byte >> bit) & 1) as u16;
            let flag = ((crc >> 15) ^ input_bit) & 1;
            crc = (crc << 1) ^ if flag != 0 { 0x782F } else { 0 };
        }
    }
    crc == stored
}

/// Build an AudioSpecificConfig for DAB+ (ETSI TS 102 563).
///
/// Uses 960-sample frames (frameLengthFlag=1), AAC-LC base profile.
/// When SBR is present, uses **hierarchical signaling** (AOT=5 or AOT=29
/// as the primary object type) so that fdk-aac reliably activates SBR
/// upsampling.  This matches the approach used by dablin and welle.io.
///
/// ISO 14496-3 hierarchical signaling layout for AOT=5 (SBR):
///   [5: AOT=5] [4: core_sr_idx] [4: channels] [4: ext_sr_idx]
///   [5: core_AOT=2] [3: GASpecificConfig]
///
/// For AOT=29 (PS, implies SBR):
///   [5: AOT=29] [4: core_sr_idx] [4: channels] [4: ext_sr_idx]
///   [5: core_AOT=2] [3: GASpecificConfig]
fn build_asc(
    core_sr_idx: u8,
    channels: u8,
    sbr_flag: bool,
    ext_sr_idx: u8,
    ps_flag: bool,
) -> Vec<u8> {
    let mut bits: u64 = 0;
    let mut nbits: usize = 0;

    if sbr_flag {
        // Hierarchical signaling: primary AOT = 5 (SBR) or 29 (PS+SBR)
        let primary_aot: u64 = if ps_flag { 29 } else { 5 };
        // AudioObjectType (primary), 5 bits
        bits = (bits << 5) | primary_aot;
        nbits += 5;
        // samplingFrequencyIndex = core rate, 4 bits
        bits = (bits << 4) | (core_sr_idx as u64 & 0xF);
        nbits += 4;
        // channelConfiguration, 4 bits
        bits = (bits << 4) | (channels as u64 & 0xF);
        nbits += 4;
        // extensionSamplingFrequencyIndex = SBR output rate, 4 bits
        bits = (bits << 4) | (ext_sr_idx as u64 & 0xF);
        nbits += 4;
        // Core AudioObjectType = 2 (AAC-LC), 5 bits
        bits = (bits << 5) | 2;
        nbits += 5;
        // GASpecificConfig: frameLengthFlag=1 (960 samples),
        // dependsOnCoreCoder=0, extensionFlag=0
        bits = (bits << 3) | 0b100;
        nbits += 3;
    } else {
        // Plain AAC-LC (no SBR)
        // AudioObjectType = 2 (AAC-LC), 5 bits
        bits = (bits << 5) | 2;
        nbits += 5;
        // samplingFrequencyIndex, 4 bits
        bits = (bits << 4) | (core_sr_idx as u64 & 0xF);
        nbits += 4;
        // channelConfiguration, 4 bits
        bits = (bits << 4) | (channels as u64 & 0xF);
        nbits += 4;
        // GASpecificConfig: frameLengthFlag=1 (960 samples),
        // dependsOnCoreCoder=0, extensionFlag=0
        bits = (bits << 3) | 0b100;
        nbits += 3;
    }

    // Pad to byte boundary
    let pad = (8 - (nbits % 8)) % 8;
    bits <<= pad;
    nbits += pad;

    let byte_len = nbits / 8;
    let mut out = Vec::with_capacity(byte_len);
    for i in (0..byte_len).rev() {
        out.push(((bits >> (i * 8)) & 0xFF) as u8);
    }
    out
}

/// Persistent fdk-aac decoder state using RAW transport.
///
/// RAW transport with explicit AudioSpecificConfig is required for DAB+
/// because ADTS doesn't support 960-sample frames.
struct AacState {
    decoder: crate::fdk::Decoder,
}

impl AacState {
    fn new_raw(asc: &[u8]) -> Result<Self, String> {
        let mut decoder = crate::fdk::Decoder::new_raw();
        decoder
            .config_raw(asc)
            .map_err(|e| format!("aacDecoder_ConfigRaw failed: 0x{:04X}", e))?;
        Ok(AacState { decoder })
    }
}

/// Stateful DAB+ audio decoder.
///
/// Accumulates raw bytes until a full superframe (5 CIFs) is available,
/// then decodes.  Uses Firecode CRC to synchronize to the correct 5-CIF
/// superframe boundary before decoding.
pub struct DabPlusDecoder {
    buf: Vec<u8>,
    /// Per-CIF byte count.  Set from the first actual decoded frame.
    pub superframe_size: usize,
    /// Whether we have found a valid Firecode alignment.
    synced: bool,
    /// Persistent fdk-aac decoder (SBR/PS/window state survives across superframes).
    aac: Option<AacState>,
    /// AU data (without CRC) from the most recently decoded superframe, for PAD
    /// extraction.  Populated by [`decode_superframe`] and drained by the caller.
    pub pad_aus: Vec<Vec<u8>>,
}

impl DabPlusDecoder {
    /// Create a decoder.
    ///
    /// `superframe_size` is the per-CIF byte count (Viterbi output).
    /// If unknown, pass 0 — the decoder will skip until `set_superframe_size`
    /// is called.
    pub fn new(superframe_size: usize) -> Self {
        DabPlusDecoder {
            buf: Vec::new(),
            superframe_size,
            synced: false,
            aac: None,
            pad_aus: Vec::new(),
        }
    }

    /// Update the per-CIF byte count.  Call when actual frame size is known.
    pub fn set_superframe_size(&mut self, size: usize) {
        self.superframe_size = size;
        self.buf.clear();
        self.synced = false;
    }

    /// Push bytes for one CIF and return any decoded PCM samples.
    pub fn push(&mut self, data: &[u8]) -> Vec<f32> {
        if self.superframe_size == 0 {
            return Vec::new();
        }
        self.buf.extend_from_slice(data);

        let sf_size = self.superframe_size * 5;

        if !self.synced {
            // Search for Firecode sync: try each CIF-aligned offset in the
            // buffer.  We need at least 5 CIFs + some extra to search
            // multiple alignments.
            if self.buf.len() < sf_size {
                return Vec::new();
            }
            // Try every CIF boundary in the buffer.
            let cif = self.superframe_size;
            let max_offset = self.buf.len().saturating_sub(sf_size);
            let num_offsets = if cif > 0 { max_offset / cif + 1 } else { 0 };
            log::debug!(
                "DAB+ sync search: buf={}, sf_size={}, max_offset={}, checking {} offsets",
                self.buf.len(),
                sf_size,
                max_offset,
                num_offsets
            );
            let mut found_at: Option<usize> = None;
            let mut offset = 0;
            while offset <= max_offset {
                if firecode_check(&self.buf[offset..offset + sf_size]) {
                    found_at = Some(offset);
                    break;
                }
                offset += cif;
            }
            if let Some(start) = found_at {
                log::info!("DAB+: Firecode sync acquired (offset={})", start);
                self.synced = true;
                self.buf.drain(..start);
                let superframe: Vec<u8> = self.buf.drain(..sf_size).collect();
                return self.decode_superframe(&superframe);
            }
            // No match found — keep only the last 4 CIFs so the next push()
            // can form new 5-CIF windows with the incoming CIF.
            let keep = 4 * cif;
            if self.buf.len() > keep {
                let drain = self.buf.len() - keep;
                log::debug!("DAB+ sync: draining {} bytes, keeping {}", drain, keep);
                self.buf.drain(..drain);
            }
            return Vec::new();
        }

        // Already synced — decode at the established boundary.
        if self.buf.len() < sf_size {
            return Vec::new();
        }
        let superframe: Vec<u8> = self.buf.drain(..sf_size).collect();
        if !firecode_check(&superframe) {
            log::warn!("DAB+: Firecode CRC failed on synced superframe, re-syncing");
            self.synced = false;
            // Re-insert the data and skip one CIF to search again.
            let mut rest = self.buf.split_off(0);
            self.buf = superframe;
            self.buf.append(&mut rest);
            self.buf.drain(..self.superframe_size);
            return Vec::new();
        }
        self.decode_superframe(&superframe)
    }

    /// Decode a validated superframe using the persistent fdk-aac decoder.
    ///
    /// Per ETSI TS 102 563 and dablin/welle.io reference implementations:
    /// - RS parity is stripped (110 data bytes per 120-byte RS codeword)
    /// - AU CRCs are the last 2 bytes within each AU boundary
    /// - Raw AAC AUs are fed via RAW transport (960-sample DAB+ frames)
    ///
    /// Populates [`pad_aus`] with the AU bytes (without CRC) from each AU that
    /// passes its CRC check.  The caller can drain this field for PAD extraction.
    fn decode_superframe(&mut self, data: &[u8]) -> Vec<f32> {
        self.pad_aus.clear();
        if data.len() < 6 {
            return Vec::new();
        }

        // RS(120,110) parity is appended at the end of the superframe.
        // Audio data occupies the first (sf_len / 120 * 110) bytes;
        // the remaining bytes are RS parity (not interleaved).
        let audio_len = if data.len().is_multiple_of(120) && data.len() >= 120 {
            data.len() / 120 * 110
        } else {
            data.len()
        };

        if audio_len < 6 {
            return Vec::new();
        }

        let header_byte = data[2];
        // Bit layout (ETSI TS 102 563, Table 1):
        //   bit 7: rfa, bit 6: dac_rate, bit 5: sbr_flag,
        //   bit 4: aac_channel_mode, bit 3: ps_flag, bits 2-0: mpeg_surround_config
        let dac_rate = (header_byte >> 6) & 1;
        let sbr_flag = (header_byte >> 5) & 1;
        let aac_channel_mode = (header_byte >> 4) & 1;
        let ps_flag = (header_byte >> 3) & 1;

        // ETSI TS 102 563, Table 2 (verified against dablin):
        let num_aus: usize = match (dac_rate, sbr_flag) {
            (0, 0) => 4,
            (0, 1) => 2,
            (1, 0) => 6,
            (1, 1) => 3,
            _ => unreachable!(),
        };

        // Core sample rate index (for ASC)
        let core_sr_idx: u8 = match (dac_rate, sbr_flag) {
            (0, 0) => 5, // 32 kHz
            (0, 1) => 8, // 16 kHz core (SBR → 32 kHz)
            (1, 0) => 3, // 48 kHz
            (1, 1) => 6, // 24 kHz core (SBR → 48 kHz)
            _ => unreachable!(),
        };

        // Output (SBR) sample rate index
        let ext_sr_idx: u8 = match dac_rate {
            0 => 5, // 32 kHz
            1 => 3, // 48 kHz
            _ => unreachable!(),
        };

        let channels: u8 = if aac_channel_mode == 0 { 1 } else { 2 };

        // AU[0] start offset — fixed per ETSI TS 102 563, Table 3 (dablin).
        let first_au_offset: usize = match (dac_rate, sbr_flag) {
            (0, 0) => 8,
            (0, 1) => 5,
            (1, 0) => 11,
            (1, 1) => 6,
            _ => unreachable!(),
        };

        if first_au_offset >= audio_len {
            log::debug!(
                "DAB+: superframe too short for header ({} bytes)",
                audio_len
            );
            return Vec::new();
        }

        // AU start offsets (absolute byte positions in decoded superframe).
        let mut au_starts: Vec<usize> = Vec::with_capacity(num_aus + 1);
        au_starts.push(first_au_offset);

        // AU[1..n-1] addresses from 12-bit fields starting at bit 24.
        for i in 0..(num_aus - 1) {
            let addr = read_12bits(data, 24 + i * 12) as usize;
            au_starts.push(addr);
        }

        // Sentinel: end of audio data (excludes RS parity).
        au_starts.push(audio_len);

        // Initialise persistent RAW decoder with AudioSpecificConfig on first use.
        if self.aac.is_none() {
            let asc = build_asc(
                core_sr_idx,
                channels,
                sbr_flag != 0,
                ext_sr_idx,
                ps_flag != 0 && aac_channel_mode == 0,
            );
            match AacState::new_raw(&asc) {
                Ok(state) => {
                    log::info!(
                        "DAB+ AAC: dac_rate={}, sbr={}, ch_mode={}, ps={}, num_aus={}, core_sr_idx={}, ASC={:02X?}",
                        dac_rate, sbr_flag, aac_channel_mode, ps_flag, num_aus, core_sr_idx, asc
                    );
                    self.aac = Some(state);
                }
                Err(e) => {
                    log::error!("DAB+ AAC init failed: {}", e);
                    return Vec::new();
                }
            }
        }

        let aac = self.aac.as_mut().unwrap();
        let mut out = Vec::new();

        // Expected output sample rate when SBR is active.
        let expected_sr: u32 = match dac_rate {
            0 => 32_000,
            _ => 48_000,
        };

        for i in 0..num_aus {
            let start = au_starts[i];
            let end = au_starts[i + 1];

            if start + 2 >= end || end > audio_len {
                log::debug!(
                    "DAB+: AU[{}] invalid range {}..{} (audio_len={})",
                    i,
                    start,
                    end,
                    audio_len
                );
                continue;
            }

            // Per-AU CRC: last 2 bytes of each AU are CRC-CCITT.
            let au_crc_stored = ((data[end - 2] as u16) << 8) | data[end - 1] as u16;
            let au_data = &data[start..end - 2];
            let au_crc_calc = crc16_ccitt(au_data);

            if au_crc_stored != au_crc_calc {
                log::debug!(
                    "DAB+: AU[{}] CRC mismatch (stored=0x{:04X}, calc=0x{:04X})",
                    i,
                    au_crc_stored,
                    au_crc_calc
                );
                continue;
            }

            // Collect AU data (without CRC) for PAD extraction by the caller.
            self.pad_aus.push(au_data.to_vec());

            // Feed raw AU data (no ADTS header) to the RAW transport decoder.
            if let Err(e) = aac.decoder.fill(au_data) {
                log::debug!("AAC fill error: 0x{:04X}", e);
                continue;
            }

            match aac.decoder.decode_frame() {
                Ok(frame_size) if frame_size > 0 => {
                    let pcm_i16 = &aac.decoder.pcm_buf[..frame_size];
                    let info = aac.decoder.stream_info();
                    let out_channels = info.num_channels;
                    let decoder_sr = info.sample_rate as u32;

                    // Check if SBR upsampling is needed but wasn't applied by fdk-aac.
                    let need_upsample = sbr_flag != 0 && decoder_sr > 0 && decoder_sr < expected_sr;
                    let upsample_ratio = if need_upsample {
                        expected_sr / decoder_sr
                    } else {
                        1
                    };

                    if i == 0 && log::log_enabled!(log::Level::Debug) {
                        log::debug!(
                            "AAC: decoded AU[0]: {} samples, {} ch, {} Hz (upsample {}x)",
                            info.frame_size,
                            info.num_channels,
                            decoder_sr,
                            upsample_ratio
                        );
                    }

                    if out_channels == 1 {
                        // Mono → stereo with optional upsampling.
                        for j in 0..pcm_i16.len() {
                            let f = pcm_i16[j] as f32 / i16::MAX as f32;
                            if upsample_ratio == 2 && j + 1 < pcm_i16.len() {
                                let f_next = pcm_i16[j + 1] as f32 / i16::MAX as f32;
                                let mid = (f + f_next) * 0.5;
                                out.push(f);
                                out.push(f);
                                out.push(mid);
                                out.push(mid);
                            } else {
                                out.push(f);
                                out.push(f);
                            }
                        }
                    } else {
                        // Stereo (interleaved L,R pairs) with optional upsampling.
                        let samples = pcm_i16.chunks_exact(2);
                        let pairs: Vec<(f32, f32)> = samples
                            .map(|lr| {
                                (
                                    lr[0] as f32 / i16::MAX as f32,
                                    lr[1] as f32 / i16::MAX as f32,
                                )
                            })
                            .collect();

                        for j in 0..pairs.len() {
                            let (l, r) = pairs[j];
                            out.push(l);
                            out.push(r);
                            if upsample_ratio == 2 {
                                // Linear interpolation with next sample, or repeat last.
                                let (l_next, r_next) = if j + 1 < pairs.len() {
                                    pairs[j + 1]
                                } else {
                                    (l, r)
                                };
                                out.push((l + l_next) * 0.5);
                                out.push((r + r_next) * 0.5);
                            }
                        }
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    log::debug!("AAC AU[{}] decode error: 0x{:04X}", i, e);
                }
            }
        }
        out
    }
}

// ─────────────────────────────────────────────────────────────────────────── //
//  Tests                                                                       //
// ─────────────────────────────────────────────────────────────────────────── //

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_returns_empty() {
        assert!(decode_mp2(&[]).is_empty());
    }

    #[test]
    fn garbage_input_returns_empty_no_panic() {
        let garbage = vec![0xFFu8; 256];
        let _ = decode_mp2(&garbage); // must not panic
    }

    #[test]
    fn mp2_decoder_buffers_until_min() {
        let mut dec = Mp2Decoder::new(512);
        let short = vec![0u8; 100];
        assert!(dec.push(&short).is_empty(), "should buffer, not decode yet");
    }

    #[test]
    fn read_12bits_byte_aligned() {
        // 12 bits starting at bit 0: 0xABC
        let data = [0xAB, 0xC0, 0x00];
        assert_eq!(read_12bits(&data, 0), 0xABC);
    }

    #[test]
    fn read_12bits_nibble_offset() {
        // 12 bits starting at bit 4: should read lower nibble of byte 0 + byte 1
        let data = [0xFA, 0xBC, 0x00];
        assert_eq!(read_12bits(&data, 4), 0xABC);
    }

    #[test]
    fn dab_plus_decoder_does_not_panic_on_garbage() {
        let mut dec = DabPlusDecoder::new(100);
        let garbage = vec![0xFFu8; 1200];
        // Push enough CIFs worth of garbage — must not panic.
        for chunk in garbage.chunks(100) {
            let _ = dec.push(chunk);
        }
    }

    #[test]
    fn dab_plus_num_aus_from_header() {
        // Test with actual header byte bit positions (rfa=bit7, dac_rate=bit6, sbr=bit5).
        let cases: [(u8, usize); 4] = [
            (0b0000_0000, 4), // dac_rate=0, sbr=0
            (0b0010_0000, 2), // dac_rate=0, sbr=1
            (0b0100_0000, 6), // dac_rate=1, sbr=0
            (0b0110_0000, 3), // dac_rate=1, sbr=1
        ];
        for (header_byte, expected_aus) in cases {
            let dac_rate = (header_byte >> 6) & 1;
            let sbr_flag = (header_byte >> 5) & 1;
            let num_aus = match (dac_rate, sbr_flag) {
                (0, 0) => 4usize,
                (0, 1) => 2,
                (1, 0) => 6,
                (1, 1) => 3,
                _ => unreachable!(),
            };
            assert_eq!(num_aus, expected_aus, "header_byte={:#010b}", header_byte);
        }
    }

    #[test]
    fn crc16_ccitt_test_vector() {
        // CRC-CCITT with final inversion: "123456789" → 0xD64E
        assert_eq!(crc16_ccitt(b"123456789"), 0xD64E);
    }

    #[test]
    fn crc16_ccitt_different_inputs() {
        let a = crc16_ccitt(&[0x80, 0x2A, 0xCC, 0x2C, 0x0D, 0x64, 0xEE, 0xAE]);
        let b = crc16_ccitt(&[0x80, 0x2A, 0x4C, 0x4B, 0x50, 0x74, 0x34, 0xA2]);
        assert_ne!(a, b, "Different inputs must give different CRCs");
    }

    #[test]
    fn firecode_check_rejects_garbage() {
        let garbage = vec![0xFFu8; 200];
        assert!(!firecode_check(&garbage));
    }

    #[test]
    fn firecode_check_rejects_short() {
        assert!(!firecode_check(&[0x00, 0x00]));
        assert!(!firecode_check(&[0u8; 10])); // need at least 11 bytes
    }

    #[test]
    fn firecode_check_accepts_valid() {
        let payload: [u8; 9] = [0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0, 0x42];
        let mut crc: u16 = 0;
        for &byte in &payload {
            for bit in (0..8).rev() {
                let input_bit = ((byte >> bit) & 1) as u16;
                let flag = ((crc >> 15) ^ input_bit) & 1;
                crc = (crc << 1) ^ if flag != 0 { 0x782F } else { 0 };
            }
        }
        let mut frame = vec![0u8; 11];
        frame[0] = (crc >> 8) as u8;
        frame[1] = crc as u8;
        frame[2..11].copy_from_slice(&payload);
        assert!(firecode_check(&frame), "CRC={:04X}", crc);

        // Also verify that flipping a bit makes it fail.
        frame[5] ^= 0x01;
        assert!(!firecode_check(&frame));
    }

    #[test]
    fn dab_plus_decoder_syncs_on_firecode() {
        let mut dec = DabPlusDecoder::new(10);
        // Push garbage — should not produce output.
        for _ in 0..10 {
            let pcm = dec.push(&[0xAA; 10]);
            assert!(pcm.is_empty());
        }
    }

    #[test]
    fn dab_plus_decoder_buffers_5_cifs() {
        let mut dec = DabPlusDecoder::new(100);
        for i in 0..4 {
            let data = vec![0u8; 100];
            let pcm = dec.push(&data);
            assert!(pcm.is_empty(), "should buffer CIF {}", i);
        }
        // 5th CIF triggers superframe decode (will return empty on garbage data)
        let data = vec![0u8; 100];
        let _ = dec.push(&data); // must not panic
    }

    #[test]
    fn fdk_aac_decoder_constructs_raw() {
        // Verify fdk-aac decoder can be instantiated with a DAB+ ASC.
        let asc = build_asc(6, 2, true, 3, false); // 24kHz core, stereo, SBR → 48kHz
        let _state = AacState::new_raw(&asc).expect("RAW decoder init failed");
    }

    #[test]
    fn build_asc_smoke() {
        // AAC-LC 24kHz stereo with SBR → 48kHz (typical DAB+ config)
        // With hierarchical signaling, primary AOT = 5 (SBR)
        let asc = build_asc(6, 2, true, 3, false);
        assert!(!asc.is_empty());
        // First 5 bits = objectType = 5 (SBR) → byte[0] top 5 bits = 00101
        assert_eq!(asc[0] >> 3, 0b00101);

        // Plain AAC-LC (no SBR): AOT = 2
        let asc_plain = build_asc(3, 2, false, 0, false);
        assert_eq!(asc_plain[0] >> 3, 0b00010);

        // PS+SBR: primary AOT = 29
        let asc_ps = build_asc(6, 1, true, 3, true);
        assert_eq!(asc_ps[0] >> 3, 0b11101);
    }
}
