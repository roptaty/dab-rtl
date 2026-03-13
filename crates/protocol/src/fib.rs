/// FIB (Fast Information Block) parser.
///
/// Each FIB is exactly 32 bytes: up to 30 bytes of concatenated FIGs followed
/// by a 2-byte CRC-CCITT.  We skip CRC verification for now.
///
/// FIG (Fast Information Group) byte layout
/// ─────────────────────────────────────────
/// Byte 0:  [type:3 | length:5]
///   type   = bits 7‥5 (0 = MCI/SI, 1 = label, 7 = end-marker)
///   length = bits 4‥0 (number of following bytes; 0 with type≠7 = empty FIG)
///
/// FIG type 0 (MCI):
///   Byte 1: [C/N:1 | OE:1 | P/D:1 | extension:5]
///   Bytes 2+: extension-specific data
///
/// FIG type 1 (labels):
///   Byte 1: [charset:4 | OE:1 | extension:3]
///   Bytes 2+: extension-specific data
use crate::ensemble::Ensemble;

pub struct FibParser {
    pub ensemble: Ensemble,
}

impl FibParser {
    pub fn new() -> Self {
        FibParser {
            ensemble: Ensemble::default(),
        }
    }

    /// Parse one FIB (first 30 bytes = FIG content; last 2 bytes = CRC, ignored).
    /// Returns `true` (CRC check skipped — always succeeds for now).
    pub fn parse_fib(&mut self, fib: &[u8]) -> bool {
        // Use only the 30 FIG-content bytes; ignore the 2-byte CRC trailer.
        let data = if fib.len() >= 32 { &fib[..30] } else { fib };
        let mut pos = 0usize;

        while pos < data.len() {
            let header = data[pos];
            pos += 1;

            let fig_type = (header >> 5) & 0x07;
            let fig_len = (header & 0x1F) as usize;

            // End-of-FIB marker.
            if fig_type == 7 || (fig_type == 0 && fig_len == 0) {
                break;
            }

            if pos + fig_len > data.len() {
                break; // truncated FIG
            }

            let body = &data[pos..pos + fig_len];
            pos += fig_len;

            if body.is_empty() {
                continue;
            }

            match fig_type {
                0 => self.parse_fig_type0(body),
                1 => self.parse_fig_type1(body),
                _ => {} // other FIG types not yet handled
            }
        }

        true
    }

    // ------------------------------------------------------------------ //
    //  FIG type 0: MCI / SI                                               //
    // ------------------------------------------------------------------ //

    fn parse_fig_type0(&mut self, body: &[u8]) {
        if body.is_empty() {
            return;
        }
        let extension = body[0] & 0x1F;
        let payload = &body[1..];

        match extension {
            0 => self.parse_fig_0_0(payload),
            2 => self.parse_fig_0_2(payload),
            _ => {}
        }
    }

    /// FIG 0/0 — Ensemble information.
    /// Layout: [EId:16 | change_flags:3 | alarm:1 | CIF_count_high:5]
    ///          [CIF_count_low:8]
    fn parse_fig_0_0(&mut self, data: &[u8]) {
        if data.len() < 2 {
            return;
        }
        let eid = u16::from_be_bytes([data[0], data[1]]);
        self.ensemble.id = eid;
        self.ensemble.country_id = (eid >> 12) as u8;
        log::debug!("FIG 0/0: EId={:04X}", eid);
    }

    /// FIG 0/2 — Basic service and service component.
    ///
    /// Layout per service entry:
    ///   [SId: 16 bits (P/D=0) or 32 bits (P/D=1)]
    ///   [Num_SC_in_SId: 4 bits | ... : 4 bits]
    ///   Per component:
    ///     [TMId:2 | ASCTy/DSCTy:6 | SubChId:6 | PS:1 | CA:1]
    ///
    /// ASCTy values (TMId=0 audio):
    ///   0x00 = MPEG Audio (DAB)
    ///   0x3F = MPEG-4 HE-AAC (DAB+)
    fn parse_fig_0_2(&mut self, data: &[u8]) {
        // Retrieve P/D flag from the FIG 0 header byte (already consumed;
        // we don't have it here).  Default to short (16-bit) SId.
        let mut i = 0usize;

        while i + 3 < data.len() {
            // Short SId (16-bit).
            let sid = u16::from_be_bytes([data[i], data[i + 1]]) as u32;
            i += 2;

            let num_sc = (data[i] >> 4) & 0x0F;
            i += 1;

            for _ in 0..num_sc {
                if i + 2 > data.len() {
                    return;
                }
                let tmid = (data[i] >> 6) & 0x03;
                let ascty = data[i] & 0x3F;
                let sub_ch_id = (data[i + 1] >> 2) & 0x3F;
                i += 2;

                let service_type = match (tmid, ascty) {
                    (0, 0x3F) => crate::ensemble::ServiceType::DabPlus,
                    (0, _) => crate::ensemble::ServiceType::Audio,
                    _ => crate::ensemble::ServiceType::Data,
                };

                let svc = self.ensemble.get_or_insert_service(sid);
                // Mark the service as DAB+ if any audio component is HE-AAC.
                if service_type == crate::ensemble::ServiceType::DabPlus {
                    svc.is_dab_plus = true;
                }
                // Avoid duplicating components.
                if !svc.components.iter().any(|c| c.subchannel_id == sub_ch_id) {
                    svc.components.push(crate::ensemble::Component {
                        subchannel_id: sub_ch_id,
                        service_type,
                        start_address: 0,
                        size: 0,
                        protection: Default::default(),
                    });
                }
                log::debug!(
                    "FIG 0/2: SId={:04X} SubChId={} TMId={} ASCTy={:#04x}",
                    sid,
                    sub_ch_id,
                    tmid,
                    ascty
                );
            }
        }
    }

    // ------------------------------------------------------------------ //
    //  FIG type 1: labels                                                 //
    // ------------------------------------------------------------------ //

    fn parse_fig_type1(&mut self, body: &[u8]) {
        if body.is_empty() {
            return;
        }
        let extension = body[0] & 0x07;
        let payload = &body[1..];

        match extension {
            0 => self.parse_fig_1_0(payload),
            1 => self.parse_fig_1_1(payload),
            _ => {}
        }
    }

    /// FIG 1/0 — Ensemble label.
    /// Layout: [EId:16][label: 16 bytes][short_label_flag: 16 bits]
    fn parse_fig_1_0(&mut self, data: &[u8]) {
        if data.len() < 18 {
            return;
        }
        // EId (2 bytes) — cross-check but not required.
        let label_bytes = &data[2..18];
        self.ensemble.label = decode_label(label_bytes);
        log::debug!("FIG 1/0: Ensemble label = {:?}", self.ensemble.label);
    }

    /// FIG 1/1 — Programme service label.
    /// Layout: [SId:16][label: 16 bytes][short_label_flag: 16 bits]
    fn parse_fig_1_1(&mut self, data: &[u8]) {
        if data.len() < 18 {
            return;
        }
        let sid = u16::from_be_bytes([data[0], data[1]]) as u32;
        let label_bytes = &data[2..18];
        let label = decode_label(label_bytes);

        let svc = self.ensemble.get_or_insert_service(sid);
        svc.label = label.clone();
        log::debug!("FIG 1/1: SId={:04X} label={:?}", sid, label);
    }
}

impl Default for FibParser {
    fn default() -> Self {
        Self::new()
    }
}

/// Decode a 16-byte label field (EBU Latin / ASCII, space-padded) to a String.
fn decode_label(bytes: &[u8]) -> String {
    // Treat as Latin-1; strip trailing spaces.
    let s: String = bytes
        .iter()
        .map(|&b| if b < 0x80 { b as char } else { '?' })
        .collect();
    s.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_fib(figs: &[u8]) -> Vec<u8> {
        let mut fib = vec![0u8; 32];
        let len = figs.len().min(30);
        fib[..len].copy_from_slice(&figs[..len]);
        // end marker
        if len < 30 {
            fib[len] = 0xFF; // type=7, length=31 — end marker
        }
        fib
    }

    #[test]
    fn parse_fig_0_0_sets_ensemble_id() {
        let mut parser = FibParser::new();
        // FIG header: type=0, length=4 → 0b000_00100 = 0x04
        // FIG 0 body byte 0: C/N=0, OE=0, P/D=0, extension=0 → 0x00
        // EId: 0x10CE (BBC national)
        let figs = [0x04, 0x00, 0x10, 0xCE, 0x00, 0x00];
        let fib = make_fib(&figs);
        parser.parse_fib(&fib);
        assert_eq!(parser.ensemble.id, 0x10CE);
        assert_eq!(parser.ensemble.country_id, 1); // upper nibble of 0x10CE
    }

    #[test]
    fn parse_fig_1_0_sets_ensemble_label() {
        let mut parser = FibParser::new();
        // FIG header: type=1, length=21 → 0b001_10101 = 0x35
        // FIG 1 body byte 0: charset=0, OE=0, extension=0 → 0x00
        // EId: 2 bytes
        // label: "BBC DAB         " (16 bytes, space-padded)
        let mut figs = vec![0x35u8, 0x00, 0x10, 0xCE];
        figs.extend_from_slice(b"BBC DAB         ");
        figs.extend_from_slice(&[0xFF, 0xFF]); // short label
        let fib = make_fib(&figs);
        parser.parse_fib(&fib);
        assert_eq!(parser.ensemble.label, "BBC DAB");
    }

    #[test]
    fn empty_fib_does_not_panic() {
        let mut parser = FibParser::new();
        parser.parse_fib(&[0u8; 32]);
    }

    #[test]
    fn decode_label_trims_spaces() {
        let bytes = b"Radio 4         ";
        assert_eq!(super::decode_label(bytes), "Radio 4");
    }
}
