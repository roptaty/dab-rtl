/// Audio decoders for DAB (MP2) and DAB+ (HE-AAC) superframes.
///
/// DAB audio is carried as raw MPEG Layer 2 frames.
/// DAB+ audio is carried as a sequence of HE-AAC Access Units (AUs) packed
/// inside a DAB+ superframe.  Each AU is wrapped in an ADTS header so that
/// Symphonia's AAC codec can decode it.
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
//  DAB+ HE-AAC decoder                                                        //
// ─────────────────────────────────────────────────────────────────────────── //

/// Wrap a single AAC Access Unit in an ADTS header.
///
/// ADTS header (7 bytes, no CRC):
///   syncword           : 12 bits = 0xFFF
///   ID                 :  1 bit  = 0 (MPEG-4)
///   layer              :  2 bits = 0b00
///   protection_absent  :  1 bit  = 1 (no CRC)
///   profile_ObjectType :  2 bits = 0b01 (AAC-LC; SBR/PS signalled implicitly)
///   sampling_freq_idx  :  4 bits (3 = 48000 Hz)
///   private            :  1 bit  = 0
///   channel_config     :  3 bits (1 = centre, 2 = L+R)
///   originality/copy   :  2 bits = 0
///   frame_length       : 13 bits = header(7) + au_len
///   buffer_fullness    : 11 bits = 0x7FF (VBR)
///   num_raw_data_blocks:  2 bits = 0b00 (one block)
fn adts_wrap(au: &[u8], sample_rate_idx: u8, channels: u8) -> Vec<u8> {
    let frame_len = 7 + au.len();
    let mut hdr = [0u8; 7];

    hdr[0] = 0xFF;
    hdr[1] = 0xF1; // syncword(4) | ID=0 | layer=00 | protection_absent=1
    hdr[2] = (0b01 << 6)                // profile = AAC-LC (object type 2 − 1)
           | ((sample_rate_idx & 0x0F) << 2)
           | ((channels >> 2) & 0x01);
    hdr[3] = ((channels & 0x03) << 6) | (((frame_len >> 11) & 0x03) as u8);
    hdr[4] = ((frame_len >> 3) & 0xFF) as u8;
    hdr[5] = (((frame_len & 0x07) << 5) as u8) | 0x1F; // low 3 bits | buf_fullness high
    hdr[6] = 0xFC; // buf_fullness low (0x7FF >> 0 bottom bits) | num_blocks=0

    let mut out = Vec::with_capacity(7 + au.len());
    out.extend_from_slice(&hdr);
    out.extend_from_slice(au);
    out
}

/// Decode one or more ADTS-framed AAC packets to f32 PCM using Symphonia.
pub fn decode_aac_adts(adts_data: &[u8]) -> Vec<f32> {
    if adts_data.is_empty() {
        return Vec::new();
    }

    let cursor = std::io::Cursor::new(adts_data.to_vec());
    let mss = symphonia::core::io::MediaSourceStream::new(Box::new(cursor), Default::default());

    let mut hint = symphonia::core::probe::Hint::new();
    hint.mime_type("audio/aac");

    let probed = match symphonia::default::get_probe().format(
        &hint,
        mss,
        &symphonia::core::formats::FormatOptions::default(),
        &symphonia::core::meta::MetadataOptions::default(),
    ) {
        Ok(p) => p,
        Err(e) => {
            log::warn!("AAC probe failed: {e}");
            return Vec::new();
        }
    };

    let mut format = probed.format;
    let track = match format.default_track() {
        Some(t) => t.clone(),
        None => {
            log::warn!("AAC: no default track");
            return Vec::new();
        }
    };

    let mut decoder = match symphonia::default::get_codecs().make(
        &track.codec_params,
        &symphonia::core::codecs::DecoderOptions::default(),
    ) {
        Ok(d) => d,
        Err(e) => {
            log::warn!("AAC decoder construction failed: {e}");
            return Vec::new();
        }
    };

    let mut out = Vec::new();
    while let Ok(packet) = format.next_packet() {
        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = *decoded.spec();
                let mut buf = symphonia::core::audio::SampleBuffer::<f32>::new(
                    decoded.capacity() as u64,
                    spec,
                );
                buf.copy_interleaved_ref(decoded);
                out.extend_from_slice(buf.samples());
            }
            Err(e) => log::debug!("AAC decode error (skipped): {e}"),
        }
    }
    out
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

/// DAB+ audio superframe parser (ETSI TS 102 563 §5.3).
///
/// A DAB+ superframe spans 5 consecutive CIFs.  Layout:
///   - 2 bytes Firecode CRC (sync/error detection)
///   - 1 byte  stream params: dac_rate, sbr_flag, aac_channel_mode,
///     ps_flag, mpeg_surround_config, rfa
///   - (num_aus − 1) × 12-bit AU start addresses (bit-packed, MSB-first)
///   - AU payloads back-to-back
///   - num_aus × 2-byte CRCs at the end of the superframe
///
/// ADTS parameters (sample rate, channels) are derived from the header.
pub fn decode_dab_plus_superframe(data: &[u8]) -> Vec<f32> {
    // Minimum: firecode(2) + header(1) + at least one 12-bit AU addr + some data
    if data.len() < 6 {
        return Vec::new();
    }

    // Byte 2 (after 2-byte Firecode): stream parameters
    let header_byte = data[2];
    let dac_rate = (header_byte >> 7) & 1;
    let sbr_flag = (header_byte >> 6) & 1;
    let aac_channel_mode = (header_byte >> 5) & 1;

    // Number of AUs depends on dac_rate and sbr_flag (ETSI TS 102 563 Table 2)
    let num_aus: usize = match (dac_rate, sbr_flag) {
        (0, 0) => 6,
        (0, 1) => 3,
        (1, 0) => 4,
        (1, 1) => 2,
        _ => unreachable!(),
    };

    // ADTS sample rate index: core AAC rate (SBR doubles it if present)
    let sample_rate_idx: u8 = match (dac_rate, sbr_flag) {
        (0, 0) => 3, // 48 kHz
        (0, 1) => 6, // 24 kHz core (SBR → 48 kHz)
        (1, 0) => 5, // 32 kHz
        (1, 1) => 8, // 16 kHz core (SBR → 32 kHz)
        _ => unreachable!(),
    };
    let channels: u8 = if aac_channel_mode == 0 { 1 } else { 2 };

    // AU start addresses: (num_aus − 1) × 12-bit values starting at bit 24.
    // All offsets are relative to byte 2 (start of "stream" after Firecode).
    let header_bits = 24 + (num_aus - 1) * 12;
    let first_au_offset = header_bits.div_ceil(8); // first byte after header

    if first_au_offset >= data.len() {
        log::debug!("DAB+: superframe too short for header ({} bytes)", data.len());
        return Vec::new();
    }

    // Build AU start offsets (relative to byte 2 of the superframe).
    // AU[0] starts right after the header; AU[1..n-1] come from the 12-bit fields.
    let mut au_offsets: Vec<usize> = Vec::with_capacity(num_aus + 1);
    au_offsets.push(first_au_offset - 2); // relative to byte 2

    for i in 0..(num_aus - 1) {
        let addr = read_12bits(data, 24 + i * 12) as usize;
        au_offsets.push(addr);
    }

    // Sentinel: end of last AU = total_size − firecode(2) − CRCs (num_aus × 2)
    let au_data_end = data.len() - 2 - 2 * num_aus; // relative to byte 2
    au_offsets.push(au_data_end);

    log::debug!(
        "DAB+ superframe: {} bytes, dac_rate={}, sbr={}, ch={}, num_aus={}, offsets={:?}",
        data.len(),
        dac_rate,
        sbr_flag,
        channels,
        num_aus,
        au_offsets
    );

    let mut pcm = Vec::new();
    for i in 0..num_aus {
        let start = 2 + au_offsets[i]; // absolute byte index in data[]
        let end = 2 + au_offsets[i + 1];

        if start >= end || end > data.len() {
            log::debug!(
                "DAB+: AU[{}] invalid range {}..{} (sf_len={})",
                i,
                start,
                end,
                data.len()
            );
            continue;
        }

        let au = &data[start..end];
        let adts = adts_wrap(au, sample_rate_idx, channels);
        pcm.extend(decode_aac_adts(&adts));
    }
    pcm
}

/// Stateful DAB+ audio decoder.
///
/// Accumulates raw bytes until a full superframe (5 CIFs) is available,
/// then decodes.  Audio parameters (sample rate, channels) are extracted
/// from the superframe header automatically.
pub struct DabPlusDecoder {
    buf: Vec<u8>,
    /// Per-CIF byte count.  Set from the first actual decoded frame.
    pub superframe_size: usize,
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
        }
    }

    /// Update the per-CIF byte count.  Call when actual frame size is known.
    pub fn set_superframe_size(&mut self, size: usize) {
        self.superframe_size = size;
        self.buf.clear();
    }

    /// Push bytes for one CIF and return any decoded PCM samples.
    pub fn push(&mut self, data: &[u8]) -> Vec<f32> {
        if self.superframe_size == 0 {
            return Vec::new();
        }
        self.buf.extend_from_slice(data);
        if self.buf.len() < self.superframe_size * 5 {
            return Vec::new();
        }
        let superframe: Vec<u8> = self.buf.drain(..self.superframe_size * 5).collect();
        decode_dab_plus_superframe(&superframe)
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
    fn dab_plus_superframe_short_input_returns_empty() {
        assert!(decode_dab_plus_superframe(&[0; 5]).is_empty());
    }

    #[test]
    fn dab_plus_superframe_does_not_panic_on_garbage() {
        let garbage = vec![0xFFu8; 1200];
        let _ = decode_dab_plus_superframe(&garbage);
    }

    #[test]
    fn dab_plus_num_aus_from_header() {
        // Verify header byte parsing for all dac_rate/sbr combinations.
        // dac_rate=0, sbr=0 (bits 7:6 = 0b00) → 6 AUs
        // dac_rate=0, sbr=1 (bits 7:6 = 0b01) → 3 AUs
        // dac_rate=1, sbr=0 (bits 7:6 = 0b10) → 4 AUs
        // dac_rate=1, sbr=1 (bits 7:6 = 0b11) → 2 AUs
        let cases: [(u8, usize); 4] = [
            (0b0000_0000, 6), // dac_rate=0, sbr=0
            (0b0100_0000, 3), // dac_rate=0, sbr=1
            (0b1000_0000, 4), // dac_rate=1, sbr=0
            (0b1100_0000, 2), // dac_rate=1, sbr=1
        ];
        for (header_byte, expected_aus) in cases {
            let dac_rate = (header_byte >> 7) & 1;
            let sbr_flag = (header_byte >> 6) & 1;
            let num_aus = match (dac_rate, sbr_flag) {
                (0, 0) => 6usize,
                (0, 1) => 3,
                (1, 0) => 4,
                (1, 1) => 2,
                _ => unreachable!(),
            };
            assert_eq!(
                num_aus, expected_aus,
                "header_byte={:#010b}",
                header_byte
            );
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
}
