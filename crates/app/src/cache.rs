/// Persistent cache of previously scanned DAB ensembles and services.
///
/// The cache is stored as JSON at `$XDG_CONFIG_HOME/dab-rtl/cache.json`
/// (falling back to `$HOME/.config/dab-rtl/cache.json`).
///
/// Keys are upper-cased Band III channel names (e.g. `"11C"`).
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ─────────────────────────────────────────────────────────────────────────── //
//  Public types                                                                //
// ─────────────────────────────────────────────────────────────────────────── //

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedService {
    pub id: u32,
    pub label: String,
    pub is_dab_plus: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedEnsemble {
    pub channel: String,
    pub ensemble_id: u16,
    pub ensemble_label: String,
    pub services: Vec<CachedService>,
}

impl CachedEnsemble {
    /// Convert to a `protocol::Ensemble`.  Components are not cached so the
    /// returned services have empty component lists.
    pub fn to_ensemble(&self) -> protocol::Ensemble {
        protocol::Ensemble {
            id: self.ensemble_id,
            label: self.ensemble_label.clone(),
            country_id: 0,
            services: self
                .services
                .iter()
                .map(|s| protocol::Service {
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
//  Cache                                                                       //
// ─────────────────────────────────────────────────────────────────────────── //

#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheFile {
    #[serde(default)]
    entries: HashMap<String, CachedEnsemble>,
}

pub struct Cache {
    path: PathBuf,
    data: CacheFile,
}

impl Cache {
    /// Load the cache from disk.  Returns an empty cache on any error.
    pub fn load() -> Self {
        let path = cache_path();
        let data = load_file(&path).unwrap_or_default();
        Cache { path, data }
    }

    /// Look up a cached ensemble by channel name (case-insensitive).
    pub fn get(&self, channel: &str) -> Option<&CachedEnsemble> {
        self.data.entries.get(&channel.to_uppercase())
    }

    /// Store an ensemble in the cache and flush to disk immediately.
    pub fn put(&mut self, channel: String, ensemble: CachedEnsemble) {
        self.data.entries.insert(channel.to_uppercase(), ensemble);
        self.flush();
    }

    /// Remove all entries and flush to disk.
    pub fn clear(&mut self) {
        self.data.entries.clear();
        self.flush();
    }

    /// Path of the cache file on disk.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn flush(&self) {
        if let Some(parent) = self.path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                log::warn!("cache: could not create directory {}: {e}", parent.display());
                return;
            }
        }
        match serde_json::to_string_pretty(&self.data) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&self.path, json) {
                    log::warn!("cache: write failed: {e}");
                }
            }
            Err(e) => log::warn!("cache: serialize failed: {e}"),
        }
    }
}

fn load_file(path: &Path) -> Option<CacheFile> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn cache_path() -> PathBuf {
    let config_root = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    config_root.join("dab-rtl").join("cache.json")
}

// ─────────────────────────────────────────────────────────────────────────── //
//  Tests                                                                       //
// ─────────────────────────────────────────────────────────────────────────── //

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_cache(dir: &std::path::Path) -> Cache {
        Cache {
            path: dir.join("cache.json"),
            data: CacheFile::default(),
        }
    }

    fn sample_ensemble(ch: &str) -> CachedEnsemble {
        CachedEnsemble {
            channel: ch.to_string(),
            ensemble_id: 0x1234,
            ensemble_label: "Test Ensemble".into(),
            services: vec![CachedService {
                id: 0xABCD,
                label: "Test Radio".into(),
                is_dab_plus: false,
            }],
        }
    }

    #[test]
    fn put_and_get() {
        let dir = tempfile::tempdir().unwrap();
        let mut cache = make_cache(dir.path());
        cache.put("11C".into(), sample_ensemble("11C"));
        assert!(cache.get("11C").is_some());
        assert_eq!(cache.get("11C").unwrap().ensemble_label, "Test Ensemble");
    }

    #[test]
    fn get_is_case_insensitive() {
        let dir = tempfile::tempdir().unwrap();
        let mut cache = make_cache(dir.path());
        cache.put("11c".into(), sample_ensemble("11C"));
        assert!(cache.get("11C").is_some());
        assert!(cache.get("11c").is_some());
    }

    #[test]
    fn clear_removes_all_entries() {
        let dir = tempfile::tempdir().unwrap();
        let mut cache = make_cache(dir.path());
        cache.put("11C".into(), sample_ensemble("11C"));
        cache.put("12A".into(), sample_ensemble("12A"));
        cache.clear();
        assert!(cache.get("11C").is_none());
        assert!(cache.get("12A").is_none());
    }

    #[test]
    fn round_trip_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.json");

        let mut cache = Cache {
            path: path.clone(),
            data: CacheFile::default(),
        };
        cache.put("9D".into(), sample_ensemble("9D"));

        // Reload from disk.
        let loaded = Cache {
            path: path.clone(),
            data: load_file(&path).unwrap_or_default(),
        };
        let ens = loaded.get("9D").expect("round-trip failed");
        assert_eq!(ens.services[0].label, "Test Radio");
    }

    #[test]
    fn to_ensemble_converts_correctly() {
        let ce = sample_ensemble("11C");
        let ens = ce.to_ensemble();
        assert_eq!(ens.id, 0x1234);
        assert_eq!(ens.label, "Test Ensemble");
        assert_eq!(ens.services[0].label, "Test Radio");
    }
}
