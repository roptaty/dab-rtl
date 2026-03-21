/// X-PAD (Extended Programme Associated Data) extractor and DLS reassembler.
///
/// DAB/DAB+ stations typically carry "now playing" DLS text via X-PAD embedded
/// directly in the audio subchannel, not via a separate packet-mode data
/// service.  This module implements:
///
/// - F-PAD parsing (ETSI EN 300 401 §7.4.2)
/// - X-PAD Content Indicator list extraction
/// - DLS (Dynamic Label Segment) reassembly (ETSI TS 102 980)
///
/// For DAB (MP2): F-PAD is the last 2 bytes of each MPEG Layer 2 frame;
/// X-PAD bytes precede it within the frame's ancillary data area.
///
/// For DAB+ (HE-AAC): PAD is carried in a `data_stream_element()` (DSE,
/// SYN_ELE = 0b100) at the very start of the AU's `raw_data_block()`
/// (ETSI TS 102 563 §5.4.3).  F-PAD is the last 2 bytes of the DSE payload;
/// X-PAD precedes F-PAD within the DSE payload.
use std::collections::BTreeMap;

// ─────────────────────────────────────────────────────────────────────────── //
//  Constants                                                                   //
// ─────────────────────────────────────────────────────────────────────────── //

/// X-PAD Application Type 2 = DLS (Dynamic Label Segment).
const APP_TYPE_DLS: u8 = 2;

/// Map CI length code (upper 4 bits of a Content Indicator byte) to byte count.
///
/// Per ETSI EN 300 401 Table 2a.  Returns `None` for the end-marker (15) or
/// reserved codes (7–14).
fn ci_length(code: u8) -> Option<usize> {
    match code {
        0 => Some(4),
        1 => Some(6),
        2 => Some(8),
        3 => Some(12),
        4 => Some(16),
        5 => Some(24),
        6 => Some(32),
        15 => None, // end marker — stop scanning CI list
        _ => None,  // reserved — stop scanning
    }
}

// ─────────────────────────────────────────────────────────────────────────── //
//  XPadAssembler                                                               //
// ─────────────────────────────────────────────────────────────────────────── //

/// Stateful X-PAD DLS assembler.
///
/// Feed it raw audio bytes (one MPEG Layer 2 frame for DAB, or one Access Unit
/// for DAB+) via [`push_mp2_frame`] / [`push_dabplus_au`].  Returns a
/// [`String`] when a complete Dynamic Label is reassembled.
pub struct XPadAssembler {
    /// Toggle bit of the last complete label (changes when label changes).
    toggle: Option<bool>,
    /// Character set of the current in-progress label.
    charset: u8,
    /// Accumulated segment text, keyed by segment number.
    segments: BTreeMap<u8, Vec<u8>>,
    /// Segment number that carried the "last" flag (None until seen).
    last_seg_num: Option<u8>,
    /// Cached CI list from the most recent frame with CI flag set.
    /// Used for variable X-PAD continuation frames (CI flag = 0).
    last_ci: Vec<(usize, u8)>,
}

impl XPadAssembler {
    pub fn new() -> Self {
        XPadAssembler {
            toggle: None,
            charset: 0,
            segments: BTreeMap::new(),
            last_seg_num: None,
            last_ci: Vec::new(),
        }
    }

    /// Reset all reassembly state (call on service change).
    pub fn reset(&mut self) {
        self.toggle = None;
        self.charset = 0;
        self.segments.clear();
        self.last_seg_num = None;
        self.last_ci.clear();
    }

    /// Process one MPEG Layer 2 frame and return DLS text if a complete label
    /// was received.
    ///
    /// F-PAD occupies the last 2 bytes of the frame; X-PAD (if present)
    /// occupies the bytes immediately before F-PAD.
    pub fn push_mp2_frame(&mut self, frame: &[u8]) -> Option<String> {
        if frame.len() < 2 {
            return None;
        }
        let fpad = [frame[frame.len() - 2], frame[frame.len() - 1]];
        self.process_fpad_xpad(&frame[..frame.len() - 2], fpad)
    }

    /// Process raw MP2 subchannel bytes (may span multiple MPEG frames) and
    /// return DLS text if a complete label was received.
    ///
    /// Scans for MPEG sync words, computes each frame boundary, and calls
    /// [`push_mp2_frame`] for every complete frame found.
    pub fn push_mp2_bytes(&mut self, data: &[u8]) -> Option<String> {
        let mut pos = 0;
        let mut frames_found = 0usize;
        let mut result = None;
        while pos + 4 <= data.len() {
            let Some(size) = mp2_frame_size(&data[pos..]) else {
                pos += 1;
                continue;
            };
            if pos + size > data.len() {
                log::debug!(
                    "X-PAD MP2: frame at offset={} size={} extends past buffer ({} bytes), stopping",
                    pos,
                    size,
                    data.len()
                );
                break;
            }
            frames_found += 1;
            log::debug!(
                "X-PAD MP2: frame at offset={} size={} (total buffer={})",
                pos,
                size,
                data.len()
            );
            if let Some(text) = self.push_mp2_frame(&data[pos..pos + size]) {
                result = Some(text);
            }
            pos += size;
        }
        if frames_found == 0 {
            log::debug!(
                "X-PAD MP2: no MPEG sync found in {} bytes \
                 (first bytes: {:02X} {:02X} {:02X} {:02X})",
                data.len(),
                data.first().copied().unwrap_or(0),
                data.get(1).copied().unwrap_or(0),
                data.get(2).copied().unwrap_or(0),
                data.get(3).copied().unwrap_or(0),
            );
        }
        result
    }

    /// Process one DAB+ Access Unit (without its 2-byte CRC) and return DLS
    /// text if a complete label was received.
    ///
    /// Per ETSI TS 102 563 §5.4.3, PAD data is carried in a
    /// `data_stream_element()` (DSE) at the beginning of the AU.  F-PAD
    /// occupies the last 2 bytes of the DSE payload; X-PAD (if present)
    /// occupies the preceding bytes of the DSE payload.
    pub fn push_dabplus_au(&mut self, au_data: &[u8]) -> Option<String> {
        let pad = extract_dab_plus_pad(au_data)?;
        if pad.len() < 2 {
            log::debug!(
                "X-PAD DAB+: DSE PAD payload too short ({} bytes)",
                pad.len()
            );
            return None;
        }
        let fpad = [pad[pad.len() - 2], pad[pad.len() - 1]];
        log::debug!(
            "X-PAD DAB+: AU {} bytes, DSE PAD {} bytes, F-PAD=[{:02X} {:02X}]",
            au_data.len(),
            pad.len(),
            fpad[0],
            fpad[1],
        );
        self.process_fpad_xpad(&pad[..pad.len() - 2], fpad)
    }

    // ──────────────────────────────────────────────────────────────────────── //
    //  Internal                                                                 //
    // ──────────────────────────────────────────────────────────────────────── //

    /// Process an F-PAD + preceding X-PAD area, returning DLS text if a
    /// complete label is now available.
    ///
    /// `xpad_area` is every byte *before* F-PAD in the audio frame / AU.
    /// `fpad` is the 2-byte Fixed PAD [byte0, byte1].
    fn process_fpad_xpad(&mut self, xpad_area: &[u8], fpad: [u8; 2]) -> Option<String> {
        // Per ETSI EN 300 401 v2.1.1 Table 7, F-PAD byte 0:
        //   bits 7-6 = frame type (00 = standard; non-zero → skip)
        //   bits 5-4 = X-PAD indicator: 00=none, 01=short, 10=variable, 11=end
        // F-PAD byte 1 bit 1: CI flag (1 = Content Indicator list present)
        let fpad_type = (fpad[0] >> 6) & 0x03;
        let xpad_type = (fpad[0] >> 4) & 0x03;
        let ci_flag = (fpad[1] & 0x02) != 0;

        log::debug!(
            "X-PAD: F-PAD=[{:02X} {:02X}] fpad_type={} xpad_type={} ({}) ci_flag={} xpad_area_len={}",
            fpad[0],
            fpad[1],
            fpad_type,
            xpad_type,
            match xpad_type {
                0b00 => "no X-PAD",
                0b01 => "short X-PAD",
                0b10 => "variable X-PAD",
                _ => "end/no X-PAD",
            },
            ci_flag,
            xpad_area.len(),
        );

        if fpad_type != 0b00 {
            log::debug!("X-PAD: non-standard F-PAD type={} — skipping", fpad_type);
            return None;
        }

        // Short X-PAD (xpad_type=01): 4 bytes immediately before F-PAD.
        // Layout: [data[0], data[1], data[2], app_type_byte] (left to right).
        // app_type_byte (rightmost) carries the application type (bits 3-0).
        // Data is 3 bytes; we only process it if app_type == DLS (2).
        if xpad_type == 0b01 {
            if xpad_area.len() < 4 {
                log::debug!("X-PAD: short X-PAD too short ({} bytes)", xpad_area.len());
                return None;
            }
            let type_byte = xpad_area[xpad_area.len() - 1];
            let app_type = type_byte & 0x0F;
            if app_type != APP_TYPE_DLS {
                log::debug!(
                    "X-PAD: short X-PAD app_type={} (not DLS) — skipping",
                    app_type
                );
                return None;
            }
            // Short X-PAD data is also stored right-to-left; reverse for logical order.
            let mut dls_chunk = xpad_area[xpad_area.len() - 4..xpad_area.len() - 1].to_vec();
            dls_chunk.reverse();
            log::debug!("X-PAD: short X-PAD DLS chunk: {:02X?}", &dls_chunk);
            return self.process_dls_chunk(&dls_chunk);
        }

        if xpad_type != 0b10 {
            // No X-PAD or end marker — DLS is not carried here.
            return None;
        }

        let dls_chunk = if ci_flag {
            // New CI list present: parse it, cache it, extract DLS data.
            let (ci_entries, data_right) = parse_ci_list(xpad_area);
            if !ci_entries.is_empty() {
                self.last_ci = ci_entries.clone();
            }
            extract_dls_with_ci(xpad_area, &ci_entries, data_right)
        } else {
            // Continuation mode: no CI list in this frame; use the cached one.
            // The entire xpad_area is application data (no CI bytes present).
            if self.last_ci.is_empty() {
                log::debug!("X-PAD: continuation frame but no cached CI list — skipping");
                return None;
            }
            log::debug!(
                "X-PAD: continuation frame, using cached CI ({} entr{})",
                self.last_ci.len(),
                if self.last_ci.len() == 1 { "y" } else { "ies" }
            );
            extract_dls_with_ci(xpad_area, &self.last_ci, xpad_area.len())
        }?;

        log::debug!(
            "X-PAD: DLS chunk {} bytes: {:02X?}",
            dls_chunk.len(),
            &dls_chunk[..dls_chunk.len().min(8)]
        );
        self.process_dls_chunk(&dls_chunk)
    }

    /// Incorporate one DLS data chunk and return a complete label if ready.
    fn process_dls_chunk(&mut self, chunk: &[u8]) -> Option<String> {
        if chunk.is_empty() {
            return None;
        }

        // DLS segment header (ETSI TS 102 980 §5.1.1):
        //   bits 7-4: Segment number (0-15)
        //   bit 3:    Toggle bit (changes when label changes)
        //   bit 2:    First flag (1 = first segment; includes charset byte)
        //   bit 1:    Last flag  (1 = last segment)
        //   bit 0:    Command flag (1 = command/control, not label text → skip)
        let cmd = chunk[0];
        let is_command = (cmd & 0x01) != 0;

        if is_command {
            log::debug!("X-PAD DLS: cmd={:02X} is a command segment — skipping", cmd);
            return None;
        }

        let first = (cmd & 0x04) != 0;
        let last = (cmd & 0x02) != 0;
        let toggle = (cmd & 0x08) != 0;
        // When both first and last are set the entire label fits in this one
        // segment; normalise to segment 0 so try_assemble finds it immediately.
        let seg_num = if first && last { 0 } else { (cmd >> 4) & 0x0F };

        log::debug!(
            "X-PAD DLS: cmd={:02X} first={} last={} toggle={} seg={} chunk_len={}",
            cmd,
            first,
            last,
            toggle,
            seg_num,
            chunk.len()
        );

        // Detect label change via toggle bit.
        if let Some(prev) = self.toggle {
            if toggle != prev {
                self.segments.clear();
                self.last_seg_num = None;
                log::debug!(
                    "X-PAD DLS: toggle changed ({} → {}) — new label",
                    prev,
                    toggle
                );
            }
        }
        self.toggle = Some(toggle);

        // The charset byte follows the command byte only in the first segment.
        let text_bytes = if first {
            if chunk.len() < 2 {
                return None;
            }
            self.charset = (chunk[1] >> 4) & 0x0F;
            &chunk[2..]
        } else {
            &chunk[1..]
        };

        // Strip null padding and store.
        let text_bytes: Vec<u8> = text_bytes
            .iter()
            .copied()
            .take_while(|&b| b != 0x00)
            .collect();
        if !text_bytes.is_empty() {
            self.segments.insert(seg_num, text_bytes);
        }
        if last {
            self.last_seg_num = Some(seg_num);
        }

        self.try_assemble()
    }

    /// Try to produce a complete label from accumulated segments.
    fn try_assemble(&self) -> Option<String> {
        let last = self.last_seg_num?;
        // Require all segments 0..=last.
        for i in 0..=last {
            self.segments.get(&i)?;
        }
        let bytes: Vec<u8> = (0..=last)
            .flat_map(|i| self.segments[&i].iter().copied())
            .collect();
        let text = decode_dls_text(&bytes, self.charset);
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }
}

impl Default for XPadAssembler {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────────────── //
//  DAB+ DSE PAD extraction                                                     //
// ─────────────────────────────────────────────────────────────────────────── //

/// Extract the PAD byte slice from the `data_stream_element()` at the start
/// of a DAB+ Access Unit.
///
/// Per ETSI TS 102 563 §5.4.3 and ISO/IEC 14496-3 (MPEG-4 AAC), the DSE
/// header occupies exactly 2 bytes (3-bit SYN_ELE + 4-bit instance_tag +
/// 1-bit align_flag + 8-bit count), giving the PAD data immediately after:
///
/// ```text
/// Byte 0: [SYN_ELE(3=0b100) | instance_tag(4) | align_flag(1)]
/// Byte 1: count (number of PAD bytes; 255 triggers escape extension)
/// Byte 2+: PAD payload = [X-PAD | F-PAD(2)]
/// ```
///
/// Returns `None` if the AU does not start with a DSE or is truncated.
fn extract_dab_plus_pad(au: &[u8]) -> Option<&[u8]> {
    if au.len() < 2 {
        return None;
    }
    // Top 3 bits of byte 0 must be 0b100 (ID_DSE = 4).
    if au[0] & 0xE0 != 0x80 {
        log::debug!(
            "X-PAD DAB+: AU byte[0]={:02X} — no DSE at start, no PAD",
            au[0]
        );
        return None;
    }
    // data_byte_align_flag (bit 0 of byte 0): if set the DSE data is byte-aligned
    // within the bitstream.  Since the DSE header is already 2 full bytes we are
    // always byte-aligned at the data start, so this flag has no practical effect
    // here and is intentionally ignored.
    let mut count = au[1] as usize;
    let data_start = if count == 255 {
        // Escape: total count = 255 + esc_count (per ISO/IEC 14496-3).
        if au.len() < 3 {
            log::debug!("X-PAD DAB+: DSE truncated before escape count byte");
            return None;
        }
        count = 255 + au[2] as usize;
        3
    } else {
        2
    };
    if au.len() < data_start + count {
        log::debug!(
            "X-PAD DAB+: DSE payload truncated (need {} bytes after offset {}, have {})",
            count,
            data_start,
            au.len()
        );
        return None;
    }
    log::debug!(
        "X-PAD DAB+: DSE found, instance_tag={}, count={} bytes",
        (au[0] >> 1) & 0x0F,
        count
    );
    Some(&au[data_start..data_start + count])
}

// ─────────────────────────────────────────────────────────────────────────── //
//  X-PAD CI list parsing                                                       //
// ─────────────────────────────────────────────────────────────────────────── //

/// Parse the CI list from the right end of `xpad_area`.
///
/// Returns `(ci_entries, data_right)` where:
/// - `ci_entries` is a list of `(length_bytes, app_type)` in CI-list order
///   (first entry = rightmost data field, i.e. closest to F-PAD)
/// - `data_right` is the exclusive right boundary of the app-data area
///   (the index just left of the CI list)
///
/// Memory layout (left = low index, right = high index):
/// ```text
/// [zeros | app_data[N-1] | ... | app_data[0] | CI[0] | CI[1] | … | end | F-PAD]
/// ```
fn parse_ci_list(xpad_area: &[u8]) -> (Vec<(usize, u8)>, usize) {
    let mut pos = xpad_area.len();
    let mut ci_entries: Vec<(usize, u8)> = Vec::new();

    loop {
        if pos == 0 {
            log::debug!("X-PAD CI: reached left edge without end marker");
            break;
        }
        pos -= 1;
        let ci = xpad_area[pos];
        let length_code = ci >> 4;
        let app_type = ci & 0x0F;

        match ci_length(length_code) {
            None => {
                log::debug!(
                    "X-PAD CI: end/reserved at pos={} byte={:02X} (length_code={})",
                    pos,
                    ci,
                    length_code
                );
                break;
            }
            Some(len) => {
                log::debug!(
                    "X-PAD CI: pos={} byte={:02X} length_code={} len={}B app_type={}",
                    pos,
                    ci,
                    length_code,
                    len,
                    app_type
                );
                ci_entries.push((len, app_type));
            }
        }
    }

    if ci_entries.is_empty() {
        log::debug!(
            "X-PAD CI: no CI entries found in {} xpad bytes",
            xpad_area.len()
        );
    } else {
        log::debug!(
            "X-PAD CI: {} entr{} found, data_right boundary={}",
            ci_entries.len(),
            if ci_entries.len() == 1 { "y" } else { "ies" },
            pos
        );
    }

    (ci_entries, pos)
}

/// Extract DLS data from `xpad_area` using a given CI list.
///
/// `data_right` is the exclusive right boundary of the app-data area.
/// In frames with a CI list present, this is the index just left of the CI
/// bytes.  In continuation frames, pass `xpad_area.len()` (whole area is data).
fn extract_dls_with_ci(
    xpad_area: &[u8],
    ci_entries: &[(usize, u8)],
    data_right: usize,
) -> Option<Vec<u8>> {
    let mut data_right = data_right;
    for (length, app_type) in ci_entries {
        if data_right < *length {
            log::debug!(
                "X-PAD CI: app_type={} needs {}B but only {}B remain — truncated",
                app_type,
                length,
                data_right
            );
            break;
        }
        let data_left = data_right - length;
        log::debug!(
            "X-PAD CI: app_type={} data[{}..{}]",
            app_type,
            data_left,
            data_right
        );
        if *app_type == APP_TYPE_DLS {
            // Per ETSI EN 300 401 §7.4.2.2.2, byte 0 of each data subfield is
            // physically adjacent to the CI list (rightmost), so bytes appear in
            // reverse logical order.  Reverse to restore cmd-byte-first order.
            let mut chunk = xpad_area[data_left..data_right].to_vec();
            chunk.reverse();
            return Some(chunk);
        }
        data_right = data_left;
    }
    log::debug!("X-PAD CI: no DLS (app_type=2) in CI list");
    None
}

// ─────────────────────────────────────────────────────────────────────────── //
//  Character set decoding                                                       //
// ─────────────────────────────────────────────────────────────────────────── //

/// Decode DLS text bytes using the given DAB charset code.
///
/// - Charset 0: EBU Latin (ETSI TS 101 756 Annex C).  The lower 128 code
///   points match ASCII; the upper 128 are decoded as their Unicode equivalents
///   (same as ISO 8859-1 for the practical range 0x80–0xFF).
/// - Charset 6: UTF-8.
/// - Others: treated as EBU Latin.
fn decode_dls_text(bytes: &[u8], charset: u8) -> String {
    let s = if charset == 6 {
        String::from_utf8_lossy(bytes).into_owned()
    } else {
        // EBU Latin / ISO 8859-1 fallback: each byte maps to the same Unicode
        // codepoint (0x00–0xFF are all valid Unicode scalar values).
        bytes
            .iter()
            .map(|&b| char::from_u32(b as u32).unwrap_or('\u{FFFD}'))
            .collect()
    };
    s.trim_matches(|c: char| c == '\0' || c.is_whitespace())
        .to_string()
}

// ─────────────────────────────────────────────────────────────────────────── //
//  MPEG Layer 2 frame size                                                     //
// ─────────────────────────────────────────────────────────────────────────── //

/// Compute the total byte length of an MPEG Layer 2 frame whose first byte is
/// `data[0]`.
///
/// Returns `None` if the sync word is absent, the header fields are reserved,
/// or the MPEG version / layer combination is not Layer 2.
pub fn mp2_frame_size(data: &[u8]) -> Option<usize> {
    if data.len() < 4 {
        return None;
    }
    // Sync: first byte = 0xFF, top 3 bits of second byte = 111.
    if data[0] != 0xFF || (data[1] & 0xE0) != 0xE0 {
        return None;
    }

    // byte[1]:  bits[7:5]=sync  bit[4]=version  bits[3:2]=layer  bit[1]=protection  bit[0]=private
    let version = (data[1] >> 4) & 0x01; // 1 = MPEG-1, 0 = MPEG-2
    let layer = (data[1] >> 2) & 0x03; // 10 = Layer II
    if layer != 0b10 {
        return None; // only Layer II handled
    }

    let bitrate_idx = ((data[2] >> 4) & 0x0F) as usize;
    let sr_idx = ((data[2] >> 2) & 0x03) as usize;
    let padding = ((data[2] >> 1) & 0x01) as usize;

    // Bitrate tables (kbps), indexed by bitrate_index 0..15.
    const BITRATES_MPEG1: [u32; 16] = [
        0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 0,
    ];
    const BITRATES_MPEG2: [u32; 16] = [
        0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0,
    ];
    // Sample-rate tables (Hz).
    const SR_MPEG1: [u32; 4] = [44_100, 48_000, 32_000, 0];
    const SR_MPEG2: [u32; 4] = [22_050, 24_000, 16_000, 0];

    let bitrate_kbps = if version == 1 {
        BITRATES_MPEG1[bitrate_idx]
    } else {
        BITRATES_MPEG2[bitrate_idx]
    };
    let sample_rate = if version == 1 {
        SR_MPEG1[sr_idx]
    } else {
        SR_MPEG2[sr_idx]
    };

    if bitrate_kbps == 0 || sample_rate == 0 {
        return None;
    }

    // MPEG Layer 2 frame size formula (ISO 11172-3):
    //   frame_size = 144 * bitrate_bps / sample_rate + padding
    Some(144 * bitrate_kbps as usize * 1000 / sample_rate as usize + padding)
}

// ─────────────────────────────────────────────────────────────────────────── //
//  Tests                                                                       //
// ─────────────────────────────────────────────────────────────────────────── //

#[cfg(test)]
mod tests {
    use super::*;

    // ── mp2_frame_size ───────────────────────────────────────────────────── //

    #[test]
    fn mp2_frame_size_mpeg1_128kbps_48khz() {
        // MPEG-1 Layer 2, 128 kbps, 48 kHz, no padding, no CRC.
        // byte[1]: 111 1 10 1 0 = 0b11111010 = 0xFA
        // byte[2]: bitrate_index=8 (128k), sr_index=1 (48kHz), pad=0 → 0b10000100 = 0x84
        let header = [0xFF, 0xFA, 0x84, 0xC4];
        assert_eq!(mp2_frame_size(&header), Some(384));
    }

    #[test]
    fn mp2_frame_size_mpeg1_192kbps_48khz() {
        // 192 kbps → bitrate_index=10 → byte[2] = 0b10100100 = 0xA4
        let header = [0xFF, 0xFA, 0xA4, 0xC4];
        assert_eq!(mp2_frame_size(&header), Some(576));
    }

    #[test]
    fn mp2_frame_size_bad_sync() {
        assert_eq!(mp2_frame_size(&[0xFE, 0xFA, 0x84, 0xC4]), None);
        assert_eq!(mp2_frame_size(&[0xFF, 0x00, 0x84, 0xC4]), None);
    }

    #[test]
    fn mp2_frame_size_layer3_returns_none() {
        // Layer III: bits[3:2] = 01 → byte[1] = 111 1 01 1 0 = 0b11110110 = 0xF6... wait
        // layer=01 → bits[3:2]=01 → byte[1] = 111_1_01_10 = 0xF6 (protection=1, priv=0)
        // Actually: 1111 0110 = 0xF6... hmm, let me compute:
        // 7=1,6=1,5=1(sync) 4=1(MPEG1) 3=0,2=1(layer=01=L3) 1=1(prot=1) 0=0(priv=0) = 11110110 = 0xF6
        let header = [0xFF, 0xF6, 0x84, 0xC4];
        assert_eq!(mp2_frame_size(&header), None);
    }

    // ── CI list / DLS extraction ─────────────────────────────────────────── //

    #[test]
    fn find_dls_chunk_single_ci() {
        // X-PAD area layout (left to right): data | end_marker | CI
        // CI entries are rightmost (closest to F-PAD); end_marker is just left of them.
        //
        // CI byte: length_code=0 (4 bytes), app_type=2 → 0x02
        // End marker: length_code=15 → 0xF0 (any low nibble)
        // DLS data: 4 bytes in physical order (byte 0 = cmd is RIGHTMOST per spec)
        //   Logical: [cmd=0xC0, charset=0x00, 'A', 'B']
        //   Physical (rightmost = byte 0): ['B', 'A', 0x00, 0xC0]
        let dls_data_physical = [0x42u8, 0x41, 0x00, 0xC0]; // 'B' 'A' charset cmd
        let end = 0xF0u8; // end marker (left of CI)
        let ci = 0x02u8; // CI: length_code=0 (4 bytes), app_type=2 (right, closest to F-PAD)
        let xpad_area: Vec<u8> = dls_data_physical.iter().copied().chain([end, ci]).collect();
        let (ci_entries, data_right) = parse_ci_list(&xpad_area);
        let chunk = extract_dls_with_ci(&xpad_area, &ci_entries, data_right)
            .expect("should find DLS chunk");
        // extract_dls_with_ci reverses physical→logical; expect logical order
        assert_eq!(chunk, [0xC0u8, 0x00, 0x41, 0x42]);
    }

    #[test]
    fn find_dls_chunk_no_dls_app() {
        // CI with app_type=3 (not DLS).  Layout: data | end_marker | CI.
        let data = [0u8; 4];
        let mut xpad = data.to_vec();
        xpad.push(0xF0); // end marker (left of CI)
        xpad.push(0x03); // CI: length_code=0 (4 bytes), app_type=3
        let (ci_entries, data_right) = parse_ci_list(&xpad);
        assert!(extract_dls_with_ci(&xpad, &ci_entries, data_right).is_none());
    }

    // ── XPadAssembler ────────────────────────────────────────────────────── //

    #[test]
    fn assembler_single_segment_label() {
        let mut asm = XPadAssembler::new();

        // Build an X-PAD area with a single-segment DLS label "Hi".
        // DLS chunk (6 bytes = length_code=1) — ETSI TS 102 980 §5.1.1 format:
        //   Logical: [cmd=0x06, charset=0x00, 'H'=0x48, 'i'=0x69, pad, pad]
        //   Physical (byte 0 = cmd is rightmost per spec):
        //     [0x00, 0x00, 'i'=0x69, 'H'=0x48, 0x00, cmd=0x06]
        let dls_chunk = [0x00u8, 0x00, 0x69, 0x48, 0x00, 0x06];
        let end = 0xF0u8; // end marker (left of CI entries)
        let ci = 0x12u8; // CI: length_code=1 (6 bytes), app_type=2 (rightmost, closest to F-PAD)

        // Build a fake MPEG frame: arbitrary audio bytes + X-PAD area + F-PAD.
        // X-PAD area layout (left to right): dls_chunk | end_marker | CI
        let mut frame = vec![0u8; 10]; // "audio" bytes (ignored)
        frame.extend_from_slice(&dls_chunk);
        frame.push(end);
        frame.push(ci);
        // F-PAD per ETSI EN 300 401 v2.1.1:
        //   byte0 = 0x20: bits 7-6=00 (standard), bits 5-4=10 (variable X-PAD)
        //   byte1 = 0x02: bit 1 = 1 (CI flag set)
        frame.push(0x20);
        frame.push(0x02);

        let result = asm.push_mp2_frame(&frame);
        assert_eq!(result, Some("Hi".to_string()));
    }

    #[test]
    fn assembler_toggle_resets_on_change() {
        let mut asm = XPadAssembler::new();

        // First label "AA" with toggle=0; second label "BB" with toggle=1.
        // Stored in physical order (byte 0 = cmd is rightmost per spec).
        // Logical dls1: [0x06, 0x00, 'A', 'A', 0x00, 0x00] → physical: [0x00, 0x00, 'A', 'A', 0x00, 0x06]
        // Logical dls2: [0x0E, 0x00, 'B', 'B', 0x00, 0x00] → physical: [0x00, 0x00, 'B', 'B', 0x00, 0x0E]
        let dls1 = [0x00u8, 0x00, 0x41, 0x41, 0x00, 0x06]; // toggle=0
        let dls2 = [0x00u8, 0x00, 0x42, 0x42, 0x00, 0x0E]; // toggle=1

        // Frame layout: fake_audio | dls_chunk | end_marker | CI | F-PAD
        let build_frame = |dls: &[u8]| {
            let mut f = vec![0u8; 4]; // fake audio
            f.extend_from_slice(dls);
            f.push(0xF0); // end marker (left of CI)
            f.push(0x12); // CI: length_code=1 (6 bytes), app_type=2
            f.push(0x20); // F-PAD byte0: bits 7-6=00 (standard), bits 5-4=10 (variable X-PAD)
            f.push(0x02); // F-PAD byte1: bit 1 = 1 (CI flag set)
            f
        };

        let r1 = asm.push_mp2_frame(&build_frame(&dls1));
        assert_eq!(r1, Some("AA".to_string()));

        let r2 = asm.push_mp2_frame(&build_frame(&dls2));
        assert_eq!(r2, Some("BB".to_string()));
    }

    #[test]
    fn assembler_continuation_mode() {
        // First frame: CI flag set — establishes the CI list.
        // Second frame: CI flag NOT set — continuation, should reuse cached CI.
        let mut asm = XPadAssembler::new();

        // DLS chunk (6 bytes = length_code=1) — stored in physical order.
        // Logical: [cmd=0x06, charset=0x00, 'O'=0x4F, 'K'=0x4B, pad, pad]
        // Physical (byte 0 = cmd is rightmost): [0x00, 0x00, 'K', 'O', 0x00, 0x06]
        let dls_chunk = [0x00u8, 0x00, 0x4B, 0x4F, 0x00, 0x06];

        // Frame 1: explicit CI list present (ci_flag=1).
        // Layout: dls_chunk | end_marker | CI | F-PAD
        let mut frame1 = vec![0u8; 4]; // fake audio
        frame1.extend_from_slice(&dls_chunk);
        frame1.push(0xF0); // end marker
        frame1.push(0x12); // CI: length_code=1 (6 bytes), app_type=2
        frame1.push(0x20); // F-PAD byte0: bits 7-6=00 (standard), bits 5-4=10 (variable X-PAD)
        frame1.push(0x02); // F-PAD byte1: bit 1 = 1 (CI flag set)

        let r1 = asm.push_mp2_frame(&frame1);
        assert_eq!(r1, Some("OK".to_string()));

        // Frame 2: continuation (ci_flag=0).
        // Layout: dls_chunk | F-PAD  (no CI bytes — entire xpad area is data)
        // Frame 2: "Go" with toggle=1. Physical order (byte 0 = cmd is rightmost).
        // Logical: [0x0E, 0x00, 'G'=0x47, 'o'=0x6F, 0x00, 0x00]
        // Physical: [0x00, 0x00, 'o', 'G', 0x00, 0x0E]
        let dls_chunk2 = [0x00u8, 0x00, 0x6F, 0x47, 0x00, 0x0E]; // toggle=1, "Go"
        let mut frame2 = vec![0u8; 4]; // fake audio
        frame2.extend_from_slice(&dls_chunk2);
        // F-PAD byte0: bits 7-6=00 (standard), bits 5-4=10 (variable X-PAD), CI flag NOT set
        frame2.push(0x20); // 0b00100000
        frame2.push(0x00); // F-PAD byte1: bit 1 = 0 (no CI list)

        let r2 = asm.push_mp2_frame(&frame2);
        assert_eq!(r2, Some("Go".to_string()));
    }

    #[test]
    fn assembler_no_xpad_returns_none() {
        let mut asm = XPadAssembler::new();
        // F-PAD byte0 = 0x00: type=00 (no X-PAD)
        let frame = [0xFF, 0xFA, 0x84, 0xC4, 0x00, 0x00u8]; // tiny fake frame
        assert!(asm.push_mp2_frame(&frame).is_none());
    }

    // ── extract_dab_plus_pad ─────────────────────────────────────────────── //

    #[test]
    fn dab_plus_pad_no_dse_returns_none() {
        // Byte 0 top 3 bits != 0b100 → not a DSE.
        let au = [0x00u8, 0x06, 0xA0, 0x00]; // SYN_ELE=0b000 (SCE)
        assert!(extract_dab_plus_pad(&au).is_none());
    }

    #[test]
    fn dab_plus_pad_extracts_dse_payload() {
        // DSE header: byte0=0x80 (SYN_ELE=0b100, tag=0, align=0), byte1=0x04 (count=4)
        // PAD payload: 4 bytes of dummy data
        let au = [0x80u8, 0x04, 0xAA, 0xBB, 0xCC, 0xDD, 0x99, 0x99];
        let pad = extract_dab_plus_pad(&au).unwrap();
        assert_eq!(pad, &[0xAA, 0xBB, 0xCC, 0xDD]);
    }

    #[test]
    fn dab_plus_pad_escape_count() {
        // count=255 → escape; total = 255 + esc_count.
        let mut au = vec![0x80u8, 0xFF, 0x01]; // header: count=255, esc=1 → total 256
        au.extend(vec![0xABu8; 256]); // 256 bytes of PAD payload
        au.extend(vec![0x00u8; 10]); // trailing audio bytes
        let pad = extract_dab_plus_pad(&au).unwrap();
        assert_eq!(pad.len(), 256);
        assert!(pad.iter().all(|&b| b == 0xAB));
    }

    #[test]
    fn dab_plus_pad_truncated_returns_none() {
        // count says 10 bytes but AU only has 3 bytes of payload.
        let au = [0x80u8, 0x0A, 0x01, 0x02, 0x03];
        assert!(extract_dab_plus_pad(&au).is_none());
    }

    #[test]
    fn assembler_dabplus_au_single_segment_label() {
        let mut asm = XPadAssembler::new();

        // DLS chunk (6 bytes, length_code=1 in CI list).
        // Stored in physical order (byte 0 = cmd is rightmost per spec).
        // Logical: [cmd=0x06, charset=0x00, 'H'=0x48, 'i'=0x69, pad, pad]
        // Physical: [0x00, 0x00, 'i', 'H', 0x00, cmd=0x06]
        let dls_chunk = [0x00u8, 0x00, 0x69, 0x48, 0x00, 0x06];
        let end = 0xF0u8; // CI end marker
        let ci = 0x12u8; // CI: length_code=1 (6 bytes), app_type=2 (DLS)

        // PAD field: [dls_chunk | end_marker | CI | F-PAD]
        // F-PAD per ETSI EN 300 401 v2.1.1:
        //   byte0 = 0x20: bits 7-6=00 (standard), bits 5-4=10 (variable X-PAD)
        //   byte1 = 0x02: bit 1 = 1 (CI flag set)
        let mut pad: Vec<u8> = dls_chunk.to_vec();
        pad.push(end);
        pad.push(ci);
        pad.push(0x20); // F-PAD byte0
        pad.push(0x02); // F-PAD byte1

        // AU: DSE header + PAD payload + trailing AAC audio bytes
        let count = pad.len() as u8; // 10
        let mut au = vec![0x80u8, count]; // DSE: SYN_ELE=DSE, tag=0, align=0; count
        au.extend_from_slice(&pad);
        au.extend(vec![0u8; 20]); // fake AAC audio bytes

        let result = asm.push_dabplus_au(&au);
        assert_eq!(result, Some("Hi".to_string()));
    }

    #[test]
    fn decode_dls_text_utf8() {
        let bytes = "Hællo".as_bytes();
        let s = decode_dls_text(bytes, 6);
        assert_eq!(s, "Hællo");
    }

    #[test]
    fn decode_dls_text_ebu_latin_ascii() {
        let bytes = b"Hello";
        let s = decode_dls_text(bytes, 0);
        assert_eq!(s, "Hello");
    }

    #[test]
    fn decode_dls_text_strips_nulls_and_whitespace() {
        let bytes = b"  Hi\0\0";
        let s = decode_dls_text(bytes, 0);
        assert_eq!(s, "Hi");
    }

    #[test]
    fn decode_dls_text_ebu_latin_high_bytes() {
        // 0xE6=æ, 0xF8=ø, 0xE5=å in ISO 8859-1 / EBU Latin
        let bytes = [0xE6u8, 0xF8, 0xE5];
        let s = decode_dls_text(&bytes, 0);
        assert_eq!(s, "æøå");
    }
}
