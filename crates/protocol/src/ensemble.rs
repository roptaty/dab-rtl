/// DAB ensemble and service description types.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataSource {
    /// Metadata extracted from audio-associated X-PAD.
    XPad,
    /// Metadata extracted from packet-mode DLS components.
    Packet,
    /// Metadata extracted from slideshow / cover-art transport.
    Slideshow,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NowPlaying {
    /// Full unstructured text as carried by DLS (fallback for display).
    pub raw_text: String,
    /// Song title extracted from DL+ tags when available.
    pub title: Option<String>,
    /// Artist extracted from DL+ tags when available.
    pub artist: Option<String>,
    /// DLS toggle bit (changes when item changes), if signalled.
    pub toggle: Option<bool>,
    /// Item running flag, if signalled by the broadcaster.
    pub item_running: Option<bool>,
    /// Origin transport for this metadata update.
    pub source: Option<MetadataSource>,
    /// Receiver timestamp in Unix milliseconds.
    pub updated_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContentItem {
    /// MIME-like content type derived from the payload or signalling.
    pub content_type: String,
    /// Best available filename for saving the content.
    pub filename: String,
    /// Raw payload bytes.
    pub bytes: Vec<u8>,
    /// Receiver timestamp in Unix milliseconds.
    pub updated_at_unix_ms: u64,
}

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
    /// Structured now-playing metadata (from X-PAD and/or packet DLS).
    pub now_playing: Option<NowPlaying>,
    /// Downloadable slideshow / cover-art objects associated with this service.
    pub content_items: Vec<ContentItem>,
    /// Unique MOT content types observed for this service in the current session.
    pub mot_content_types: Vec<String>,
}

impl Service {
    pub fn advertised_app_labels(&self) -> Vec<&'static str> {
        let mut labels = Vec::new();
        for comp in &self.components {
            for app in &comp.user_applications {
                let label = match app.uatype {
                    UserApplication::UATYPE_DYNAMIC_LABEL => "DLS",
                    UserApplication::UATYPE_SLIDESHOW => "SlideShow",
                    UserApplication::UATYPE_EPG => "EPG",
                    UserApplication::UATYPE_TPEG => "TPEG",
                    _ => continue,
                };
                if !labels.contains(&label) {
                    labels.push(label);
                }
            }
        }
        labels
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserApplication {
    /// 11-bit User Application Type from FIG 0/13.
    pub uatype: u16,
    /// Raw application data bytes from FIG 0/13.
    pub data: Vec<u8>,
    /// Optional X-PAD application type signalled for X-PAD transports.
    pub xpad_app_type: Option<u8>,
    /// Optional DSCTy / transport indication when present.
    pub dscty: Option<u8>,
    /// Whether MSC data groups are indicated for this application, when signalled.
    pub uses_msc_data_groups: Option<bool>,
    /// Whether CA applies to this application, when signalled.
    pub ca_applies: Option<bool>,
}

impl UserApplication {
    pub const UATYPE_DYNAMIC_LABEL: u16 = 0x002;
    pub const UATYPE_SLIDESHOW: u16 = 0x004;
    pub const UATYPE_EPG: u16 = 0x007;
    pub const UATYPE_TPEG: u16 = 0x00D;

    pub fn is_known_metadata_app(&self) -> bool {
        matches!(
            self.uatype,
            Self::UATYPE_DYNAMIC_LABEL | Self::UATYPE_SLIDESHOW | Self::UATYPE_EPG
        )
    }
}

#[derive(Debug, Clone)]
pub struct Component {
    /// Subchannel number (0–63).
    pub subchannel_id: u8,
    /// Service Component Identifier within the Service when known.
    pub scids: Option<u8>,
    pub service_type: ServiceType,
    /// Start address in Capacity Units within the MSC.
    pub start_address: u16,
    /// Size of the subchannel in Capacity Units (1 CU = 64 bits).
    pub size: u16,
    pub protection: ProtectionLevel,
    /// 10-bit packet address for packet-mode components (FIG 0/3).
    /// `None` for stream-mode (audio) components.
    pub packet_address: Option<u16>,
    /// User applications signalled for this component via FIG 0/13.
    pub user_applications: Vec<UserApplication>,
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
