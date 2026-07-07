// Performance Layer - Caching System
// File: performance/src/caching.rs
// Purpose: Intelligent multi-level caching strategy
use anyhow::Result;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tracing::{debug, info};

pub struct CacheEntry<T> {
    pub value: T,
    pub created_at: Instant,
    pub accessed_at: Instant,
    pub access_count: u32,
    pub ttl: Option<Duration>,
}

#[derive(Default)]
pub struct LRUCache<T> {
    cache: Arc<RwLock<HashMap<String, CacheEntry<T>>>>,
    max_size: usize,
    hit_count: Arc<RwLock<u64>>,
    miss_count: Arc<RwLock<u64>>,
}

impl<T: Clone> LRUCache<T> {
    /// Create new LRU cache
    pub async fn new(max_size: usize) -> Result<Self> {
        info!("Creating LRU cache with size {}", max_size);
        
        Ok(LRUCache {
            cache: Arc::new(RwLock::new(HashMap::new())),
            max_size,
            hit_count: Arc::new(RwLock::new(0)),
            miss_count: Arc::new(RwLock::new(0)),
        })
    }
    
    /// Get value from cache
    pub fn get(&self, key: &str) -> Option<T> {
        let mut cache = self.cache.write().unwrap();
        
        if let Some(entry) = cache.get_mut(key) {
            // Check TTL
            if let Some(ttl) = entry.ttl {
                if entry.created_at.elapsed() > ttl {
                    cache.remove(key);
                    *self.miss_count.write().unwrap() += 1;
                    return None;
                }
            }
            
            entry.accessed_at = Instant::now();
            entry.access_count += 1;
            
            *self.hit_count.write().unwrap() += 1;
            debug!("Cache HIT: {}", key);
            
            return Some(entry.value.clone());
        }
        
        *self.miss_count.write().unwrap() += 1;
        debug!("Cache MISS: {}", key);
        None
    }
    
    /// Put value in cache
    pub fn put(&self, key: String, value: T, ttl: Option<Duration>) {
        let mut cache = self.cache.write().unwrap();
        
        // Check size and evict if necessary
        if cache.len() >= self.max_size && !cache.contains_key(&key) {
            self.evict_lru(&mut cache);
        }
        
        let entry = CacheEntry {
            value,
            created_at: Instant::now(),
            accessed_at: Instant::now(),
            access_count: 1,
            ttl,
        };
        
        cache.insert(key, entry);
        debug!("Cache PUT: {} entries", cache.len());
    }
    
    /// Evict least recently used entry
    fn evict_lru(&self, cache: &mut HashMap<String, CacheEntry<T>>) {
        if let Some((key_to_remove, _)) = cache
            .iter()
            .min_by_key(|(_, entry)| entry.accessed_at)
        {
            let key = key_to_remove.to_string();
            cache.remove(&key);
            debug!("Cache EVICT: {} (LRU)", key);
        }
    }
    
    /// Clear cache
    pub fn clear(&self) {
        self.cache.write().unwrap().clear();
        debug!("Cache cleared");
    }
    
    /// Get cache statistics
    pub fn stats(&self) -> CacheStats {
        let cache = self.cache.read().unwrap();
        let hits = *self.hit_count.read().unwrap();
        let misses = *self.miss_count.read().unwrap();
        let total = hits + misses;
        
        let hit_rate = if total > 0 {
            (hits as f64 / total as f64) * 100.0
        } else {
            0.0
        };
        
        CacheStats {
            size: cache.len(),
            max_size: self.max_size,
            hits,
            misses,
            hit_rate,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CacheStats {
    pub size: usize,
    pub max_size: usize,
    pub hits: u64,
    pub misses: u64,
    pub hit_rate: f64,
}

/// Multi-level cache system
pub struct MultiLevelCache {
    pub l1_cache: Arc<LRUCache<Vec<u8>>>,  // Fast, small
    pub l2_cache: Arc<LRUCache<Vec<u8>>>,  // Medium, larger
    pub l3_cache: Arc<LRUCache<Vec<u8>>>,  // Large, slower
}

impl MultiLevelCache {
    /// Create multi-level cache hierarchy
    pub async fn new() -> Result<Self> {
        info!("Creating multi-level cache hierarchy");
        
        Ok(MultiLevelCache {
            l1_cache: Arc::new(LRUCache::new(256).await?),   // L1: 256 entries
            l2_cache: Arc::new(LRUCache::new(1024).await?),  // L2: 1024 entries
            l3_cache: Arc::new(LRUCache::new(4096).await?),  // L3: 4096 entries
        })
    }
    
    /// Get from cache hierarchy
    pub fn get(&self, key: &str) -> Option<Vec<u8>> {
        // Try L1
        if let Some(value) = self.l1_cache.get(key) {
            return Some(value);
        }
        
        // Try L2
        if let Some(value) = self.l2_cache.get(key) {
            self.l1_cache.put(key.to_string(), value.clone(), None);
            return Some(value);
        }
        
        // Try L3
        if let Some(value) = self.l3_cache.get(key) {
            self.l2_cache.put(key.to_string(), value.clone(), None);
            return Some(value);
        }
        
        None
    }
    
    /// Put in cache hierarchy
    pub fn put(&self, key: String, value: Vec<u8>) {
        self.l1_cache.put(key.clone(), value.clone(), None);
        self.l2_cache.put(key.clone(), value.clone(), None);
        self.l3_cache.put(key, value, None);
    }
}

// #[cfg(test)]
// mod tests {
//     use super::*;
    
//     #[test]
//     fn test_lru_cache() {
//         let cache = LRUCache::new(2).await?;
        
//         cache.put("key1".to_string(), "value1".to_string(), None);
//         cache.put("key2".to_string(), "value2".to_string(), None);
        
//         assert_eq!(cache.get("key1"), Some("value1".to_string()));
//         assert_eq!(cache.get("key2"), Some("value2".to_string()));
//     }
    
//     #[test]
//     fn test_cache_statistics() {
//         let cache: LRUCache<String> = LRUCache::new(10).await?;
        
//         cache.put("key".to_string(), "value".to_string(), None);
//         cache.get("key");
//         cache.get("missing");
        
//         let stats = cache.stats();
//         assert_eq!(stats.hits, 1);
//         assert_eq!(stats.misses, 1);
//     }
// }
