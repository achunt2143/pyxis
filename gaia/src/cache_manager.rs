// ============================================================================
// CACHE MANAGER MODULE
// File: gaia/src/cache_manager.rs
// Purpose: LRU cache with TTL support
// ============================================================================

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug)]
struct CacheEntry {
    key: String,
    value: Vec<u8>,
    timestamp: u64,
    ttl: Option<u64>,
}

pub struct CacheManager {
    cache: HashMap<String, CacheEntry>,
    max_size: usize,
    stats: CacheStats,
}

#[derive(Debug, Clone)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
}

impl CacheManager {
    pub fn new(max_size: usize) -> Self {
        info!("📦 Initializing Cache Manager (max_size: {})", max_size);
        
        CacheManager {
            cache: HashMap::new(),
            max_size,
            stats: CacheStats {
                hits: 0,
                misses: 0,
                evictions: 0,
            },
        }
    }

    pub fn set(&mut self, key: String, value: Vec<u8>, ttl: Option<u64>) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        if self.cache.len() >= self.max_size {
            if let Some((oldest_key, _)) = self.cache
                .iter()
                .min_by_key(|(_, entry)| entry.timestamp)
                .map(|(k, e)| (k.clone(), e.clone()))
            {
                self.cache.remove(&oldest_key);
                self.stats.evictions += 1;
            }
        }

        self.cache.insert(key, CacheEntry {
            key: key.clone(),
            value,
            timestamp: now,
            ttl,
        });

        debug!("✅ Cache entry set: {}", key);
    }

    pub fn get(&mut self, key: &str) -> Option<Vec<u8>> {
        if let Some(entry) = self.cache.get(key) {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();

            if let Some(ttl) = entry.ttl {
                if now - entry.timestamp > ttl {
                    self.cache.remove(key);
                    self.stats.misses += 1;
                    return None;
                }
            }

            self.stats.hits += 1;
            return Some(entry.value.clone());
        }

        self.stats.misses += 1;
        None
    }

    pub fn clear(&mut self) {
        self.cache.clear();
        debug!("🗑️ Cache cleared");
    }

    pub fn get_stats(&self) -> CacheStats {
        self.stats.clone()
    }
}