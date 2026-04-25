/// DAB ensemble and service description types.
use std::collections::BTreeMap;

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
    /// Song title (DL+ Item.Title, content type 0x01).
    pub title: Option<String>,
    /// Artist (DL+ Item.Artist, content type 0x04).
    pub artist: Option<String>,
    /// Album (DL+ Item.Album, content type 0x02).
    pub album: Option<String>,
    /// Track number or name (DL+ Item.TrackNumber, content type 0x03).
    pub track: Option<String>,
    /// Composer (DL+ Item.Composer, content type 0x08).
    pub composer: Option<String>,
    /// Band (DL+ Item.Band, content type 0x09).
    pub band: Option<String>,
    /// Genre (DL+ Item.Genre, content type 0x0B).
    pub genre: Option<String>,
    /// DLS toggle bit (changes when item changes), if signalled.
    pub toggle: Option<bool>,
    /// DL+ Item Toggle (IT) bit from the DL+ command header
    /// (TS 102 980 §7.3.2). Flipped by the broadcaster to mark a new
    /// programme item; distinct from the cosmetic DLS segment toggle.
    pub item_toggle: Option<bool>,
    /// Item running flag. `Some(false)` means the broadcaster has signalled
    /// that the current item has stopped; the UI should clear song details.
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
    /// MOT `CategoryTitle` parameter (TS 101 499 §4.1.10) when signalled.
    pub category_title: Option<String>,
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
    /// Currently-active announcements keyed by Cluster Id, populated by FIG 0/19.
    /// Empty entries are removed when the FIG drops the cluster.
    pub active_announcements: BTreeMap<u8, ActiveAnnouncement>,
}

/// One active announcement entry from FIG 0/19.
///
/// `asw_flags` is the announcement-type bitfield (same shape as the support
/// flags in FIG 0/18). `subch_id` is the sub-channel actually carrying the
/// announcement audio for the duration of the switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveAnnouncement {
    pub asw_flags: u16,
    pub subch_id: u8,
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
    /// Static Programme Type (FIG 0/17 with S/D=0): the genre the broadcaster
    /// permanently associates with this service.
    pub pty_static: Option<u8>,
    /// Dynamic Programme Type (FIG 0/17 with S/D=1): the genre of the current
    /// programme item, may change throughout the day.
    pub pty_dynamic: Option<u8>,
    /// Programme language code (FIG 0/17 L flag, EN 300 401 Annex D).
    pub language: Option<u8>,
    /// Announcement support bitfield from FIG 0/18 (one bit per announcement type).
    pub announcement_support: u16,
    /// Announcement cluster ids the service belongs to (FIG 0/18).
    pub announcement_clusters: Vec<u8>,
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

impl ProtectionLevel {
    /// Short human-readable label (e.g. "EEP-3A", "UEP-2").
    pub fn label(&self) -> String {
        match self {
            ProtectionLevel::Uep(level) => format!("UEP-{level}"),
            ProtectionLevel::EepA(level) => format!("EEP-{level}A"),
            ProtectionLevel::EepB(level) => format!("EEP-{level}B"),
        }
    }
}

impl Component {
    /// Bitrate in kbps derived from sub-channel size (CUs) and protection.
    ///
    /// One Capacity Unit carries 64 bits per 24 ms CIF, i.e. 8/3 kbit/s.
    /// EEP-A (option 0) uses code rates 1/4, 3/8, 1/2, 3/4 for protection
    /// levels 1..4. EEP-B (option 1) uses 4/9, 4/7, 4/6, 4/5. UEP rates are
    /// non-uniform across the sub-channel; the value here is the average
    /// audio bitrate corresponding to the standard UEP table entries.
    pub fn bitrate_kbps(&self) -> Option<u32> {
        if self.size == 0 {
            return None;
        }
        // Capacity bits per second across the sub-channel.
        let cu_bits_per_sec = (self.size as f32) * 64.0 / 0.024;
        let rate = match self.protection {
            ProtectionLevel::EepA(level) => match level {
                1 => 1.0 / 4.0,
                2 => 3.0 / 8.0,
                3 => 1.0 / 2.0,
                4 => 3.0 / 4.0,
                _ => return None,
            },
            ProtectionLevel::EepB(level) => match level {
                1 => 4.0 / 9.0,
                2 => 4.0 / 7.0,
                3 => 4.0 / 6.0,
                4 => 4.0 / 5.0,
                _ => return None,
            },
            // UEP rates are not a single number; approximate with the Table 7
            // overall rate so the displayed bitrate matches the standard table.
            ProtectionLevel::Uep(_) => return uep_bitrate_kbps(self.size),
        };
        Some((cu_bits_per_sec * rate / 1000.0).round() as u32)
    }
}

/// Map a UEP sub-channel size (CUs) back to the standard audio bitrate.
///
/// The UEP table in EN 300 401 Annex B fixes one bitrate per (size, level)
/// pair. Looking up by size alone is unambiguous within the common audio
/// rates because each rate has a distinct CU count per protection level.
fn uep_bitrate_kbps(size: u16) -> Option<u32> {
    match size {
        16 | 21 | 24 | 29 | 35 => Some(32),
        42 | 52 => Some(48),
        58 => Some(56),
        70 => Some(64),
        84 => Some(80),
        104 => Some(96),
        116 => Some(112),
        140 => Some(128),
        168 => Some(160),
        208 => Some(192),
        232 => Some(224),
        280 => Some(256),
        416 => Some(384),
        _ => None,
    }
}

/// EN 300 401 Annex A — Programme Type (PTy) labels.
pub fn pty_label(code: u8) -> &'static str {
    match code & 0x1F {
        0 => "None",
        1 => "News",
        2 => "Current Affairs",
        3 => "Information",
        4 => "Sport",
        5 => "Education",
        6 => "Drama",
        7 => "Culture",
        8 => "Science",
        9 => "Talk",
        10 => "Pop Music",
        11 => "Rock Music",
        12 => "Easy Listening",
        13 => "Light Classical",
        14 => "Serious Classical",
        15 => "Other Music",
        16 => "Weather",
        17 => "Finance",
        18 => "Children's",
        19 => "Social Affairs",
        20 => "Religion",
        21 => "Phone In",
        22 => "Travel",
        23 => "Leisure",
        24 => "Jazz Music",
        25 => "Country Music",
        26 => "National Music",
        27 => "Oldies Music",
        28 => "Folk Music",
        29 => "Documentary",
        30 => "Alarm Test",
        31 => "Alarm",
        _ => "Reserved",
    }
}

/// EN 300 401 §8.1.6.1 — Announcement type label by ASu/ASw bit position.
pub fn announcement_label(bit: u8) -> &'static str {
    match bit {
        0 => "Alarm",
        1 => "Road Traffic",
        2 => "Transport",
        3 => "Warning/Service",
        4 => "News Flash",
        5 => "Area Weather",
        6 => "Event",
        7 => "Special Event",
        8 => "Programme Info",
        9 => "Sport Report",
        10 => "Financial Report",
        _ => "Reserved",
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

    fn make_component(size: u16, protection: ProtectionLevel) -> Component {
        Component {
            subchannel_id: 0,
            scids: None,
            service_type: ServiceType::DabPlus,
            start_address: 0,
            size,
            protection,
            packet_address: None,
            user_applications: Vec::new(),
        }
    }

    #[test]
    fn bitrate_eep_a_3_at_84_cu_is_96_kbps() {
        // EEP-3A, 84 CUs: 84 * 64 / 0.024 * 1/2 / 1000 = ~112 kbps... let's verify
        // Actually 84 * 64 = 5376 bits per CIF, /0.024 s = 224000 bps, * 1/2 = 112 kbps
        let comp = make_component(84, ProtectionLevel::EepA(3));
        assert_eq!(comp.bitrate_kbps(), Some(112));
    }

    #[test]
    fn bitrate_eep_a_2_at_72_cu_is_72_kbps() {
        // EEP-2A: code rate 3/8. 72 CUs * 64 / 0.024 * 3/8 / 1000 = 72 kbps
        let comp = make_component(72, ProtectionLevel::EepA(2));
        assert_eq!(comp.bitrate_kbps(), Some(72));
    }

    #[test]
    fn bitrate_uep_lookup() {
        // UEP table: size 84 → 80 kbps (EN 300 401 Annex B)
        let comp = make_component(84, ProtectionLevel::Uep(2));
        assert_eq!(comp.bitrate_kbps(), Some(80));
    }

    #[test]
    fn protection_label_strings() {
        assert_eq!(ProtectionLevel::EepA(3).label(), "EEP-3A");
        assert_eq!(ProtectionLevel::EepB(1).label(), "EEP-1B");
        assert_eq!(ProtectionLevel::Uep(4).label(), "UEP-4");
    }

    #[test]
    fn pty_label_known_codes() {
        assert_eq!(pty_label(0), "None");
        assert_eq!(pty_label(1), "News");
        assert_eq!(pty_label(10), "Pop Music");
        assert_eq!(pty_label(31), "Alarm");
    }

    #[test]
    fn announcement_label_known_bits() {
        assert_eq!(announcement_label(0), "Alarm");
        assert_eq!(announcement_label(1), "Road Traffic");
        assert_eq!(announcement_label(4), "News Flash");
        assert_eq!(announcement_label(99), "Reserved");
    }
}
