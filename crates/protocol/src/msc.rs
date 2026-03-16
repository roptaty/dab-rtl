/// MSC (Main Service Channel) handler.
///
/// The MSC carries audio and data subchannels interleaved across CIFs
/// (Common Interleaved Frames).  Each subchannel occupies a contiguous
/// range of Capacity Units (CUs), where 1 CU = 64 bits.
use crate::ensemble::Component;

/// A decoded audio frame ready for the audio decoder.
pub struct AudioFrame {
    /// Which subchannel this belongs to.
    pub subchannel_id: u8,
    /// Raw audio payload bytes (MP2 or AAC superframe).
    pub data: Vec<u8>,
    /// `false` = DAB (MP2), `true` = DAB+ (HE-AAC).
    pub is_dab_plus: bool,
}

pub struct MscHandler {
    target: Option<u8>,
}

impl MscHandler {
    pub fn new() -> Self {
        MscHandler { target: None }
    }

    /// Set which subchannel to extract.
    pub fn set_target(&mut self, subchannel_id: u8) {
        self.target = Some(subchannel_id);
    }

    /// Process one CIF of MSC soft bits.
    ///
    /// A CIF contains 55 296 soft bits (864 CUs × 64 bits).  The component's
    /// subchannel starts at `component.start_address * 64` and spans
    /// `component.size * 64` soft bits.
    ///
    /// Hard decisions are made (positive soft bit → 0, negative → 1) and the
    /// result is packed MSB-first into bytes.
    pub fn process_cif(&self, cif_soft: &[f32], component: &Component) -> Option<AudioFrame> {
        if self.target != Some(component.subchannel_id) {
            return None;
        }

        let start_bit = component.start_address as usize * 64;
        let end_bit = start_bit + component.size as usize * 64;

        if end_bit > cif_soft.len() {
            log::warn!(
                "MSC: subchannel {}: bit range {}..{} exceeds CIF length {}",
                component.subchannel_id,
                start_bit,
                end_bit,
                cif_soft.len()
            );
            return None;
        }

        let subchannel_bits = &cif_soft[start_bit..end_bit];
        let bytes = pack_bits(subchannel_bits);

        Some(AudioFrame {
            subchannel_id: component.subchannel_id,
            data: bytes,
            is_dab_plus: component.service_type == crate::ensemble::ServiceType::DabPlus,
        })
    }
}

impl Default for MscHandler {
    fn default() -> Self {
        Self::new()
    }
}

/// Hard-decide soft bits and pack MSB-first into bytes.
/// Positive soft value → bit 0, negative → bit 1.
fn pack_bits(soft: &[f32]) -> Vec<u8> {
    let n_bytes = soft.len().div_ceil(8);
    let mut out = vec![0u8; n_bytes];
    for (i, &s) in soft.iter().enumerate() {
        if s < 0.0 {
            out[i / 8] |= 0x80 >> (i % 8);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ensemble::{Component, ProtectionLevel, ServiceType};

    fn dummy_component(start: u16, size: u16) -> Component {
        Component {
            subchannel_id: 3,
            service_type: ServiceType::Audio,
            start_address: start,
            size,
            protection: ProtectionLevel::EepA(2),
            packet_address: None,
        }
    }

    #[test]
    fn extracts_correct_bytes() {
        let mut handler = MscHandler::new();
        handler.set_target(3);

        // 2 CUs = 128 bits = 16 bytes.  Fill with all-positive (→ all zeros).
        let mut cif = vec![1.0f32; 64 * 10]; // 10 CUs total
                                             // subchannel at CU 2, size 2: bits 128..256
                                             // Set all bits in that range to negative → should produce 0xFF bytes.
        for v in &mut cif[128..256] {
            *v = -1.0;
        }

        let comp = dummy_component(2, 2);
        let frame = handler.process_cif(&cif, &comp).expect("expected frame");
        assert_eq!(frame.data.len(), 16);
        assert!(frame.data.iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn wrong_target_returns_none() {
        let handler = MscHandler::new(); // no target set
        let cif = vec![1.0f32; 640];
        let comp = dummy_component(0, 1);
        assert!(handler.process_cif(&cif, &comp).is_none());
    }

    #[test]
    fn pack_bits_all_positive() {
        let soft = vec![1.0f32; 8];
        assert_eq!(pack_bits(&soft), vec![0x00]);
    }

    #[test]
    fn pack_bits_all_negative() {
        let soft = vec![-1.0f32; 8];
        assert_eq!(pack_bits(&soft), vec![0xFF]);
    }

    #[test]
    fn pack_bits_alternating() {
        // bits: 1,0,1,0,1,0,1,0 → MSB first → 0b10101010 = 0xAA
        let soft: Vec<f32> = (0..8)
            .map(|i| if i % 2 == 0 { -1.0 } else { 1.0 })
            .collect();
        assert_eq!(pack_bits(&soft), vec![0xAA]);
    }
}
