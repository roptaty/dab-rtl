/// Persistent cache of previously scanned DAB channels and services.
///
/// Results are stored as JSON at `~/.config/dab-rtl/cache.json` and
/// reloaded on the next run so that `tune` and `play` can show services
/// immediately without waiting for a full FIC decode.
use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use protocol::{Ensemble, Service};

// ─────────────────────────────────────────────────────────────────────────── //
//  Cache data model                                                             //
// ─────────────────────────────────────────────────────────────────────────── //

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct CachedService {
    pub id: u32,
    pub label: String,
    pub is_dab_plus: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CachedEnsemble {
    /// Ensemble Identifier (EId).
    pub id: u16,
    /// Human-readable ensemble label.
    pub label: String,
    /// DAB Band III channel name (e.g. "11C").
    pub channel: String,
    /// Unix timestamp (seconds) when this entry was last written.
    pub scanned_at: u64,
    /// Services discovered on this channel.
    pub services: Vec<CachedService>,
}

/// Top-level cache: maps normalised channel name → ensemble snapshot.
#[derive(Serialize, Deserialize, Debug, Default)]
pub struct ChannelCache {
    pub channels: HashMap<String, CachedEnsemble>,
}

// ─────────────────────────────────────────────────────────────────────────── //
//  Load / save                                                                  //
// ─────────────────────────────────────────────────────────────────────────── //

impl ChannelCache {
    /// Returns the path to the cache file.
    ///
    /// Respects `$HOME`; falls back to `.` when that variable is unset.
    pub fn cache_path() -> PathBuf {
        let base = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        base.join(".config").join("dab-rtl").join("cache.json")
    }

    /// Load the cache from disk, returning an empty cache on any error.
    pub fn load() -> Self {
        let path = Self::cache_path();
        let content = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => return Self::default(),
        };
        serde_json::from_str(&content).unwrap_or_default()
    }

    /// Write the cache to disk, creating parent directories as needed.
    pub fn save(&self) -> io::Result<()> {
        let path = Self::cache_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        std::fs::write(&path, json)
    }

    // ─── Accessors ──────────────────────────────────────────────────────── //

    /// Insert or replace the entry for `channel` from a live `Ensemble`.
    pub fn put(&mut self, channel: &str, ensemble: &Ensemble) {
        let entry = CachedEnsemble {
            id: ensemble.id,
            label: ensemble.label.clone(),
            channel: channel.to_uppercase(),
            scanned_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            services: ensemble
                .services
                .iter()
                .map(|s| CachedService {
                    id: s.id,
                    label: s.label.clone(),
                    is_dab_plus: s.is_dab_plus,
                })
                .collect(),
        };
        self.channels.insert(channel.to_uppercase(), entry);
    }

    /// Return the cached ensemble for `channel`, if any.
    pub fn get(&self, channel: &str) -> Option<&CachedEnsemble> {
        self.channels.get(&channel.to_uppercase())
    }

    /// Remove all cached channels.
    pub fn clear(&mut self) {
        self.channels.clear();
    }
}

impl CachedEnsemble {
    /// Convert to an `Ensemble` that the TUI / pipeline can use directly.
    ///
    /// Component details are not cached (only needed for MSC decoding which
    /// requires a live signal), so `components` is left empty.
    pub fn to_ensemble(&self) -> Ensemble {
        Ensemble {
            id: self.id,
            label: self.label.clone(),
            country_id: (self.id >> 12) as u8,
            services: self
                .services
                .iter()
                .map(|s| Service {
                    id: s.id,
                    label: s.label.clone(),
                    is_dab_plus: s.is_dab_plus,
                    components: vec![],
                })
                .collect(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────── //
//  Tests                                                                        //
// ─────────────────────────────────────────────────────────────────────────── //

#[cfg(test)]
mod tests {
    use super::*;

    fn make_ensemble() -> Ensemble {
        use protocol::{Component, ProtectionLevel, ServiceType};
        let mut ens = Ensemble {
            id: 0x1234,
            label: "Test Ensemble".into(),
            country_id: 1,
            services: vec![],
        };
        ens.services.push(Service {
            id: 0xAAAA,
            label: "Radio A".into(),
            is_dab_plus: false,
            components: vec![Component {
                subchannel_id: 0,
                service_type: ServiceType::Audio,
                start_address: 0,
                size: 48,
                protection: ProtectionLevel::EepA(2),
            }],
        });
        ens.services.push(Service {
            id: 0xBBBB,
            label: "Radio B+".into(),
            is_dab_plus: true,
            components: vec![],
        });
        ens
    }

    #[test]
    fn put_and_get_roundtrip() {
        let mut cache = ChannelCache::default();
        let ens = make_ensemble();
        cache.put("11C", &ens);

        let entry = cache.get("11C").expect("entry should be present");
        assert_eq!(entry.label, "Test Ensemble");
        assert_eq!(entry.services.len(), 2);
        assert_eq!(entry.services[0].label, "Radio A");
        assert!(!entry.services[0].is_dab_plus);
        assert!(entry.services[1].is_dab_plus);
    }

    #[test]
    fn get_is_case_insensitive() {
        let mut cache = ChannelCache::default();
        cache.put("11C", &make_ensemble());
        assert!(cache.get("11c").is_some());
        assert!(cache.get("11C").is_some());
    }

    #[test]
    fn clear_removes_all_entries() {
        let mut cache = ChannelCache::default();
        cache.put("11C", &make_ensemble());
        cache.put("12A", &make_ensemble());
        cache.clear();
        assert!(cache.channels.is_empty());
    }

    #[test]
    fn to_ensemble_preserves_service_fields() {
        let mut cache = ChannelCache::default();
        let ens = make_ensemble();
        cache.put("11C", &ens);

        let entry = cache.get("11C").unwrap();
        let restored = entry.to_ensemble();
        assert_eq!(restored.id, 0x1234);
        assert_eq!(restored.label, "Test Ensemble");
        assert_eq!(restored.services.len(), 2);
        assert_eq!(restored.services[1].label, "Radio B+");
        assert!(restored.services[1].is_dab_plus);
    }

    #[test]
    fn json_roundtrip() {
        let mut cache = ChannelCache::default();
        cache.put("11C", &make_ensemble());
        let json = serde_json::to_string(&cache).unwrap();
        let restored: ChannelCache = serde_json::from_str(&json).unwrap();
        assert!(restored.get("11C").is_some());
        assert_eq!(restored.get("11C").unwrap().services.len(), 2);
    }
}
