/**
 * Gaia Application Bootstrap
 * File: gaia/static/scripts/app.js
 * Purpose: Initialize and manage Gaia Shell application
 */

// Import services
import { APIService } from './services/api.js';
import { EchoService } from './services/echo.js';
import { CacheService } from './services/cache.js';
import { PerformanceService } from './services/performance.js';

// Import module loader if it's a separate file
import { ModuleLoader } from './modules/loader.js';


class GaiaApp {
  constructor() {
    this.modules = new Map();
    this.services = new Map();
    this.config = {
      debug: true,
      apiUrl: 'http://localhost:3000/api',
      echoSocket: 'ws://localhost:8080'
    };
    this.eventBus = new EventTarget();
  }

  /**
   * Bootstrap the application
   */
  async bootstrap() {
    try {
      console.log('🚀 Gaia Shell v2.0 Bootstrapping...');
      
      // Initialize core systems
      await this.initializeServices();
      await this.initializeModules();
      await this.setupEventListeners();
      
      console.log('✅ Gaia Shell Ready');
      this.emit('app:ready');
    } catch (error) {
      console.error('❌ Bootstrap failed:', error);
      this.showError('Failed to initialize Gaia Shell');
    }
  }

  /**
   * Initialize services
   */
  async initializeServices() {
    console.log('📦 Initializing services...');
    
    // API Service
    const apiService = new APIService(this.config.apiUrl);
    this.services.set('api', apiService);
    
    // Echo IPC Service
    const echoService = new EchoService(this.config.echoSocket);
    await echoService.connect();
    this.services.set('echo', echoService);
    
    // Cache Service
    const cacheService = new CacheService();
    this.services.set('cache', cacheService);
    
    // Performance Service
    const perfService = new PerformanceService();
    this.services.set('performance', perfService);
  }

  /**
   * Initialize modules
   */
  async initializeModules() {
    console.log('🎨 Initializing modules...');
    
    const modulesToLoad = [
      { name: 'taskbar', selector: '#taskbar' },
      { name: 'sidebar', selector: '#sidebar' },
      { name: 'dashboard', selector: '#dashboard' },
      { name: 'window-manager', selector: '#windows-container' }
    ];

    for (const module of modulesToLoad) {
      try {
        const ModuleClass = await ModuleLoader.load(module.name);
        const instance = new ModuleClass({
          element: document.querySelector(module.selector),
          app: this,
          services: this.services
        });
        
        await instance.initialize();
        this.modules.set(module.name, instance);
        console.log(`✓ Loaded: ${module.name}`);
      } catch (error) {
        console.error(`✗ Failed to load module: ${module.name}`, error);
      }
    }
  }

  /**
   * Setup event listeners
   */
  setupEventListeners() {
    // Context menu
    document.addEventListener('contextmenu', (e) => this.handleContextMenu(e));
    
    // Keyboard shortcuts
    document.addEventListener('keydown', (e) => this.handleKeyboard(e));
    
    // Window focus
    window.addEventListener('focus', () => this.emit('app:focused'));
    window.addEventListener('blur', () => this.emit('app:blurred'));
  }

  /**
   * Handle context menu
   */
  handleContextMenu(event) {
    event.preventDefault();
    const contextMenu = document.getElementById('context-menu');
    contextMenu.style.left = event.pageX + 'px';
    contextMenu.style.top = event.pageY + 'px';
    contextMenu.classList.add('active');
  }

  /**
   * Handle keyboard shortcuts
   */
  handleKeyboard(event) {
    // Ctrl+Alt+T: Open Terminal
    if (event.ctrlKey && event.altKey && event.key === 't') {
      event.preventDefault();
      this.launchApp('terminal');
    }
    
    // Ctrl+Alt+F: Open File Manager
    if (event.ctrlKey && event.altKey && event.key === 'f') {
      event.preventDefault();
      this.launchApp('file-manager');
    }
    
    // Alt+Tab: Switch windows
    if (event.altKey && event.key === 'Tab') {
      event.preventDefault();
      this.switchWindow();
    }
  }

  /**
   * Launch application
   */
  async launchApp(appId) {
    try {
      const echo = this.services.get('echo');
      const result = await echo.call('org.geckoOS.Persephone', 'executeApplication', {
        appId: appId
      });
      
      this.emit('app:launched', { appId, result });
    } catch (error) {
      console.error('Failed to launch app:', error);
    }
  }

  /**
   * Switch windows
   */
  switchWindow() {
    const windowManager = this.modules.get('window-manager');
    if (windowManager) {
      windowManager.switchWindow();
    }
  }

  /**
   * Show error notification
   */
  showError(message) {
    const notification = document.createElement('div');
    notification.className = 'alert alert-danger';
    notification.textContent = message;
    
    const container = document.getElementById('notifications');
    container.appendChild(notification);
    
    setTimeout(() => notification.remove(), 5000);
  }

  /**
   * Emit event
   */
  emit(eventName, detail = {}) {
    const event = new CustomEvent(eventName, { detail });
    this.eventBus.dispatchEvent(event);
    window.dispatchEvent(event);
  }

  /**
   * Subscribe to event
   */
  on(eventName, callback) {
    this.eventBus.addEventListener(eventName, callback);
  }

  /**
   * Get service
   */
  getService(name) {
    return this.services.get(name);
  }

  /**
   * Get module
   */
  getModule(name) {
    return this.modules.get(name);
  }
}

// Initialize when DOM ready
if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', () => {
    window.gaiaApp = new GaiaApp();
    window.gaiaApp.bootstrap();
  });
} else {
  window.gaiaApp = new GaiaApp();
  window.gaiaApp.bootstrap();
}
