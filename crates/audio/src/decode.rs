/// MP2 (MPEG Layer 2) decoder for DAB audio superframes.
///
/// DAB audio is carried as a sequence of raw MPEG Layer 2 frames with no
/// container.  We feed them into Symphonia's MPA codec via a cursor-backed
/// `MediaSourceStream`.

use symphonia::core::{
    audio::SampleBuffer,
    codecs::DecoderOptions,
    formats::FormatOptions,
    io::MediaSourceStream,
    meta::MetadataOptions,
    probe::Hint,
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

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(_) => break,
        };

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
        Mp2Decoder { buf: Vec::new(), min_bytes }
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
