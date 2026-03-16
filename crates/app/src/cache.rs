/// Persistent channel-scan cache stored at `~/.config/dab-rtl/cache.json`.
///
/// The cache maps DAB Band III channel names (e.g. `"11C"`) to the ensemble
/// metadata discovered during the last successful scan of that channel.  It is
/// written after every `scan` run and read on `tune` / `play` so that the TUI
/// can display previously-discovered services immediately while the live FIC
/// stream re-decodes in the background.
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// ─────────────────────────────────────────────────────────────────────────── //
//  Data model                                                                  //
// ─────────────────────────────────────────────────────────────────────────── //

/// Minimal per-service information stored in the cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedService {
    pub id: u32,
    pub label: String,
    pub is_dab_plus: bool,
}

/// Cached ensemble for a single Band III channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedEnsemble {
    pub id: u16,
    pub label: String,
    pub services: Vec<CachedService>,
}

// ─────────────────────────────────────────────────────────────────────────── //
//  Cache                                                                       //
// ─────────────────────────────────────────────────────────────────────────── //

/// Persistent cache mapping channel name → ensemble.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Cache {
    channels: HashMap<String, CachedEnsemble>,
}

impl Cache {
    /// Default cache file path: `$XDG_CONFIG_HOME/dab-rtl/cache.json` or
    /// `~/.config/dab-rtl/cache.json`.
    pub fn default_path() -> PathBuf {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_else(|| PathBuf::from(".config"));
        base.join("dab-rtl").join("cache.json")
    }

    /// Load cache from `path`.  Returns an empty cache if the file does not
    /// exist or cannot be parsed (non-fatal).
    pub fn load(path: &PathBuf) -> Self {
        let Ok(data) = fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str(&data).unwrap_or_default()
    }

    /// Persist the cache to `path`, creating parent directories as needed.
    pub fn save(&self, path: &PathBuf) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        fs::write(path, json)
    }

    /// Insert or replace the ensemble for `channel`.
    pub fn put(&mut self, channel: &str, ensemble: CachedEnsemble) {
        self.channels.insert(channel.to_owned(), ensemble);
    }

    /// Look up the cached ensemble for `channel`.
    pub fn get(&self, channel: &str) -> Option<&CachedEnsemble> {
        self.channels.get(channel)
    }

    /// Remove all cached entries.
    pub fn clear(&mut self) {
        self.channels.clear();
    }
}

// ─────────────────────────────────────────────────────────────────────────── //
//  Tests                                                                       //
// ─────────────────────────────────────────────────────────────────────────── //

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_ensemble() -> CachedEnsemble {
        CachedEnsemble {
            id: 0x1001,
            label: "Test Ensemble".into(),
            services: vec![
                CachedService {
                    id: 0xAABB,
                    label: "Radio A".into(),
                    is_dab_plus: false,
                },
                CachedService {
                    id: 0xCCDD,
                    label: "Radio B".into(),
                    is_dab_plus: true,
                },
            ],
        }
    }

    #[test]
    fn put_and_get() {
        let mut cache = Cache::default();
        cache.put("11C", sample_ensemble());
        let ens = cache.get("11C").unwrap();
        assert_eq!(ens.label, "Test Ensemble");
        assert_eq!(ens.services.len(), 2);
    }

    #[test]
    fn get_missing_returns_none() {
        let cache = Cache::default();
        assert!(cache.get("11C").is_none());
    }

    #[test]
    fn clear_removes_all() {
        let mut cache = Cache::default();
        cache.put("11C", sample_ensemble());
        cache.clear();
        assert!(cache.get("11C").is_none());
    }

    #[test]
    fn save_and_load_roundtrip() {
        let path = std::env::temp_dir().join("dab_rtl_test_cache.json");
        let mut cache = Cache::default();
        cache.put("11C", sample_ensemble());
        cache.save(&path).unwrap();

        let loaded = Cache::load(&path);
        let ens = loaded.get("11C").unwrap();
        assert_eq!(ens.services[0].label, "Radio A");
        assert_eq!(ens.services[1].is_dab_plus, true);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn load_missing_returns_empty() {
        let path = PathBuf::from("/nonexistent/path/cache.json");
        let cache = Cache::load(&path);
        assert!(cache.get("11C").is_none());
    }
}
