/**
 * Gaia Cache Service
 * File: gaia/static/services/cache-service.js
 * Purpose: LRU cache with TTL support for performance optimization
 */

class CacheService {
  constructor(maxSize = 100) {
    this.cache = new Map();
    this.maxSize = maxSize;
    this.stats = {
      hits: 0,
      misses: 0,
      evictions: 0
    };
  }

  /**
   * Set cache entry with optional TTL
   */
  set(key, value, ttl = null) {
    // Remove existing if present (to update position)
    if (this.cache.has(key)) {
      this.cache.delete(key);
    }

    // Evict if at max size
    if (this.cache.size >= this.maxSize) {
      const firstKey = this.cache.keys().next().value;
      this.cache.delete(firstKey);
      this.stats.evictions++;
    }

    const entry = {
      value,
      timestamp: Date.now(),
      ttl
    };

    this.cache.set(key, entry);

    // Set timeout for TTL expiry
    if (ttl) {
      setTimeout(() => this.delete(key), ttl);
    }
  }

  /**
   * Get cache entry
   */
  get(key) {
    if (!this.cache.has(key)) {
      this.stats.misses++;
      return null;
    }

    const entry = this.cache.get(key);

    // Check TTL
    if (entry.ttl && Date.now() - entry.timestamp > entry.ttl) {
      this.cache.delete(key);
      this.stats.misses++;
      return null;
    }

    // Move to end (LRU)
    this.cache.delete(key);
    this.cache.set(key, entry);

    this.stats.hits++;
    return entry.value;
  }

  /**
   * Delete entry
   */
  delete(key) {
    return this.cache.delete(key);
  }

  /**
   * Check if key exists
   */
  has(key) {
    return this.cache.has(key);
  }

  /**
   * Clear all cache
   */
  clear() {
    this.cache.clear();
    this.stats = { hits: 0, misses: 0, evictions: 0 };
  }

  /**
   * Get hit rate
   */
  getHitRate() {
    const total = this.stats.hits + this.stats.misses;
    return total === 0 ? 0 : (this.stats.hits / total) * 100;
  }

  /**
   * Get cache statistics
   */
  getStats() {
    return {
      ...this.stats,
      size: this.cache.size,
      maxSize: this.maxSize,
      hitRate: this.getHitRate().toFixed(2) + '%'
    };
  }
}

export default CacheService;