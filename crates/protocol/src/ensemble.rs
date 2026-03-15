/// DAB ensemble and service description types.

#[derive(Debug, Clone, Default)]
pub struct Ensemble {
    /// 16-bit Ensemble Identifier (EId).
    pub id: u16,
    /// Human-readable ensemble label (up to 16 chars).
    pub label: String,
    /// Country identifier (upper 4 bits of EId).
    pub country_id: u8,
    /// Services carried in this ensemble.
    pub services: Vec<Service>,
    /// Tuner centre frequency in Hz (0 = unknown).
    pub freq_hz: u32,
}

impl Ensemble {
    /// Find a service by SId, returning a mutable reference.
    pub fn service_mut(&mut self, sid: u32) -> Option<&mut Service> {
        self.services.iter_mut().find(|s| s.id == sid)
    }

    /// Find or insert a service with the given SId.
    pub fn get_or_insert_service(&mut self, sid: u32) -> &mut Service {
        if let Some(pos) = self.services.iter().position(|s| s.id == sid) {
            return &mut self.services[pos];
        }
        self.services.push(Service {
            id: sid,
            ..Default::default()
        });
        self.services.last_mut().unwrap()
    }
}

#[derive(Debug, Clone, Default)]
pub struct Service {
    /// Service Identifier.  16-bit for DAB audio, 32-bit for DAB+ / data.
    pub id: u32,
    /// Human-readable service label (up to 16 chars).
    pub label: String,
    /// `true` when the primary audio component uses HE-AAC (DAB+).
    pub is_dab_plus: bool,
    /// Service components (audio/data subchannels).
    pub components: Vec<Component>,
    /// Dynamic Label Segment text (from MSC data packets), if received.
    pub dls_text: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Component {
    /// Subchannel number (0–63).
    pub subchannel_id: u8,
    pub service_type: ServiceType,
    /// Start address in Capacity Units within the MSC.
    pub start_address: u16,
    /// Size of the subchannel in Capacity Units (1 CU = 64 bits).
    pub size: u16,
    pub protection: ProtectionLevel,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub enum ServiceType {
    #[default]
    Audio,
    /// HE-AAC audio (DAB+, ASCTy = 0x3F).
    DabPlus,
    Data,
}

#[derive(Debug, Clone)]
pub enum ProtectionLevel {
    /// Unequal Error Protection (levels 1–5).
    Uep(u8),
    /// Equal Error Protection profile A (levels 1–4).
    EepA(u8),
    /// Equal Error Protection profile B (levels 1–4).
    EepB(u8),
}

impl Default for ProtectionLevel {
    fn default() -> Self {
        ProtectionLevel::EepA(2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_or_insert_creates_service() {
        let mut ens = Ensemble::default();
        let svc = ens.get_or_insert_service(0x1234);
        svc.label = "Test".into();
        assert_eq!(ens.services.len(), 1);
        assert_eq!(ens.services[0].label, "Test");
    }

    #[test]
    fn get_or_insert_idempotent() {
        let mut ens = Ensemble::default();
        ens.get_or_insert_service(0xABCD).label = "Radio".into();
        ens.get_or_insert_service(0xABCD); // second call must not duplicate
        assert_eq!(ens.services.len(), 1);
    }
}
