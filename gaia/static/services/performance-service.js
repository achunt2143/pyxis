/**
 * Gaia Performance Service
 * File: gaia/static/services/performance-service.js
 * Purpose: Performance monitoring and optimization
 */

class PerformanceService {
  constructor() {
    this.metrics = {
      startTime: performance.now(),
      frameCount: 0,
      fps: 0,
      memory: {}
    };
  }

  /**
   * Get current metrics
   */
  getMetrics() {
    if (performance.memory) {
      this.metrics.memory = {
        usedJSHeapSize: performance.memory.usedJSHeapSize,
        totalJSHeapSize: performance.memory.totalJSHeapSize,
        jsHeapSizeLimit: performance.memory.jsHeapSizeLimit
      };
    }

    return this.metrics;
  }

  /**
   * Measure operation
   */
  measure(name, fn) {
    const start = performance.now();
    const result = fn();
    const duration = performance.now() - start;
    console.log(`${name}: ${duration.toFixed(2)}ms`);
    return result;
  }

  /**
   * Get page load time
   */
  getPageLoadTime() {
    const perfData = performance.timing;
    return perfData.loadEventEnd - perfData.navigationStart;
  }
}

export { PerformanceService };