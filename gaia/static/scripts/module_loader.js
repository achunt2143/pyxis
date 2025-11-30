/**
 * Gaia Module Loader System
 * File: gaia/static/scripts/module-loader.js
 * Purpose: Dynamic module loading with caching and dependency resolution
 */

class ModuleLoader {
  static modules = new Map();
  static loading = new Map();
  static cache = new Map();

  /**
   * Load a module dynamically
   */
  static async load(moduleName) {
    // Return from cache if available
    if (this.modules.has(moduleName)) {
      return this.modules.get(moduleName);
    }

    // Return pending promise if already loading
    if (this.loading.has(moduleName)) {
      return this.loading.get(moduleName);
    }

    // Start loading
    const loadPromise = this._loadModule(moduleName);
    this.loading.set(moduleName, loadPromise);

    try {
      const module = await loadPromise;
      this.modules.set(moduleName, module);
      return module;
    } finally {
      this.loading.delete(moduleName);
    }
  }

  /**
   * Internal module loading logic
   */
  static async _loadModule(moduleName) {
    const modulePath = `/gaia/static/modules/${moduleName}/${moduleName}.js`;
    
    try {
      // Dynamic import
      const moduleExport = await import(modulePath);
      const ModuleClass = moduleExport.default || moduleExport[this._toCamelCase(moduleName)];
      
      if (!ModuleClass) {
        throw new Error(`Module ${moduleName} does not export default class`);
      }

      return ModuleClass;
    } catch (error) {
      console.error(`Failed to load module ${moduleName}:`, error);
      throw new Error(`Module loading failed: ${moduleName}`);
    }
  }

  /**
   * Load module stylesheet
   */
  static async loadStyles(moduleName) {
    const cacheKey = `styles-${moduleName}`;
    
    if (this.cache.has(cacheKey)) {
      return;
    }

    const stylePath = `/gaia/static/modules/${moduleName}/${moduleName}.css`;
    
    return new Promise((resolve, reject) => {
      const link = document.createElement('link');
      link.rel = 'stylesheet';
      link.href = stylePath;
      link.onload = () => {
        this.cache.set(cacheKey, true);
        resolve();
      };
      link.onerror = reject;
      document.head.appendChild(link);
    });
  }

  /**
   * Preload multiple modules
   */
  static async preload(moduleNames) {
    return Promise.all(moduleNames.map(name => this.load(name)));
  }

  /**
   * Clear cache
   */
  static clearCache() {
    this.modules.clear();
    this.cache.clear();
  }

  /**
   * Convert kebab-case to camelCase
   */
  static _toCamelCase(str) {
    return str.replace(/-([a-z])/g, (g) => g[1].toUpperCase());
  }

  /**
   * Get loading progress
   */
  static getProgress() {
    return {
      total: this.modules.size,
      loading: this.loading.size,
      cached: this.cache.size
    };
  }
}

export { ModuleLoader }; 
