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

/// DAB+ audio superframe parser.
///
/// A DAB+ audio superframe is 5 × (subchannel_size / 5) bytes, structured as:
///   - 2 bytes Firecode header (sync + CRC, we skip CRC here)
///   - 1 byte  flags (num_AUs, DAC rate, SBR flag, etc.)
///   - 2 × num_AUs bytes  AU start offsets (big-endian, relative to payload start)
///   - AU payloads back-to-back
///
/// We extract each AU, wrap it in ADTS, and decode.
///
/// `sample_rate_idx` and `channels` are the ADTS parameters to use.
pub fn decode_dab_plus_superframe(data: &[u8], sample_rate_idx: u8, channels: u8) -> Vec<f32> {
    // Minimum viable superframe: firecode(2) + header(1) + one AU offset(2) = 5 bytes
    if data.len() < 5 {
        return Vec::new();
    }

    // Byte 2 (after 2-byte Firecode): num_AUs is encoded in bits [4:3]
    //   00 = 2 AUs, 01 = 3 AUs, 10 = 4 AUs, 11 = 6 AUs
    let header_byte = data[2];
    let num_aus: usize = match (header_byte >> 3) & 0x03 {
        0 => 2,
        1 => 3,
        2 => 4,
        _ => 6,
    };

    // AU start offsets: 2 bytes each, starting at byte 3.
    // Last AU ends at data.len() - 2 (last 2 bytes are the RFA/padding).
    let offsets_start = 3usize;
    let offsets_end = offsets_start + num_aus * 2;
    if offsets_end > data.len() {
        log::debug!("DAB+: superframe too short for {} AU offsets", num_aus);
        return Vec::new();
    }

    let payload_start = offsets_end;
    let payload_end = data.len().saturating_sub(2); // trim 2-byte trailing CRC

    let mut au_starts: Vec<usize> = (0..num_aus)
        .map(|k| {
            let o = offsets_start + k * 2;
            u16::from_be_bytes([data[o], data[o + 1]]) as usize + payload_start
        })
        .collect();
    au_starts.push(payload_end); // sentinel

    let mut pcm = Vec::new();
    for w in au_starts.windows(2) {
        let (start, end) = (w[0], w[1]);
        if start >= end || end > data.len() {
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
/// Accumulates raw bytes until a full superframe is available, then decodes.
/// Call `set_params` once the audio parameters are known (from PAD/XPad or
/// defaults: 48 kHz stereo).
pub struct DabPlusDecoder {
    buf: Vec<u8>,
    pub superframe_size: usize,
    sample_rate_idx: u8,
    channels: u8,
}

impl DabPlusDecoder {
    /// Create a decoder.
    ///
    /// `superframe_size` is the subchannel capacity in bytes per 24 ms frame.
    /// If unknown, pass 0 — the decoder will skip until `set_superframe_size`
    /// is called.  Default audio params: 48 kHz stereo.
    pub fn new(superframe_size: usize) -> Self {
        DabPlusDecoder {
            buf: Vec::new(),
            superframe_size,
            sample_rate_idx: 3, // 48000 Hz
            channels: 2,        // stereo
        }
    }

    /// Update the superframe size (bytes).  Call when subchannel info is known.
    pub fn set_superframe_size(&mut self, size: usize) {
        self.superframe_size = size;
        self.buf.clear();
    }

    /// Update sample rate and channel count for ADTS wrapping.
    /// ADTS sample_rate_idx: 3=48000, 4=44100, 5=32000, 6=24000, 7=22050.
    pub fn set_params(&mut self, sample_rate_idx: u8, channels: u8) {
        self.sample_rate_idx = sample_rate_idx;
        self.channels = channels;
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
        decode_dab_plus_superframe(&superframe, self.sample_rate_idx, self.channels)
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
}
