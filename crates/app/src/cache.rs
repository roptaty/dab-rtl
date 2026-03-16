/// Persistent channel cache.
///
/// Scanned channels and their services are stored in
/// `~/.config/dab-rtl/cache.json` so that the TUI can show a service list
/// immediately on the next launch without waiting for a full FIC decode cycle.
///
/// The cache is keyed by DAB channel name (e.g. `"11C"`) or raw frequency
/// string for file-based sources (not cached).
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// ─────────────────────────────────────────────────────────────────────────── //
//  Serialisable data types (mirrors protocol::Ensemble / Service)             //
// ─────────────────────────────────────────────────────────────────────────── //

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CachedService {
    pub id: u32,
    pub label: String,
    pub is_dab_plus: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CachedEnsemble {
    pub id: u16,
    pub label: String,
    pub services: Vec<CachedService>,
}

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Cache {
    pub entries: HashMap<String, CachedEnsemble>,
}

// ─────────────────────────────────────────────────────────────────────────── //
//  File path                                                                   //
// ─────────────────────────────────────────────────────────────────────────── //

pub fn cache_path() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join(".config")
        .join("dab-rtl")
        .join("cache.json")
}

// ─────────────────────────────────────────────────────────────────────────── //
//  Public API                                                                  //
// ─────────────────────────────────────────────────────────────────────────── //

/// Load the cache from disk.  Returns an empty cache on any error.
pub fn load() -> Cache {
    let path = cache_path();
    let Ok(data) = fs::read_to_string(&path) else {
        return Cache::default();
    };
    serde_json::from_str(&data).unwrap_or_default()
}

/// Persist the cache to disk, creating parent directories as needed.
pub fn save(cache: &Cache) {
    let path = cache_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(data) = serde_json::to_string_pretty(cache) {
        let _ = fs::write(&path, data);
    }
}

/// Store one ensemble (keyed by channel name) and flush to disk.
///
/// No-ops if the ensemble has no label and no services (no lock obtained).
pub fn put(channel: &str, ensemble: &protocol::Ensemble) {
    if ensemble.label.is_empty() && ensemble.services.is_empty() {
        return;
    }
    let mut cache = load();
    cache.entries.insert(
        channel.to_string(),
        CachedEnsemble {
            id: ensemble.id,
            label: ensemble.label.clone(),
            services: ensemble
                .services
                .iter()
                .map(|s| CachedService {
                    id: s.id,
                    label: s.label.clone(),
                    is_dab_plus: s.is_dab_plus,
                })
                .collect(),
        },
    );
    save(&cache);
}

/// Return the cached ensemble for `channel`, or `None` if not present.
pub fn get_ensemble(channel: &str) -> Option<CachedEnsemble> {
    load().entries.get(channel).cloned()
}

/// Delete the cache file and return its path (for user-facing messages).
pub fn clear() -> PathBuf {
    let path = cache_path();
    if path.exists() {
        let _ = fs::remove_file(&path);
    }
    path
}

// ─────────────────────────────────────────────────────────────────────────── //
//  Tests                                                                       //
// ─────────────────────────────────────────────────────────────────────────── //

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Override HOME so tests don't touch the real config directory.
    fn temp_home() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", dir.path());
        dir
    }

    #[test]
    fn round_trip_empty_cache() {
        let _dir = temp_home();
        let cache = Cache::default();
        save(&cache);
        let loaded = load();
        assert!(loaded.entries.is_empty());
    }

    #[test]
    fn put_and_get() {
        let _dir = temp_home();
        let mut ens = protocol::Ensemble::default();
        ens.label = "Test Ensemble".into();
        ens.id = 0x1234;
        let svc = ens.get_or_insert_service(0xABCD);
        svc.label = "Radio One".into();
        svc.is_dab_plus = false;

        put("11C", &ens);

        let got = get_ensemble("11C").unwrap();
        assert_eq!(got.label, "Test Ensemble");
        assert_eq!(got.services.len(), 1);
        assert_eq!(got.services[0].label, "Radio One");
    }

    #[test]
    fn get_missing_channel_returns_none() {
        let _dir = temp_home();
        assert!(get_ensemble("99Z").is_none());
    }

    #[test]
    fn clear_removes_file() {
        let _dir = temp_home();
        let mut cache = Cache::default();
        cache.entries.insert(
            "11C".into(),
            CachedEnsemble {
                id: 1,
                label: "X".into(),
                services: vec![],
            },
        );
        save(&cache);
        assert!(cache_path().exists());
        clear();
        assert!(!cache_path().exists());
    }

    #[test]
    fn put_skips_empty_ensemble() {
        let _dir = temp_home();
        let ens = protocol::Ensemble::default();
        put("11C", &ens);
        assert!(get_ensemble("11C").is_none());
    }
}
