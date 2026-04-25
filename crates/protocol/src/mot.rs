//! MSC Data Group and MOT (Multimedia Object Transfer) parsing.
//!
//! MSC Data Group framing per ETSI EN 300 401 §5.3.3.1.
//! MOT header encoding per ETSI EN 301 234 §6.
//!
//! Two public entry points:
//! - [`parse_msc_data_group`] — one pre-assembled MSC-DG → header fields + payload.
//! - [`MotAssembler`] — stateful collector that ingests MSC-DG bytes (type 3 =
//!   MOT header, type 4 = MOT body) keyed by Transport Id, and emits a
//!   [`MotObject`] when body_size bytes have been accumulated.

use std::collections::HashMap;

// ─────────────────────────────────────────────────────────────────────────── //
//  MSC Data Group                                                              //
// ─────────────────────────────────────────────────────────────────────────── //

/// Data group type 3 = MOT header.
pub const DG_TYPE_MOT_HEADER: u8 = 3;
/// Data group type 4 = MOT body (segmented).
pub const DG_TYPE_MOT_BODY: u8 = 4;

/// Parsed MSC Data Group fields plus the payload slice.
#[derive(Debug, Clone)]
pub struct MscDataGroup<'a> {
    pub data_group_type: u8,
    pub continuity_index: u8,
    pub repetition_index: u8,
    pub segment_flag: bool,
    pub user_access_flag: bool,
    pub crc_present: bool,
    /// `(segment_number, last)` when the segment field was present.
    pub segment: Option<(u16, bool)>,
    /// Transport Id when the user-access field had `TransportIdFlag=1`.
    pub transport_id: Option<u16>,
    /// Data-group data field. When `segment_flag` is set, the first two bytes
    /// are the segmentation header (repetition count + 13-bit size); the
    /// returned `payload` slice has those two bytes stripped and is already
    /// truncated to the segment size when present.
    pub payload: &'a [u8],
    /// `true` when the CRC was absent (nothing to verify) or verified. `false`
    /// when a CRC was signalled but the check failed.
    pub crc_ok: bool,
    /// Total byte length of the DG within the input buffer (header + optional
    /// extension/segment/UA fields + payload + optional CRC). Useful for
    /// callers that greedy-parse a stream of concatenated DGs.
    pub dg_len: usize,
}

/// Parse one MSC Data Group. Returns `None` when the buffer is too short for
/// the claimed framing.
///
/// `bytes` is expected to hold exactly one MSC-DG (header + optional
/// extension + optional session header + payload + optional CRC). In
/// packet-mode DAB, these come pre-assembled by the packet re-assembler.
pub fn parse_msc_data_group(bytes: &[u8]) -> Option<MscDataGroup<'_>> {
    if bytes.len() < 2 {
        return None;
    }
    let b0 = bytes[0];
    let b1 = bytes[1];
    let extension_flag = (b0 & 0x80) != 0;
    let crc_present = (b0 & 0x40) != 0;
    let segment_flag = (b0 & 0x20) != 0;
    let user_access_flag = (b0 & 0x10) != 0;
    let data_group_type = b0 & 0x0F;
    let continuity_index = (b1 >> 4) & 0x0F;
    let repetition_index = b1 & 0x0F;

    let mut idx = 2;
    if extension_flag {
        if bytes.len() < idx + 2 {
            return None;
        }
        idx += 2;
    }

    let segment = if segment_flag {
        if bytes.len() < idx + 2 {
            return None;
        }
        let seg_hi = bytes[idx];
        let seg_lo = bytes[idx + 1];
        let last = (seg_hi & 0x80) != 0;
        let segment_number = (((seg_hi & 0x7F) as u16) << 8) | seg_lo as u16;
        idx += 2;
        Some((segment_number, last))
    } else {
        None
    };

    let mut transport_id = None;
    if user_access_flag {
        if bytes.len() <= idx {
            return None;
        }
        let ua = bytes[idx];
        idx += 1;
        let tid_flag = (ua & 0x10) != 0;
        // Length indicator counts bytes that follow, comprising the 2-byte
        // Transport Id (if present) plus any End-user address field.
        let length_indicator = (ua & 0x0F) as usize;
        if bytes.len() < idx + length_indicator {
            return None;
        }
        if tid_flag {
            if length_indicator < 2 {
                return None;
            }
            transport_id = Some(((bytes[idx] as u16) << 8) | bytes[idx + 1] as u16);
        }
        idx += length_indicator;
    }

    // When segment_flag is set the first two bytes of the data field are a
    // segmentation header [repetition_count(3) | size(13)], which tells us
    // exactly how long the payload is. We use that to pin down where the DG
    // ends (and where the trailing CRC lives) instead of blindly trusting
    // the buffer length — the buffer may carry trailing padding, e.g. when
    // the DG is embedded in a variable X-PAD sub-field whose length is
    // dictated by a CI length code rather than the DG.
    //
    // When segment_flag is unset, fall back to the full buffer (the caller
    // is expected to pass exactly one DG).
    let (payload, crc_end): (&[u8], usize) = if segment_flag {
        if bytes.len() < idx + 2 {
            return None;
        }
        let size = (((bytes[idx] & 0x1F) as usize) << 8) | bytes[idx + 1] as usize;
        let payload_start = idx + 2;
        let payload_end = payload_start.checked_add(size)?;
        let expected_end = payload_end + if crc_present { 2 } else { 0 };
        if bytes.len() < expected_end {
            // Claimed segment size exceeds available bytes — reject.
            return None;
        }
        (&bytes[payload_start..payload_end], expected_end)
    } else {
        let end = bytes.len();
        let payload_end = if crc_present {
            end.checked_sub(2)?
        } else {
            end
        };
        if payload_end < idx {
            return None;
        }
        (&bytes[idx..payload_end], end)
    };

    let crc_ok = if crc_present {
        if crc_end < 2 {
            return None;
        }
        let crc_idx = crc_end - 2;
        let data_crc = ((bytes[crc_idx] as u16) << 8) | bytes[crc_idx + 1] as u16;
        let computed = crc16_ccitt(&bytes[..crc_idx]);
        computed == data_crc
    } else {
        true
    };

    Some(MscDataGroup {
        data_group_type,
        continuity_index,
        repetition_index,
        segment_flag,
        user_access_flag,
        crc_present,
        segment,
        transport_id,
        payload,
        crc_ok,
        dg_len: crc_end,
    })
}

/// CRC-16/CCITT used by MSC data groups. Polynomial 0x1021, initial value
/// 0xFFFF, output complemented (matches FIB CRC).
pub(crate) fn crc16_ccitt(bytes: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in bytes {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            if (crc & 0x8000) != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    !crc
}

// ─────────────────────────────────────────────────────────────────────────── //
//  MOT header                                                                  //
// ─────────────────────────────────────────────────────────────────────────── //

// Common MOT header parameter IDs used by DAB broadcasters. Values match
// widely-deployed encoders (ODR-PadEnc / mot-encoder) and TS 101 499.
const PARAM_CONTENT_NAME: u8 = 0x0C;
const PARAM_MIME_TYPE: u8 = 0x10;
const PARAM_CATEGORY_TITLE: u8 = 0x26;
const PARAM_TRIGGER_TIME: u8 = 0x05;
const PARAM_EXPIRE_TIME: u8 = 0x04;

/// Parsed MOT header (EN 301 234 §6.1 + parameter list §6.2).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MotHeader {
    pub body_size: u32,
    pub header_size: u16,
    pub content_type: u8,
    pub content_subtype: u16,
    pub content_name: Option<String>,
    pub mime_type: Option<String>,
    pub category_title: Option<String>,
    pub trigger_time: Option<Vec<u8>>,
    pub expire_time: Option<Vec<u8>>,
}

impl MotHeader {
    /// Return an `image/<fmt>` MIME guess derived from `content_type` +
    /// `content_subtype` when the broadcaster hasn't supplied an explicit
    /// `mime_type` parameter.
    pub fn inferred_mime(&self) -> Option<&'static str> {
        if let Some(mt) = &self.mime_type {
            // Only return an inferred value — caller can fall back to
            // `mime_type` directly when preferred.
            let _ = mt;
        }
        match (self.content_type, self.content_subtype) {
            (2, 0x000) => Some("image/gif"),
            (2, 0x001) => Some("image/jpeg"),
            (2, 0x002) => Some("image/bmp"),
            (2, 0x003) => Some("image/png"),
            _ => None,
        }
    }

    /// Best-effort filename extension from MIME (`mime_type` parameter
    /// preferred, falling back to the `content_type`/`content_subtype` table).
    pub fn preferred_extension(&self) -> &'static str {
        let mime = self
            .mime_type
            .as_deref()
            .or_else(|| self.inferred_mime())
            .unwrap_or("");
        match mime {
            "image/png" => "png",
            "image/jpeg" | "image/jpg" => "jpg",
            "image/gif" => "gif",
            "image/bmp" => "bmp",
            _ => "bin",
        }
    }
}

/// Parse the 7-byte MOT header core plus the header extension parameters.
/// Returns `None` when the buffer is too short or `header_size` is
/// inconsistent with the supplied bytes.
pub fn parse_mot_header(bytes: &[u8]) -> Option<MotHeader> {
    if bytes.len() < 7 {
        return None;
    }
    // MOT header core (56 bits):
    //   BodySize       28 bits
    //   HeaderSize     13 bits
    //   ContentType     6 bits
    //   ContentSubType  9 bits
    let w = u64::from_be_bytes([
        0, bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6],
    ]);
    let body_size = ((w >> 28) & 0x0FFF_FFFF) as u32;
    let header_size = ((w >> 15) & 0x1FFF) as u16;
    let content_type = ((w >> 9) & 0x3F) as u8;
    let content_subtype = (w & 0x1FF) as u16;

    // header_size counts the ENTIRE MOT header (core + parameter list).
    let header_size_bytes = header_size as usize;
    if header_size_bytes < 7 {
        return None;
    }
    if bytes.len() < header_size_bytes {
        return None;
    }

    let mut hdr = MotHeader {
        body_size,
        header_size,
        content_type,
        content_subtype,
        ..Default::default()
    };

    parse_mot_parameters(&bytes[7..header_size_bytes], &mut hdr);
    Some(hdr)
}

fn parse_mot_parameters(params: &[u8], hdr: &mut MotHeader) {
    // Each parameter is a 1-byte control field:
    //   PLI (2 bits) | ParamId (6 bits)
    // PLI selects the data-field length:
    //   0b00 = 0 bytes (flag-only)
    //   0b01 = 1 byte
    //   0b10 = 4 bytes
    //   0b11 = length indicator follows
    //     first byte bit7=0 → length in bits 6-0 (0..127)
    //     first byte bit7=1 → 2-byte length field: bits 14-0
    let mut i = 0usize;
    while i < params.len() {
        let ctrl = params[i];
        i += 1;
        let pli = (ctrl >> 6) & 0x03;
        let id = ctrl & 0x3F;
        let data_len = match pli {
            0b00 => 0usize,
            0b01 => 1,
            0b10 => 4,
            0b11 => {
                if i >= params.len() {
                    return;
                }
                let first = params[i];
                i += 1;
                if (first & 0x80) == 0 {
                    (first & 0x7F) as usize
                } else {
                    if i >= params.len() {
                        return;
                    }
                    let second = params[i];
                    i += 1;
                    (((first & 0x7F) as usize) << 8) | second as usize
                }
            }
            _ => 0,
        };
        if i + data_len > params.len() {
            return;
        }
        let data = &params[i..i + data_len];
        i += data_len;
        assign_mot_parameter(id, data, hdr);
    }
}

fn assign_mot_parameter(id: u8, data: &[u8], hdr: &mut MotHeader) {
    match id {
        PARAM_CONTENT_NAME if data.len() >= 2 => {
            // TS 101 499 §4.1.4: [Charset(4) | Rfa(4) | Name bytes]
            let charset = (data[0] >> 4) & 0x0F;
            let name_bytes = &data[1..];
            hdr.content_name = Some(crate::text::decode_dab_text(name_bytes, charset));
        }
        PARAM_MIME_TYPE => {
            hdr.mime_type = Some(
                String::from_utf8_lossy(data)
                    .trim_matches(char::from(0))
                    .to_string(),
            );
        }
        PARAM_CATEGORY_TITLE => {
            hdr.category_title = Some(
                String::from_utf8_lossy(data)
                    .trim_matches(char::from(0))
                    .to_string(),
            );
        }
        PARAM_TRIGGER_TIME => {
            hdr.trigger_time = Some(data.to_vec());
        }
        PARAM_EXPIRE_TIME => {
            hdr.expire_time = Some(data.to_vec());
        }
        _ => { /* ignore unknown parameters */ }
    }
}

// ─────────────────────────────────────────────────────────────────────────── //
//  MotAssembler                                                                //
// ─────────────────────────────────────────────────────────────────────────── //

/// A fully reassembled MOT object (header + body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MotObject {
    pub transport_id: u16,
    pub header: MotHeader,
    pub body: Vec<u8>,
}

#[derive(Default)]
struct TransportState {
    header: Option<MotHeader>,
    header_segments: Vec<Option<Vec<u8>>>,
    header_last: Option<u16>,
    body_segments: Vec<Option<Vec<u8>>>,
    body_last: Option<u16>,
}

impl TransportState {
    fn assemble_header(&mut self) -> Option<MotHeader> {
        if self.header.is_some() {
            return self.header.clone();
        }
        let last = self.header_last?;
        let mut bytes = Vec::new();
        for i in 0..=last as usize {
            let seg = self.header_segments.get(i).and_then(|s| s.as_ref())?;
            bytes.extend_from_slice(seg);
        }
        let parsed = parse_mot_header(&bytes)?;
        self.header = Some(parsed.clone());
        Some(parsed)
    }

    fn assemble_body(&self, expected_size: u32) -> Option<Vec<u8>> {
        let last = self.body_last?;
        let mut bytes = Vec::new();
        for i in 0..=last as usize {
            let seg = self.body_segments.get(i).and_then(|s| s.as_ref())?;
            bytes.extend_from_slice(seg);
        }
        if (bytes.len() as u64) < expected_size as u64 {
            return None;
        }
        bytes.truncate(expected_size as usize);
        Some(bytes)
    }
}

/// Upper bound on MOT body size we'll accept, to avoid runaway allocations
/// from corrupt streams. 4 MiB is much larger than any real DAB slide.
const MAX_BODY_SIZE: u32 = 4 * 1024 * 1024;

/// Stateful MOT segment assembler keyed by Transport Id.
#[derive(Default)]
pub struct MotAssembler {
    by_transport: HashMap<u16, TransportState>,
}

impl MotAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset all in-progress transports (call on service change).
    pub fn reset(&mut self) {
        self.by_transport.clear();
    }

    /// Feed one pre-assembled MSC Data Group. Returns a completed MOT object
    /// when the last outstanding segment of that transport arrives.
    pub fn push_msc_data_group(&mut self, dg_bytes: &[u8]) -> Option<MotObject> {
        let dg = parse_msc_data_group(dg_bytes)?;
        if !dg.crc_ok {
            log::debug!(
                "MOT: MSC-DG type={} CRC mismatch — dropping segment",
                dg.data_group_type
            );
            return None;
        }
        self.push_parsed(&dg)
    }

    /// Feed an already-parsed MSC Data Group (callers holding a parsed view
    /// can skip re-parsing).
    pub fn push_parsed(&mut self, dg: &MscDataGroup<'_>) -> Option<MotObject> {
        let transport_id = dg.transport_id?;
        let (seg_num, last) = dg.segment?;
        if dg.data_group_type != DG_TYPE_MOT_HEADER && dg.data_group_type != DG_TYPE_MOT_BODY {
            return None;
        }
        let state = self.by_transport.entry(transport_id).or_default();

        let seg_idx = seg_num as usize;
        match dg.data_group_type {
            DG_TYPE_MOT_HEADER => {
                ensure_slot(&mut state.header_segments, seg_idx);
                state.header_segments[seg_idx] = Some(dg.payload.to_vec());
                if last {
                    state.header_last = Some(seg_num);
                }
            }
            DG_TYPE_MOT_BODY => {
                ensure_slot(&mut state.body_segments, seg_idx);
                state.body_segments[seg_idx] = Some(dg.payload.to_vec());
                if last {
                    state.body_last = Some(seg_num);
                }
            }
            _ => {}
        }

        let header = state.assemble_header()?;
        if header.body_size == 0 || header.body_size > MAX_BODY_SIZE {
            log::debug!(
                "MOT: rejecting transport_id={:04X} body_size={} (>{} or zero)",
                transport_id,
                header.body_size,
                MAX_BODY_SIZE
            );
            // Drop the in-progress state so a later, valid assembly under
            // the same transport id can succeed.
            self.by_transport.remove(&transport_id);
            return None;
        }
        let body = state.assemble_body(header.body_size)?;
        // Reset transport state after a successful assembly so the next
        // carousel cycle can re-trigger emission. Callers dedup by body
        // hash to avoid UI churn from repeated identical slides.
        self.by_transport.remove(&transport_id);
        Some(MotObject {
            transport_id,
            header,
            body,
        })
    }
}

fn ensure_slot(segs: &mut Vec<Option<Vec<u8>>>, idx: usize) {
    if segs.len() <= idx {
        segs.resize(idx + 1, None);
    }
}

// ─────────────────────────────────────────────────────────────────────────── //
//  Tests                                                                       //
// ─────────────────────────────────────────────────────────────────────────── //

#[cfg(test)]
mod tests {
    use super::*;

    fn build_msc_dg(
        data_group_type: u8,
        continuity: u8,
        repetition: u8,
        segment: Option<(u16, bool)>,
        transport_id: Option<u16>,
        payload: &[u8],
        with_crc: bool,
    ) -> Vec<u8> {
        let seg_flag = segment.is_some();
        let ua_flag = transport_id.is_some();
        let mut out = Vec::new();
        // ext flag = 0 (not emitted, so no bit set)
        let b0 = ((with_crc as u8) << 6)
            | ((seg_flag as u8) << 5)
            | ((ua_flag as u8) << 4)
            | (data_group_type & 0x0F);
        let b1 = ((continuity & 0x0F) << 4) | (repetition & 0x0F);
        out.push(b0);
        out.push(b1);
        if let Some((seg_num, last)) = segment {
            let hi = ((last as u8) << 7) | ((seg_num >> 8) as u8 & 0x7F);
            let lo = (seg_num & 0xFF) as u8;
            out.push(hi);
            out.push(lo);
        }
        if let Some(tid) = transport_id {
            // Rfa=000, TId flag=1, length indicator=2 (just the TId).
            out.push((1 << 4) | 0x02);
            out.push((tid >> 8) as u8);
            out.push((tid & 0xFF) as u8);
        }
        // Data field: segmentation header when seg flag set, then payload.
        if seg_flag {
            let size = payload.len() as u16;
            out.push(((size >> 8) & 0x1F) as u8); // rep count=0, size hi 5 bits
            out.push((size & 0xFF) as u8);
            out.extend_from_slice(payload);
        } else {
            out.extend_from_slice(payload);
        }
        if with_crc {
            let crc = crc16_ccitt(&out);
            out.push((crc >> 8) as u8);
            out.push((crc & 0xFF) as u8);
        }
        out
    }

    fn build_mot_header(
        body_size: u32,
        content_type: u8,
        content_subtype: u16,
        params: &[u8],
    ) -> Vec<u8> {
        let header_size = (7 + params.len()) as u16;
        let mut bytes = Vec::with_capacity(header_size as usize);
        // Pack: body_size(28) | header_size(13) | content_type(6) | content_subtype(9)
        let w: u64 = ((body_size as u64 & 0x0FFF_FFFF) << 28)
            | ((header_size as u64 & 0x1FFF) << 15)
            | ((content_type as u64 & 0x3F) << 9)
            | (content_subtype as u64 & 0x1FF);
        // Serialise as 7 big-endian bytes.
        bytes.push(((w >> 48) & 0xFF) as u8);
        bytes.push(((w >> 40) & 0xFF) as u8);
        bytes.push(((w >> 32) & 0xFF) as u8);
        bytes.push(((w >> 24) & 0xFF) as u8);
        bytes.push(((w >> 16) & 0xFF) as u8);
        bytes.push(((w >> 8) & 0xFF) as u8);
        bytes.push((w & 0xFF) as u8);
        bytes.extend_from_slice(params);
        bytes
    }

    #[test]
    fn parse_msc_dg_roundtrip_with_crc() {
        let payload = b"hello";
        let bytes = build_msc_dg(
            DG_TYPE_MOT_HEADER,
            1,
            0,
            Some((0, true)),
            Some(0x1234),
            payload,
            true,
        );
        let dg = parse_msc_data_group(&bytes).expect("parse");
        assert_eq!(dg.data_group_type, DG_TYPE_MOT_HEADER);
        assert_eq!(dg.continuity_index, 1);
        assert_eq!(dg.segment, Some((0, true)));
        assert_eq!(dg.transport_id, Some(0x1234));
        assert_eq!(dg.payload, payload);
        assert!(dg.crc_ok);
    }

    #[test]
    fn parse_msc_dg_rejects_bad_crc() {
        let mut bytes = build_msc_dg(
            DG_TYPE_MOT_BODY,
            0,
            0,
            Some((0, true)),
            Some(0x1234),
            b"body",
            true,
        );
        // Flip a byte in the payload so the CRC no longer matches.
        let len = bytes.len();
        bytes[len - 4] ^= 0xFF;
        let dg = parse_msc_data_group(&bytes).expect("parse");
        assert!(!dg.crc_ok);
    }

    #[test]
    fn parse_msc_dg_rejects_oversized_segment_size() {
        // Build with_crc=false to make byte slicing easier, then hand-edit
        // the size header to claim more bytes than actually present.
        let mut bytes = build_msc_dg(
            DG_TYPE_MOT_BODY,
            0,
            0,
            Some((0, true)),
            Some(0x1234),
            b"abcd",
            false,
        );
        // seg header is at position: 2 + 2 (segment) + 3 (user access) = 7
        let seg_hdr_pos = 7;
        bytes[seg_hdr_pos] = 0x1F;
        bytes[seg_hdr_pos + 1] = 0xFF;
        assert!(parse_msc_data_group(&bytes).is_none());
    }

    #[test]
    fn parse_mot_header_extracts_content_name_and_mime() {
        // ContentName PLI=11 len=7: [0x0C | flag_byte_first | len=7 | charset=0F | "hi.png"]
        let content_name = {
            let name = b"hi.png";
            let mut p = Vec::new();
            p.push((0b11 << 6) | PARAM_CONTENT_NAME); // PLI=11, id=0x0C
            p.push((1 + name.len()) as u8); // PLI=11 short form: high bit=0, len in 6..0
            p.push(0x0F << 4); // charset=UTF-8, rfa=0
            p.extend_from_slice(name);
            p
        };
        let mime = {
            let m = b"image/png";
            let mut p = Vec::new();
            p.push((0b11 << 6) | PARAM_MIME_TYPE);
            p.push(m.len() as u8);
            p.extend_from_slice(m);
            p
        };
        let mut params = Vec::new();
        params.extend_from_slice(&content_name);
        params.extend_from_slice(&mime);

        let bytes = build_mot_header(1024, 2, 3, &params); // image/png
        let hdr = parse_mot_header(&bytes).expect("parse mot header");
        assert_eq!(hdr.body_size, 1024);
        assert_eq!(hdr.content_type, 2);
        assert_eq!(hdr.content_subtype, 3);
        assert_eq!(hdr.content_name.as_deref(), Some("hi.png"));
        assert_eq!(hdr.mime_type.as_deref(), Some("image/png"));
        assert_eq!(hdr.inferred_mime(), Some("image/png"));
        assert_eq!(hdr.preferred_extension(), "png");
    }

    #[test]
    fn parse_mot_header_unknown_params_are_skipped() {
        // Unknown param id 0x3F with PLI=10 (4 bytes), then ContentName.
        let mut params = Vec::new();
        params.push((0b10 << 6) | 0x3F);
        params.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        let name = b"slide.jpg";
        params.push((0b11 << 6) | PARAM_CONTENT_NAME);
        params.push((1 + name.len()) as u8);
        params.push(0x0F << 4);
        params.extend_from_slice(name);
        let bytes = build_mot_header(2048, 2, 1, &params);
        let hdr = parse_mot_header(&bytes).expect("parse");
        assert_eq!(hdr.body_size, 2048);
        assert_eq!(hdr.content_name.as_deref(), Some("slide.jpg"));
        assert_eq!(hdr.preferred_extension(), "jpg");
    }

    #[test]
    fn mot_assembler_single_header_single_body() {
        // Single-segment header + single-segment body, transport id 0x00A1.
        let mut params = Vec::new();
        let name = b"cover.png";
        params.push((0b11 << 6) | PARAM_CONTENT_NAME);
        params.push((1 + name.len()) as u8);
        params.push(0x0F << 4);
        params.extend_from_slice(name);
        let body = vec![0x55u8; 128];
        let header_bytes = build_mot_header(body.len() as u32, 2, 3, &params);

        let mut asm = MotAssembler::new();
        // Header segment 0 (last=true)
        let header_dg = build_msc_dg(
            DG_TYPE_MOT_HEADER,
            0,
            0,
            Some((0, true)),
            Some(0x00A1),
            &header_bytes,
            true,
        );
        assert!(asm.push_msc_data_group(&header_dg).is_none());

        // Body segment 0 (last=true)
        let body_dg = build_msc_dg(
            DG_TYPE_MOT_BODY,
            0,
            0,
            Some((0, true)),
            Some(0x00A1),
            &body,
            true,
        );
        let obj = asm.push_msc_data_group(&body_dg).expect("expected object");
        assert_eq!(obj.transport_id, 0x00A1);
        assert_eq!(obj.body, body);
        assert_eq!(obj.header.content_name.as_deref(), Some("cover.png"));
    }

    #[test]
    fn mot_assembler_multi_segment_body_any_order() {
        let mut params = Vec::new();
        let name = b"slide.jpg";
        params.push((0b11 << 6) | PARAM_CONTENT_NAME);
        params.push((1 + name.len()) as u8);
        params.push(0x0F << 4);
        params.extend_from_slice(name);
        let body_chunks = [vec![0xAAu8; 50], vec![0xBBu8; 60], vec![0xCCu8; 40]];
        let body_len: u32 = body_chunks.iter().map(|c| c.len() as u32).sum();
        let header_bytes = build_mot_header(body_len, 2, 1, &params);

        let mut asm = MotAssembler::new();
        let header_dg = build_msc_dg(
            DG_TYPE_MOT_HEADER,
            0,
            0,
            Some((0, true)),
            Some(0x0042),
            &header_bytes,
            true,
        );
        // Arrive body segments out of order: 2 (last), 0, 1.
        let b2 = build_msc_dg(
            DG_TYPE_MOT_BODY,
            0,
            0,
            Some((2, true)),
            Some(0x0042),
            &body_chunks[2],
            true,
        );
        let b0 = build_msc_dg(
            DG_TYPE_MOT_BODY,
            0,
            0,
            Some((0, false)),
            Some(0x0042),
            &body_chunks[0],
            true,
        );
        let b1 = build_msc_dg(
            DG_TYPE_MOT_BODY,
            0,
            0,
            Some((1, false)),
            Some(0x0042),
            &body_chunks[1],
            true,
        );
        assert!(asm.push_msc_data_group(&header_dg).is_none());
        assert!(asm.push_msc_data_group(&b2).is_none());
        assert!(asm.push_msc_data_group(&b0).is_none());
        let obj = asm
            .push_msc_data_group(&b1)
            .expect("complete on last segment");
        assert_eq!(obj.body.len(), body_len as usize);
        let mut expected = Vec::new();
        expected.extend_from_slice(&body_chunks[0]);
        expected.extend_from_slice(&body_chunks[1]);
        expected.extend_from_slice(&body_chunks[2]);
        assert_eq!(obj.body, expected);
        assert_eq!(obj.header.content_name.as_deref(), Some("slide.jpg"));
    }

    #[test]
    fn mot_assembler_rejects_oversized_body() {
        // Claim a body_size larger than MAX_BODY_SIZE; assembler should
        // drop the transport without producing an object.
        let header_bytes = build_mot_header(MAX_BODY_SIZE + 1, 2, 3, &[]);
        let mut asm = MotAssembler::new();
        let header_dg = build_msc_dg(
            DG_TYPE_MOT_HEADER,
            0,
            0,
            Some((0, true)),
            Some(0x0500),
            &header_bytes,
            true,
        );
        assert!(asm.push_msc_data_group(&header_dg).is_none());
        // Even if a body segment arrives later, no object is produced.
        let body = vec![0u8; 32];
        let body_dg = build_msc_dg(
            DG_TYPE_MOT_BODY,
            0,
            0,
            Some((0, true)),
            Some(0x0500),
            &body,
            true,
        );
        assert!(asm.push_msc_data_group(&body_dg).is_none());
    }

    #[test]
    fn mot_assembler_ignores_crc_failure() {
        // Break the CRC on the header DG — the assembler should skip it and
        // not emit a partial object when the body arrives.
        let header_bytes = build_mot_header(32, 2, 3, &[]);
        let mut asm = MotAssembler::new();
        let mut header_dg = build_msc_dg(
            DG_TYPE_MOT_HEADER,
            0,
            0,
            Some((0, true)),
            Some(0x0600),
            &header_bytes,
            true,
        );
        let idx = header_dg.len() - 4;
        header_dg[idx] ^= 0xFF;
        assert!(asm.push_msc_data_group(&header_dg).is_none());
        let body = vec![0u8; 32];
        let body_dg = build_msc_dg(
            DG_TYPE_MOT_BODY,
            0,
            0,
            Some((0, true)),
            Some(0x0600),
            &body,
            true,
        );
        // With no valid header, we can't emit.
        assert!(asm.push_msc_data_group(&body_dg).is_none());
    }

    #[test]
    fn mot_assembler_re_emits_on_carousel_repeat() {
        // A typical slideshow carousel cycles back to the same transport_id.
        // Assembler must emit on every complete assembly so the caller can
        // dedup by body hash rather than silently hiding repeated objects.
        let mut params = Vec::new();
        let name = b"slide.png";
        params.push((0b11 << 6) | PARAM_CONTENT_NAME);
        params.push((1 + name.len()) as u8);
        params.push(0x0F << 4);
        params.extend_from_slice(name);
        let body = vec![0x77u8; 64];
        let header_bytes = build_mot_header(body.len() as u32, 2, 3, &params);
        let header_dg = build_msc_dg(
            DG_TYPE_MOT_HEADER,
            0,
            0,
            Some((0, true)),
            Some(0x0101),
            &header_bytes,
            true,
        );
        let body_dg = build_msc_dg(
            DG_TYPE_MOT_BODY,
            0,
            0,
            Some((0, true)),
            Some(0x0101),
            &body,
            true,
        );

        let mut asm = MotAssembler::new();
        assert!(asm.push_msc_data_group(&header_dg).is_none());
        asm.push_msc_data_group(&body_dg).expect("first emission");
        // Cycle repeats — must emit again.
        assert!(asm.push_msc_data_group(&header_dg).is_none());
        asm.push_msc_data_group(&body_dg)
            .expect("second carousel cycle must re-emit");
    }
}
