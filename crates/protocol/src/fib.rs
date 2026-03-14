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
use std::collections::HashMap;

use crate::ensemble::Ensemble;

/// Subchannel parameters from FIG 0/1, stored independently from components.
#[derive(Debug, Clone)]
struct SubchannelInfo {
    start_address: u16,
    size: u16,
    protection: crate::ensemble::ProtectionLevel,
}

/// UEP sub-channel size (CU) and protection level per table index.
///
/// ETSI EN 300 401 Table 8.  Index 0–63.
/// Each entry is `(sub_channel_size_CU, uep_protection_level)`.
/// Protection levels: 1 = strongest … 5 = weakest.
#[rustfmt::skip]
const UEP_TABLE: [(u16, u8); 64] = [
    // idx  size  lvl     bitrate  prot
    ( 16, 5), // 0    32 kbit/s  level 5
    ( 21, 4), // 1    32         level 4
    ( 24, 3), // 2    32         level 3
    ( 29, 2), // 3    32         level 2
    ( 35, 1), // 4    32         level 1
    ( 24, 5), // 5    48         level 5
    ( 29, 4), // 6    48         level 4
    ( 35, 3), // 7    48         level 3
    ( 42, 2), // 8    48         level 2
    ( 52, 1), // 9    48         level 1
    ( 29, 5), // 10   56         level 5
    ( 35, 4), // 11   56         level 4
    ( 42, 3), // 12   56         level 3
    ( 52, 2), // 13   56         level 2
    ( 32, 5), // 14   64         level 5
    ( 42, 4), // 15   64         level 4
    ( 48, 3), // 16   64         level 3
    ( 58, 2), // 17   64         level 2
    ( 70, 1), // 18   64         level 1
    ( 40, 5), // 19   80         level 5
    ( 52, 4), // 20   80         level 4
    ( 58, 3), // 21   80         level 3
    ( 70, 2), // 22   80         level 2
    ( 84, 1), // 23   80         level 1
    ( 48, 5), // 24   96         level 5
    ( 58, 4), // 25   96         level 4
    ( 70, 3), // 26   96         level 3
    ( 84, 2), // 27   96         level 2
    (104, 1), // 28   96         level 1
    ( 58, 5), // 29  112         level 5
    ( 70, 4), // 30  112         level 4
    ( 84, 3), // 31  112         level 3
    (104, 2), // 32  112         level 2
    ( 64, 5), // 33  128         level 5
    ( 84, 4), // 34  128         level 4
    ( 96, 3), // 35  128         level 3
    (116, 2), // 36  128         level 2
    (140, 1), // 37  128         level 1
    ( 80, 5), // 38  160         level 5
    (104, 4), // 39  160         level 4
    (116, 3), // 40  160         level 3
    (140, 2), // 41  160         level 2
    (168, 1), // 42  160         level 1
    ( 96, 5), // 43  192         level 5
    (116, 4), // 44  192         level 4
    (140, 3), // 45  192         level 3
    (168, 2), // 46  192         level 2
    (208, 1), // 47  192         level 1
    (116, 5), // 48  224         level 5
    (140, 4), // 49  224         level 4
    (168, 3), // 50  224         level 3
    (208, 2), // 51  224         level 2
    (232, 1), // 52  224         level 1
    (128, 5), // 53  256         level 5
    (168, 4), // 54  256         level 4
    (192, 3), // 55  256         level 3
    (232, 2), // 56  256         level 2
    (280, 1), // 57  256         level 1
    (160, 5), // 58  320         level 5
    (208, 4), // 59  320         level 4
    (280, 2), // 60  320         level 2
    (192, 5), // 61  384         level 5
    (280, 3), // 62  384         level 3
    (416, 1), // 63  384         level 1
];

pub struct FibParser {
    pub ensemble: Ensemble,
    /// Subchannel parameters indexed by SubChId (0–63), populated by FIG 0/1.
    /// Stored separately so they are available regardless of FIG arrival order.
    subchannels: HashMap<u8, SubchannelInfo>,
}

impl FibParser {
    pub fn new() -> Self {
        FibParser {
            ensemble: Ensemble::default(),
            subchannels: HashMap::new(),
        }
    }

    /// Parse one FIB (first 30 bytes = FIG content; last 2 bytes = CRC-16).
    /// Returns `true` if the CRC was valid (or if the FIB was too short to check).
    pub fn parse_fib(&mut self, fib: &[u8]) -> bool {
        // Verify CRC-16/CCITT before parsing.
        if fib.len() >= 32 && !fib_crc_valid(&fib[..32]) {
            return false;
        }
        // Use only the 30 FIG-content bytes.
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
            1 => self.parse_fig_0_1(payload),
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

    /// FIG 0/1 — Subchannel organisation (ETSI EN 300 401 §6.2.1).
    ///
    /// Provides start address, size, and protection level for each subchannel.
    ///
    /// Short form (S/L=0): 3 bytes per entry → UEP via table index.
    /// Long form  (S/L=1): 4 bytes per entry → EEP with explicit size & level.
    fn parse_fig_0_1(&mut self, data: &[u8]) {
        let mut i = 0usize;

        while i + 3 <= data.len() {
            let sub_ch_id = (data[i] >> 2) & 0x3F;
            let start_address = ((data[i] as u16 & 0x03) << 8) | data[i + 1] as u16;
            let long_form = (data[i + 2] >> 7) & 1 == 1;

            if long_form {
                // Long form: 4 bytes total
                if i + 4 > data.len() {
                    break;
                }
                let option = (data[i + 2] >> 4) & 0x07;
                let prot_level = ((data[i + 2] >> 2) & 0x03) + 1; // 1-4
                let sub_ch_size = ((data[i + 2] as u16 & 0x03) << 8) | data[i + 3] as u16;

                let protection = match option {
                    0 => crate::ensemble::ProtectionLevel::EepA(prot_level),
                    _ => crate::ensemble::ProtectionLevel::EepB(prot_level),
                };

                self.update_subchannel(sub_ch_id, start_address, sub_ch_size, protection);
                i += 4;
            } else {
                // Short form: 3 bytes total → UEP table lookup
                let table_index = (data[i + 2] & 0x3F) as usize;

                if let Some(&(size, level)) = UEP_TABLE.get(table_index) {
                    let protection = crate::ensemble::ProtectionLevel::Uep(level);
                    self.update_subchannel(sub_ch_id, start_address, size, protection);
                }
                i += 3;
            }

            log::debug!(
                "FIG 0/1: SubChId={} StartAddr={} long={}",
                sub_ch_id,
                start_address,
                long_form
            );
        }
    }

    /// Store subchannel parameters and apply to any existing components.
    ///
    /// FIG 0/1 may arrive before or after FIG 0/2 creates the component.
    /// We store the info in `self.subchannels` so it survives either ordering.
    fn update_subchannel(
        &mut self,
        sub_ch_id: u8,
        start_address: u16,
        size: u16,
        protection: crate::ensemble::ProtectionLevel,
    ) {
        // Store for future components (FIG 0/2 arriving later).
        self.subchannels.insert(
            sub_ch_id,
            SubchannelInfo {
                start_address,
                size,
                protection: protection.clone(),
            },
        );

        // Apply to any components that already exist.
        for svc in &mut self.ensemble.services {
            for comp in &mut svc.components {
                if comp.subchannel_id == sub_ch_id {
                    comp.start_address = start_address;
                    comp.size = size;
                    comp.protection = protection.clone();
                }
            }
        }
    }

    /// FIG 0/2 — Basic service and service component.
    ///
    /// Layout per service entry:
    ///   [SId: 16 bits (P/D=0) or 32 bits (P/D=1)]
    ///   [Local_flag:1 | CAID:3 | Num_SC:4]
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

        while i + 3 <= data.len() {
            // Short SId (16-bit).
            let sid = u16::from_be_bytes([data[i], data[i + 1]]) as u32;
            i += 2;

            let num_sc = data[i] & 0x0F;
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
                    // Apply subchannel info if FIG 0/1 already provided it.
                    let (start_address, size, protection) =
                        if let Some(info) = self.subchannels.get(&sub_ch_id) {
                            (info.start_address, info.size, info.protection.clone())
                        } else {
                            (0, 0, Default::default())
                        };
                    svc.components.push(crate::ensemble::Component {
                        subchannel_id: sub_ch_id,
                        service_type,
                        start_address,
                        size,
                        protection,
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

/// CRC-16/CCITT check for a 32-byte FIB.
///
/// Polynomial: x^16 + x^12 + x^5 + 1 (0x1021), init 0xFFFF.
/// The transmitted CRC is the bitwise complement of the CRC of the first 30
/// data bytes, stored big-endian in bytes 30–31.
fn fib_crc_valid(fib: &[u8]) -> bool {
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
        // Compute and append CRC-16/CCITT so the FIB passes validation.
        let mut crc: u16 = 0xFFFF;
        for &b in &fib[..30] {
            crc ^= (b as u16) << 8;
            for _ in 0..8 {
                if crc & 0x8000 != 0 {
                    crc = (crc << 1) ^ 0x1021;
                } else {
                    crc <<= 1;
                }
            }
        }
        let crc = !crc;
        fib[30] = (crc >> 8) as u8;
        fib[31] = crc as u8;
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

    #[test]
    fn crc_valid_on_correct_fib() {
        // Construct a FIB with correct CRC.
        let mut fib = vec![0xFFu8; 32]; // all end-markers → valid content
                                        // Compute CRC of first 30 bytes.
        let mut crc: u16 = 0xFFFF;
        for &b in &fib[..30] {
            crc ^= (b as u16) << 8;
            for _ in 0..8 {
                if crc & 0x8000 != 0 {
                    crc = (crc << 1) ^ 0x1021;
                } else {
                    crc <<= 1;
                }
            }
        }
        let crc = !crc; // invert per DAB spec
        fib[30] = (crc >> 8) as u8;
        fib[31] = crc as u8;
        assert!(super::fib_crc_valid(&fib));
    }

    #[test]
    fn crc_rejects_corrupted_fib() {
        let mut fib = vec![0u8; 32];
        fib[30] = 0xDE;
        fib[31] = 0xAD;
        assert!(!super::fib_crc_valid(&fib));
    }

    #[test]
    fn parse_fig_0_1_long_form_eep() {
        let mut parser = FibParser::new();
        // First create a service with a component on SubChId=5
        let svc = parser.ensemble.get_or_insert_service(0x1234);
        svc.components.push(crate::ensemble::Component {
            subchannel_id: 5,
            service_type: crate::ensemble::ServiceType::Audio,
            start_address: 0,
            size: 0,
            protection: Default::default(),
        });

        // FIG 0/1 long form: SubChId=5, StartAddr=100, Option=0 (EEP-A),
        // ProtLevel=1 (encoded as 0), SubChSize=84
        // Byte 0: SubChId(5) << 2 | StartAddr_high(0) = 0x14
        // Byte 1: StartAddr_low(100) = 0x64
        // Byte 2: LongForm(1) | Option(0) | ProtLevel(0) | Size_high(0) = 0x80
        // Byte 3: Size_low(84) = 0x54
        let payload = [0x14, 0x64, 0x80, 0x54];
        parser.parse_fig_0_1(&payload);

        let comp = &parser.ensemble.services[0].components[0];
        assert_eq!(comp.start_address, 100);
        assert_eq!(comp.size, 84);
        assert!(matches!(
            comp.protection,
            crate::ensemble::ProtectionLevel::EepA(1)
        ));
    }

    #[test]
    fn parse_fig_0_1_short_form_uep() {
        let mut parser = FibParser::new();
        let svc = parser.ensemble.get_or_insert_service(0x5678);
        svc.components.push(crate::ensemble::Component {
            subchannel_id: 10,
            service_type: crate::ensemble::ServiceType::Audio,
            start_address: 0,
            size: 0,
            protection: Default::default(),
        });

        // Short form: SubChId=10, StartAddr=50, TableIndex=15 (64kbit/s lvl4, 42 CU)
        // Byte 0: SubChId(10) << 2 | StartAddr_high(0) = 0x28
        // Byte 1: StartAddr_low(50) = 0x32
        // Byte 2: LongForm(0) | TableSwitch(0) | TableIndex(15) = 0x0F
        let payload = [0x28, 0x32, 0x0F];
        parser.parse_fig_0_1(&payload);

        let comp = &parser.ensemble.services[0].components[0];
        assert_eq!(comp.start_address, 50);
        assert_eq!(comp.size, 42); // UEP_TABLE[15]
        assert!(matches!(
            comp.protection,
            crate::ensemble::ProtectionLevel::Uep(4)
        ));
    }

    #[test]
    fn fig_0_1_before_fig_0_2_applies_subchannel_info() {
        let mut parser = FibParser::new();

        // FIG 0/1 arrives first: SubChId=5, StartAddr=100, EEP-A level 1, size=84
        let fig_0_1_payload = [0x14, 0x64, 0x80, 0x54];
        parser.parse_fig_0_1(&fig_0_1_payload);

        // No components yet — but subchannel info is stored.
        assert!(parser.ensemble.services.is_empty());

        // FIG 0/2 arrives later: SId=0x1234, 1 component, TMId=0 ASCTy=0, SubChId=5
        let fig_0_2_payload = [0x12, 0x34, 0x01, 0x00, 0x14];
        parser.parse_fig_0_2(&fig_0_2_payload);

        // Component should have the subchannel info from FIG 0/1.
        let comp = &parser.ensemble.services[0].components[0];
        assert_eq!(comp.subchannel_id, 5);
        assert_eq!(comp.start_address, 100);
        assert_eq!(comp.size, 84);
        assert!(matches!(
            comp.protection,
            crate::ensemble::ProtectionLevel::EepA(1)
        ));
    }

    #[test]
    fn fig_0_2_last_service_not_skipped() {
        let mut parser = FibParser::new();

        // Two services back to back, each with 1 component (5 bytes each = 10 total).
        // Svc 1: SId=0xAAAA, num_sc=1, TMId=0 ASCTy=0x00, SubChId=1
        // Svc 2: SId=0xBBBB, num_sc=1, TMId=0 ASCTy=0x3F (DAB+), SubChId=2
        let payload = [
            0xAA, 0xAA, 0x01, 0x00, 0x04, // svc 1: SubChId=1
            0xBB, 0xBB, 0x01, 0x3F, 0x08, // svc 2: SubChId=2
        ];
        parser.parse_fig_0_2(&payload);

        assert_eq!(parser.ensemble.services.len(), 2);
        assert_eq!(parser.ensemble.services[0].id, 0xAAAA);
        assert_eq!(parser.ensemble.services[0].components.len(), 1);
        assert_eq!(parser.ensemble.services[1].id, 0xBBBB);
        assert_eq!(parser.ensemble.services[1].components.len(), 1);
        assert!(parser.ensemble.services[1].is_dab_plus);
    }
}
