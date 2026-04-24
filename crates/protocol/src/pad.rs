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
use std::time::{SystemTime, UNIX_EPOCH};

use crate::ensemble::{MetadataSource, NowPlaying};
use crate::text::decode_dab_text;

// ─────────────────────────────────────────────────────────────────────────── //
//  Constants                                                                   //
// ─────────────────────────────────────────────────────────────────────────── //

/// X-PAD Application Type 2 = DLS start (Dynamic Label Segment, start of data group).
const APP_TYPE_DLS_START: u8 = 2;

/// X-PAD Application Type 3 = DLS continuation.
const APP_TYPE_DLS_CONT: u8 = 3;

/// Map CI sub-field length indicator (upper 3 bits of a Content Indicator byte)
/// to byte count.
///
/// Per ETSI EN 300 401 §7.4.2.2, Table 2 (variable-size X-PAD).
/// All 3-bit codes 0–7 are valid lengths.  The end of the CI list is signalled
/// by `app_type == 0` (bottom 5 bits), not by the length indicator.
fn ci_length(code: u8) -> usize {
    match code {
        0 => 4,
        1 => 6,
        2 => 8,
        3 => 12,
        4 => 16,
        5 => 24,
        6 => 32,
        7 => 48,
        _ => unreachable!(), // 3-bit code is always 0–7
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
    /// Command segments (bit0=1 in DLS command byte), keyed by segment number.
    command_segments: BTreeMap<u8, Vec<u8>>,
    /// Segment number that carried the "last" flag for command segments.
    command_last_seg_num: Option<u8>,
    /// Most recently decoded DL+ tags.
    dl_plus: Option<DlPlusFields>,
}

#[derive(Debug, Clone, Default)]
struct DlPlusFields {
    tags: Vec<(u8, usize, usize)>,
    /// Item Toggle bit from the DL+ command header (TS 102 980 §7.3).
    /// Stored for future cross-checking against the DLS segment toggle.
    #[allow(dead_code)]
    item_toggle: Option<bool>,
    /// Item Running bit from the DL+ command header.  `Some(false)` tells the
    /// receiver to clear any displayed title/artist/etc. for the service.
    item_running: Option<bool>,
}

#[derive(Debug, Clone, Default)]
struct DlPlusValues {
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    track: Option<String>,
    composer: Option<String>,
    band: Option<String>,
    genre: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct ParsedDls {
    text: String,
    values: DlPlusValues,
    toggle: Option<bool>,
    item_running: Option<bool>,
}

impl XPadAssembler {
    pub fn new() -> Self {
        XPadAssembler {
            toggle: None,
            charset: 0,
            segments: BTreeMap::new(),
            last_seg_num: None,
            last_ci: Vec::new(),
            command_segments: BTreeMap::new(),
            command_last_seg_num: None,
            dl_plus: None,
        }
    }

    /// Reset all reassembly state (call on service change).
    pub fn reset(&mut self) {
        self.toggle = None;
        self.charset = 0;
        self.segments.clear();
        self.last_seg_num = None;
        self.last_ci.clear();
        self.command_segments.clear();
        self.command_last_seg_num = None;
        self.dl_plus = None;
    }

    /// Process one MPEG Layer 2 frame and return DLS text if a complete label
    /// was received.
    ///
    /// F-PAD occupies the last 2 bytes of the frame; X-PAD (if present)
    /// occupies the bytes immediately before F-PAD.
    pub fn push_mp2_frame(&mut self, frame: &[u8]) -> Option<String> {
        self.push_mp2_frame_metadata(frame).map(|m| m.raw_text)
    }

    /// Process one MPEG Layer 2 frame and return structured metadata if a
    /// complete Dynamic Label was received.
    pub fn push_mp2_frame_metadata(&mut self, frame: &[u8]) -> Option<NowPlaying> {
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
        self.push_mp2_bytes_metadata(data).map(|m| m.raw_text)
    }

    /// Process raw MP2 bytes and return structured metadata if a complete
    /// Dynamic Label was received.
    pub fn push_mp2_bytes_metadata(&mut self, data: &[u8]) -> Option<NowPlaying> {
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
            if let Some(meta) = self.push_mp2_frame_metadata(&data[pos..pos + size]) {
                result = Some(meta);
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
    ///
    /// The DSE preserves the same byte layout as the MPEG Layer 2 ancillary
    /// data area: CI list at the right end (closest to F-PAD), data sub-fields
    /// growing leftward.  No byte reversal is needed.
    pub fn push_dabplus_au(&mut self, au_data: &[u8]) -> Option<String> {
        self.push_dabplus_au_metadata(au_data).map(|m| m.raw_text)
    }

    /// Process one DAB+ Access Unit and return structured metadata if a
    /// complete Dynamic Label was received.
    pub fn push_dabplus_au_metadata(&mut self, au_data: &[u8]) -> Option<NowPlaying> {
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
    fn process_fpad_xpad(&mut self, xpad_area: &[u8], fpad: [u8; 2]) -> Option<NowPlaying> {
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
            if app_type != APP_TYPE_DLS_START {
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
    fn process_dls_chunk(&mut self, chunk: &[u8]) -> Option<NowPlaying> {
        if chunk.len() < 4 {
            return None;
        }

        // Reference layout matches ODR-PadEnc / ETSI EN 300 401 §7.4.5.2:
        //   byte0: toggle bit7, first bit6, last bit5, command bit4, low4 = len-1 or command id
        //   byte1: for text first segment = charset<<4, continuation = seg_index<<4
        //          for DL+ command = optional link bit7 + low7 = len-1
        let header0 = chunk[0];
        let header1 = chunk[1];
        let is_command = (header0 & 0x10) != 0;
        let first = (header0 & 0x40) != 0;
        let last = (header0 & 0x20) != 0;
        let toggle = (header0 & 0x80) != 0;
        let cmd_or_len = header0 & 0x0F;
        let body_len = if is_command {
            // Command frames use a 7-bit payload length in byte1.
            if cmd_or_len == 0x02 {
                (header1 as usize & 0x7F) + 1
            } else {
                0
            }
        } else {
            (cmd_or_len as usize) + 1
        };
        if chunk.len() < 2 + body_len + 2 {
            log::debug!(
                "X-PAD DLS: truncated segment h0={:02X} h1={:02X} body_len={} chunk_len={}",
                header0,
                header1,
                body_len,
                chunk.len()
            );
            return None;
        }
        let payload = &chunk[2..2 + body_len];
        let field2 = header1 >> 4;
        let seg_num = if first { 0 } else { (field2 & 0x07) + 1 };

        log::debug!(
            "X-PAD DLS: h0={:02X} h1={:02X} first={} last={} toggle={} command={} seg={} body_len={} chunk_len={}",
            header0,
            header1,
            first,
            last,
            toggle,
            is_command,
            seg_num,
            body_len,
            chunk.len()
        );

        // Detect item change via toggle bit and reset in-progress buffers.
        if let Some(prev) = self.toggle {
            if toggle != prev {
                self.segments.clear();
                self.last_seg_num = None;
                self.command_segments.clear();
                self.command_last_seg_num = None;
                self.dl_plus = None;
                log::debug!(
                    "X-PAD DLS: toggle changed ({} → {}) — new label",
                    prev,
                    toggle
                );
            }
        }
        self.toggle = Some(toggle);

        if is_command {
            let command_id = cmd_or_len;
            if command_id == 0x01 {
                self.segments.clear();
                self.last_seg_num = None;
                log::debug!("X-PAD DLS: remove-label command received");
                return None;
            }
            if !payload.is_empty() {
                self.command_segments.insert(seg_num, payload.to_vec());
            }
            if last {
                self.command_last_seg_num = Some(seg_num);
            }
            if let Some(cmd_bytes) = self.try_assemble_command_segments() {
                self.dl_plus = parse_dl_plus_command(&cmd_bytes);
                if let Some(parsed) = self.try_assemble() {
                    return Some(now_playing_from_parsed(parsed));
                }
            }
            return None;
        }

        // For the first text segment, high nibble of byte1 carries the charset.
        if first {
            self.charset = header1 >> 4;
        }

        let mut text_bytes: Vec<u8> = payload.to_vec();
        while text_bytes.last() == Some(&0x00) {
            text_bytes.pop();
        }
        if !text_bytes.is_empty() {
            self.segments.insert(seg_num, text_bytes);
        }
        if last {
            self.last_seg_num = Some(seg_num);
        }

        let parsed = self.try_assemble()?;
        Some(now_playing_from_parsed(parsed))
    }

    /// Try to produce a complete label from accumulated segments.
    fn try_assemble(&self) -> Option<ParsedDls> {
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
            return None;
        }
        let values = apply_dl_plus_to_text(&text, self.dl_plus.as_ref());
        let item_running = self.dl_plus.as_ref().and_then(|d| d.item_running);
        Some(ParsedDls {
            text,
            values,
            toggle: self.toggle,
            item_running,
        })
    }

    fn try_assemble_command_segments(&self) -> Option<Vec<u8>> {
        let last = self.command_last_seg_num?;
        for i in 0..=last {
            self.command_segments.get(&i)?;
        }
        Some(
            (0..=last)
                .flat_map(|i| self.command_segments[&i].iter().copied())
                .collect(),
        )
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

    // Per dablin / ETSI EN 300 401 §7.4.2.2: at most 4 CI entries before the
    // end marker.  The end marker is a CI byte with app_type == 0 (bottom 5
    // bits all zero), NOT signalled by the length indicator.
    while ci_entries.len() < 4 {
        if pos == 0 {
            log::debug!("X-PAD CI: reached left edge without end marker");
            break;
        }
        pos -= 1;
        let ci = xpad_area[pos];
        // ETSI EN 300 401 §7.4.2.2, Table 2: variable-size X-PAD CI byte
        // is 3-bit sub-field length indicator (bits 7-5) + 5-bit application
        // type (bits 4-0).
        let length_code = (ci >> 5) & 0x07;
        let app_type = ci & 0x1F;

        // End marker: app_type == 0 terminates the CI list.
        if app_type == 0 {
            log::debug!("X-PAD CI: end marker at pos={} byte={:02X}", pos, ci,);
            break;
        }

        let len = ci_length(length_code);
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
        if *app_type == APP_TYPE_DLS_START || *app_type == APP_TYPE_DLS_CONT {
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

fn unix_ms_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn parse_dl_plus_command(bytes: &[u8]) -> Option<DlPlusFields> {
    if bytes.is_empty() {
        return None;
    }
    // DL+ command payload per ETSI TS 102 980 §7.3:
    //   byte 0: [link:4 = 0001 | IT:1 | IR:1 | NUM_TAGS-1:2]
    //   byte 1+: tag triplets [content_type, start_char, length_char]
    //
    // Some packet-mode paths prepend a legacy command id 0x02 before the
    // control byte; the control byte there is just NUM_TAGS-1 in bits 1-0
    // and we have no IT/IR in that variant.
    let (start, expected_tags, header_flags) = if bytes[0] == 0x02 && bytes.len() >= 2 {
        let control = bytes[1];
        let n = (control & 0x03) as usize + 1;
        (2, Some(n), None)
    } else if (bytes[0] >> 4) == 0x01 {
        let b0 = bytes[0];
        let it = (b0 & 0x08) != 0;
        let ir = (b0 & 0x04) != 0;
        let n = (b0 & 0x03) as usize + 1;
        (1, Some(n), Some((it, ir)))
    } else {
        (0, None, None)
    };
    if start > bytes.len() {
        return None;
    }
    let mut tags: Vec<(u8, usize, usize)> = Vec::new();
    let mut i = start;
    while i + 3 <= bytes.len() {
        tags.push((bytes[i], bytes[i + 1] as usize, bytes[i + 2] as usize));
        i += 3;
    }
    if let Some(expected) = expected_tags {
        tags.truncate(expected);
    }
    let (item_toggle, item_running) = match header_flags {
        Some((it, ir)) => (Some(it), Some(ir)),
        None => (None, None),
    };
    // A DL+ command can legitimately carry zero tags when the broadcaster
    // only wants to toggle IT/IR (e.g. to clear the displayed item).  Only
    // return None when we have neither tags nor header flags.
    if tags.is_empty() && item_toggle.is_none() && item_running.is_none() {
        return None;
    }
    Some(DlPlusFields {
        tags,
        item_toggle,
        item_running,
    })
}

fn apply_dl_plus_to_text(text: &str, dl_plus: Option<&DlPlusFields>) -> DlPlusValues {
    let mut out = DlPlusValues::default();
    let Some(dl_plus) = dl_plus else {
        return out;
    };
    let chars: Vec<char> = text.chars().collect();
    for (ty, start, len) in &dl_plus.tags {
        let start = *start;
        let len = *len;
        if len == 0 || start >= chars.len() {
            continue;
        }
        let end = (start + len).min(chars.len());
        let val = chars[start..end]
            .iter()
            .collect::<String>()
            .trim()
            .to_string();
        if val.is_empty() {
            continue;
        }
        // ETSI TS 102 980 Annex A, Table 9 — item.* content type codes.
        let slot = match *ty {
            0x01 => &mut out.title,
            0x02 => &mut out.album,
            0x03 => &mut out.track,
            0x04 => &mut out.artist,
            0x08 => &mut out.composer,
            0x09 => &mut out.band,
            0x0B => &mut out.genre,
            _ => continue,
        };
        if slot.is_none() {
            *slot = Some(val);
        }
    }
    out
}

fn now_playing_from_parsed(parsed: ParsedDls) -> NowPlaying {
    // When the broadcaster signals the item has stopped (IR=false), drop any
    // per-item fields so the UI can clear stale song details. The raw DLS
    // text is kept — it often carries a station slogan while idle.
    let item_stopped = matches!(parsed.item_running, Some(false));
    let values = if item_stopped {
        DlPlusValues::default()
    } else {
        parsed.values
    };
    NowPlaying {
        raw_text: parsed.text,
        title: values.title,
        artist: values.artist,
        album: values.album,
        track: values.track,
        composer: values.composer,
        band: values.band,
        genre: values.genre,
        toggle: parsed.toggle,
        item_running: parsed.item_running,
        source: Some(MetadataSource::XPad),
        updated_at_unix_ms: unix_ms_now(),
    }
}

// ─────────────────────────────────────────────────────────────────────────── //
//  Character set decoding                                                       //
// ─────────────────────────────────────────────────────────────────────────── //

fn decode_dls_text(bytes: &[u8], charset: u8) -> String {
    decode_dab_text(bytes, charset)
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

    fn build_dls_segment_physical(
        text: &[u8],
        toggle: bool,
        first: bool,
        last: bool,
        charset: u8,
        seg_num: u8,
    ) -> Vec<u8> {
        let mut byte0 = 0u8;
        if toggle {
            byte0 |= 1 << 7;
        }
        if first {
            byte0 |= 1 << 6;
        }
        if last {
            byte0 |= 1 << 5;
        }
        byte0 |= text.len().saturating_sub(1) as u8 & 0x0F;
        let byte1 = if first {
            (charset & 0x0F) << 4
        } else {
            ((seg_num.saturating_sub(1)) & 0x07) << 4
        };
        let mut logical = Vec::with_capacity(2 + text.len() + 2);
        logical.push(byte0);
        logical.push(byte1);
        logical.extend_from_slice(text);
        logical.extend_from_slice(&[0x00, 0x00]); // CRC placeholder
        logical.reverse();
        logical
    }

    fn build_dl_plus_command_physical(payload: &[u8], toggle: bool) -> Vec<u8> {
        let byte0 = (if toggle { 1 << 7 } else { 0 }) | (1 << 6) | (1 << 5) | (1 << 4) | 0x02;
        let byte1 = (payload.len().saturating_sub(1) as u8) & 0x7F;
        let mut logical = Vec::with_capacity(2 + payload.len() + 2);
        logical.push(byte0);
        logical.push(byte1);
        logical.extend_from_slice(payload);
        logical.extend_from_slice(&[0x00, 0x00]); // CRC placeholder
        logical.reverse();
        logical
    }

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
        // CI byte (3+5 split): length_code=0 (4 bytes), app_type=2 → (0<<5)|2 = 0x02
        // End marker: app_type=0 → any byte with bottom 5 bits = 0 (e.g. 0x00)
        // DLS data: 4 bytes in physical order (byte 0 = cmd is RIGHTMOST per spec)
        //   Logical: [cmd=0xC0, charset=0x00, 'A', 'B']
        //   Physical (rightmost = byte 0): ['B', 'A', 0x00, 0xC0]
        let dls_data_physical = [0x42u8, 0x41, 0x00, 0xC0]; // 'B' 'A' charset cmd
        let end = 0x00u8; // end marker: app_type=0 (left of CI)
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
        // CI with app_type=5 (not DLS).  Layout: data | end_marker | CI.
        let data = [0u8; 4];
        let mut xpad = data.to_vec();
        xpad.push(0x00); // end marker: app_type=0 (left of CI)
        xpad.push(0x05); // CI (3+5): length_code=0 (4 bytes), app_type=5
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
        let dls_chunk = build_dls_segment_physical(b"Hi", false, true, true, 0, 0);
        let end = 0x00u8; // end marker: app_type=0 (left of CI entries)
        let ci = 0x22u8; // CI (3+5): length_code=1 (6 bytes), app_type=2 (rightmost, closest to F-PAD)

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
        let dls1 = build_dls_segment_physical(b"AA", false, true, true, 0, 0);
        let dls2 = build_dls_segment_physical(b"BB", true, true, true, 0, 0);

        // Frame layout: fake_audio | dls_chunk | end_marker | CI | F-PAD
        let build_frame = |dls: &[u8]| {
            let mut f = vec![0u8; 4]; // fake audio
            f.extend_from_slice(dls);
            f.push(0x00); // end marker: app_type=0 (left of CI)
            f.push(0x22); // CI (3+5): length_code=1 (6 bytes), app_type=2
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
        let dls_chunk = build_dls_segment_physical(b"OK", false, true, true, 0, 0);

        // Frame 1: explicit CI list present (ci_flag=1).
        // Layout: dls_chunk | end_marker | CI | F-PAD
        let mut frame1 = vec![0u8; 4]; // fake audio
        frame1.extend_from_slice(&dls_chunk);
        frame1.push(0x00); // end marker: app_type=0
        frame1.push(0x22); // CI (3+5): length_code=1 (6 bytes), app_type=2
        frame1.push(0x20); // F-PAD byte0: bits 7-6=00 (standard), bits 5-4=10 (variable X-PAD)
        frame1.push(0x02); // F-PAD byte1: bit 1 = 1 (CI flag set)

        let r1 = asm.push_mp2_frame(&frame1);
        assert_eq!(r1, Some("OK".to_string()));

        // Frame 2: continuation (ci_flag=0).
        // Layout: dls_chunk | F-PAD  (no CI bytes — entire xpad area is data)
        // Frame 2: "Go" with toggle=1. Physical order (byte 0 = cmd is rightmost).
        // Logical: [0x0E, 0x00, 'G'=0x47, 'o'=0x6F, 0x00, 0x00]
        // Physical: [0x00, 0x00, 'o', 'G', 0x00, 0x0E]
        let dls_chunk2 = build_dls_segment_physical(b"Go", true, true, true, 0, 0);
        let mut frame2 = vec![0u8; 4]; // fake audio
        frame2.extend_from_slice(&dls_chunk2);
        // F-PAD byte0: bits 7-6=00 (standard), bits 5-4=10 (variable X-PAD), CI flag NOT set
        frame2.push(0x20); // 0b00100000
        frame2.push(0x00); // F-PAD byte1: bit 1 = 0 (no CI list)

        let r2 = asm.push_mp2_frame(&frame2);
        assert_eq!(r2, Some("Go".to_string()));
    }

    #[test]
    fn assembler_two_segment_label_uses_minus_one_segment_numbering() {
        let mut asm = XPadAssembler::new();

        let seg0 = build_dls_segment_physical(b"Te", false, true, false, 0, 0);
        let seg1 = build_dls_segment_physical(b"xt", false, false, true, 0, 1);

        let build_frame = |dls: &[u8]| {
            let mut f = vec![0u8; 4];
            f.extend_from_slice(dls);
            f.push(0x00);
            f.push(0x22);
            f.push(0x20);
            f.push(0x02);
            f
        };

        assert!(asm.push_mp2_frame(&build_frame(&seg1)).is_none());
        let result = asm.push_mp2_frame(&build_frame(&seg0));
        assert_eq!(result, Some("Text".to_string()));
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

        // DSE preserves the same byte layout as DAB (MPEG): CI at right end,
        // data sub-fields growing leftward, byte 0 of each sub-field closest
        // to CI (rightmost).
        //
        // DLS chunk (6 bytes, length_code=1) — physical order (byte 0 = cmd
        // is rightmost, closest to CI):
        //   [pad, pad, 'i', 'H', charset=0x00, cmd=0x06]
        let ci = 0x22u8; // CI (3+5): length_code=1 (6 bytes), app_type=2 (DLS)
        let end = 0x00u8; // CI end marker: app_type=0

        // PAD field: [dls_chunk(physical) | end_marker | CI | F-PAD]
        let dls_physical = build_dls_segment_physical(b"Hi", false, true, true, 0, 0);
        let mut pad: Vec<u8> = dls_physical.to_vec();
        pad.push(end);
        pad.push(ci);
        pad.push(0x20); // F-PAD byte0: variable X-PAD
        pad.push(0x02); // F-PAD byte1: CI flag set

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
        let s = decode_dls_text(bytes, 0x0F);
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
        // EBU Latin Annex C differs from ISO-8859-1 for many high bytes.
        let bytes = [0x24u8, 0x5C, 0x80]; // ł Ů á
        let s = decode_dls_text(&bytes, 0);
        assert_eq!(s, "łŮá");
    }

    #[test]
    fn dl_plus_tags_extract_title_artist() {
        // One DL+ command payload as emitted by ODR-PadEnc:
        // title type 0x01 at chars 0..4, artist type 0x04 at chars 8..13.
        let cmd = [0x11u8, 0x01, 0, 5, 0x04, 8, 6];
        let dlp = parse_dl_plus_command(&cmd).expect("expected tags");
        let values = apply_dl_plus_to_text("Title - Artist", Some(&dlp));
        assert_eq!(values.title.as_deref(), Some("Title"));
        assert_eq!(values.artist.as_deref(), Some("Artist"));
    }

    #[test]
    fn dl_plus_tags_extract_album_track_genre() {
        // Tag layout against "Song 03 Album Rock":
        //   0x01 title      offset  0 len 4  → "Song"
        //   0x03 track      offset  5 len 2  → "03"
        //   0x02 album      offset  8 len 5  → "Album"
        //   0x0B genre      offset 14 len 4  → "Rock"
        // NUM_TAGS-1 is a 2-bit field so we split the tags across two DL+
        // command payloads with IR=1 to smoke-test merge semantics.
        let cmd_a = [0x16u8, 0x01, 0, 4, 0x03, 5, 2, 0x02, 8, 5];
        let cmd_b = [0x14u8, 0x0B, 14, 4];
        let dlp_a = parse_dl_plus_command(&cmd_a).expect("expected tags");
        let dlp_b = parse_dl_plus_command(&cmd_b).expect("expected genre tag");
        let values_a = apply_dl_plus_to_text("Song 03 Album Rock", Some(&dlp_a));
        assert_eq!(values_a.title.as_deref(), Some("Song"));
        assert_eq!(values_a.track.as_deref(), Some("03"));
        assert_eq!(values_a.album.as_deref(), Some("Album"));
        let values_b = apply_dl_plus_to_text("Song 03 Album Rock", Some(&dlp_b));
        assert_eq!(values_b.genre.as_deref(), Some("Rock"));
    }

    #[test]
    fn dl_plus_drops_non_standard_content_types() {
        // 0x1F/0x20 used to map to title/artist — no longer per TS 102 980.
        let cmd = [0x12u8, 0x1F, 0, 5, 0x20, 6, 6];
        let dlp = parse_dl_plus_command(&cmd).expect("expected tags");
        let values = apply_dl_plus_to_text("XXXXX YYYYYY", Some(&dlp));
        assert_eq!(values.title, None);
        assert_eq!(values.artist, None);
    }

    #[test]
    fn dl_plus_header_extracts_item_running_and_toggle() {
        // byte 0 = 0x1D = 0001_1101 → link=1, IT=1, IR=1, num_tags-1=1.
        let cmd = [0x1Du8, 0x01, 0, 5, 0x04, 8, 6];
        let dlp = parse_dl_plus_command(&cmd).expect("expected tags");
        assert_eq!(dlp.item_toggle, Some(true));
        assert_eq!(dlp.item_running, Some(true));
        assert_eq!(dlp.tags.len(), 2);

        // byte 0 = 0x10 = 0001_0000 → IT=0, IR=0, num_tags-1=0 (1 tag).
        let cmd2 = [0x10u8, 0x01, 0, 5];
        let dlp2 = parse_dl_plus_command(&cmd2).expect("expected tags");
        assert_eq!(dlp2.item_toggle, Some(false));
        assert_eq!(dlp2.item_running, Some(false));
    }

    #[test]
    fn dl_plus_item_running_false_clears_song_fields() {
        let mut asm = XPadAssembler::new();
        let text_chunk = build_dls_segment_physical(b"TitleArtist", false, true, true, 0, 0);
        // IR=1 first — title/artist should populate.
        let cmd_running = [0x15u8, 0x01, 0, 5, 0x04, 5, 6];
        let cmd_running_chunk = build_dl_plus_command_physical(&cmd_running, false);

        let mut frame1 = vec![0u8; 8];
        frame1.extend_from_slice(&text_chunk);
        frame1.push(0x00);
        frame1.push(0x82);
        frame1.push(0x20);
        frame1.push(0x02);
        asm.push_mp2_frame_metadata(&frame1).expect("text");

        let mut logical_running = cmd_running_chunk.clone();
        logical_running.reverse();
        let running = asm
            .process_dls_chunk(&logical_running)
            .expect("expected IR=1 metadata");
        assert_eq!(running.title.as_deref(), Some("Title"));
        assert_eq!(running.artist.as_deref(), Some("Artist"));
        assert_eq!(running.item_running, Some(true));

        // Now the broadcaster signals IR=0 — song fields should be cleared
        // even though the tags still identify character ranges.
        let cmd_stopped = [0x11u8, 0x01, 0, 5, 0x04, 5, 6];
        let cmd_stopped_chunk = build_dl_plus_command_physical(&cmd_stopped, false);
        let mut logical_stopped = cmd_stopped_chunk.clone();
        logical_stopped.reverse();
        let stopped = asm
            .process_dls_chunk(&logical_stopped)
            .expect("expected IR=0 metadata");
        assert_eq!(stopped.item_running, Some(false));
        assert_eq!(stopped.title, None);
        assert_eq!(stopped.artist, None);
        // raw_text survives — stations often broadcast a slogan while idle.
        assert_eq!(stopped.raw_text, "TitleArtist");
    }

    #[test]
    fn assembler_text_then_dl_plus_command_extracts_artist_title() {
        let mut asm = XPadAssembler::new();
        let text_chunk = build_dls_segment_physical(b"TitleArtist", false, true, true, 0, 0);
        // 0x15 = 0001_0101 → link=1, IT=0, IR=1, num_tags-1=1 (2 tags).
        let cmd_payload = [0x15u8, 0x01, 0, 5, 0x04, 5, 6];
        let cmd_chunk = build_dl_plus_command_physical(&cmd_payload, false);

        let mut frame1 = vec![0u8; 8];
        frame1.extend_from_slice(&text_chunk);
        frame1.push(0x00);
        frame1.push(0x82);
        frame1.push(0x20);
        frame1.push(0x02);
        let meta1 = asm.push_mp2_frame_metadata(&frame1).expect("expected text");
        assert_eq!(meta1.raw_text, "TitleArtist");

        let mut logical_cmd = cmd_chunk.clone();
        logical_cmd.reverse();
        let refreshed = asm
            .process_dls_chunk(&logical_cmd)
            .expect("expected refreshed metadata");
        assert_eq!(refreshed.title.as_deref(), Some("Title"));
        assert_eq!(refreshed.artist.as_deref(), Some("Artist"));
    }

    #[test]
    fn dl_plus_command_emits_metadata_for_current_text() {
        let mut asm = XPadAssembler::new();
        let text_chunk = build_dls_segment_physical(b"TitleArtist", false, true, true, 0, 0);
        // 0x15 = 0001_0101 → link=1, IT=0, IR=1, num_tags-1=1 (2 tags).
        let cmd_payload = [0x15u8, 0x01, 0, 5, 0x04, 5, 6];
        let cmd_chunk = build_dl_plus_command_physical(&cmd_payload, false);

        let mut logical_text = text_chunk.clone();
        logical_text.reverse();
        let text_meta = asm
            .process_dls_chunk(&logical_text)
            .expect("expected text metadata");
        assert_eq!(text_meta.raw_text, "TitleArtist");
        assert_eq!(text_meta.title, None);
        assert_eq!(text_meta.artist, None);

        let mut logical_cmd = cmd_chunk.clone();
        logical_cmd.reverse();
        let cmd_meta = asm
            .process_dls_chunk(&logical_cmd)
            .expect("expected metadata refresh from DL+ command");
        assert_eq!(cmd_meta.raw_text, "TitleArtist");
        assert_eq!(cmd_meta.title.as_deref(), Some("Title"));
        assert_eq!(cmd_meta.artist.as_deref(), Some("Artist"));
        assert_eq!(cmd_meta.source, Some(MetadataSource::XPad));
    }

    #[test]
    fn push_mp2_frame_metadata_sets_source() {
        let mut asm = XPadAssembler::new();
        let dls_chunk = build_dls_segment_physical(b"Hi", false, true, true, 0, 0);
        let mut frame = vec![0u8; 8];
        frame.extend_from_slice(&dls_chunk);
        frame.push(0x00);
        frame.push(0x22);
        frame.push(0x20);
        frame.push(0x02);
        let meta = asm
            .push_mp2_frame_metadata(&frame)
            .expect("expected metadata");
        assert_eq!(meta.raw_text, "Hi");
        assert_eq!(meta.source, Some(MetadataSource::XPad));
    }
}
