/**
 * Gaia Taskbar Module
 * File: gaia/static/modules/taskbar/taskbar.js
 * Purpose: Main taskbar component with app buttons, system tray, clock
 */

import { ModuleLoader } from '../../scripts/module-loader.js';

class Taskbar {
  constructor(options = {}) {
    this.element = options.element;
    this.app = options.app;
    this.services = options.services;
    this.apps = [];
    this.activeApp = null;
  }

  /**
   * Initialize taskbar
   */
  async initialize() {
    try {
      // Load stylesheet
      await ModuleLoader.loadStyles('taskbar');
      
      // Create taskbar elements
      this.createStructure();
      this.attachEventListeners();
      this.loadAppButtons();
      this.initClock();
      this.initSystemTray();
      
      console.log('✓ Taskbar initialized');
    } catch (error) {
      console.error('Taskbar initialization failed:', error);
    }
  }

  /**
   * Create taskbar HTML structure
   */
  createStructure() {
    this.element.innerHTML = `
      <div class="taskbar-container">
        <div class="taskbar-apps" id="taskbar-apps"></div>
        <div class="taskbar-center" id="taskbar-center"></div>
        <div class="taskbar-system">
          <div class="system-tray" id="system-tray"></div>
          <div class="clock" id="clock"></div>
        </div>
      </div>
    `;
  }

  /**
   * Load app buttons
   */
  async loadAppButtons() {
    try {
      const AppButtons = await import('./app-buttons.js').then(m => m.default);
      const appButtons = new AppButtons({
        container: document.getElementById('taskbar-apps'),
        taskbar: this
      });
      await appButtons.render();
    } catch (error) {
      console.error('Failed to load app buttons:', error);
    }
  }

  /**
   * Initialize clock
   */
  async initClock() {
    try {
      const Clock = await import('./clock.js').then(m => m.default);
      const clock = new Clock({
        container: document.getElementById('clock')
      });
      clock.start();
    } catch (error) {
      console.error('Failed to initialize clock:', error);
    }
  }

  /**
   * Initialize system tray
   */
  async initSystemTray() {
    try {
      const SystemTray = await import('./system-tray.js').then(m => m.default);
      const tray = new SystemTray({
        container: document.getElementById('system-tray')
      });
      await tray.render();
    } catch (error) {
      console.error('Failed to initialize system tray:', error);
    }
  }

  /**
   * Attach event listeners
   */
  attachEventListeners() {
    window.addEventListener('app:launched', (e) => this.handleAppLaunched(e));
    window.addEventListener('app:closed', (e) => this.handleAppClosed(e));
  }

  /**
   * Handle app launched
   */
  handleAppLaunched(event) {
    const { appId } = event.detail;
    this.apps.push(appId);
    this.updateActiveIndicators();
  }

  /**
   * Handle app closed
   */
  handleAppClosed(event) {
    const { appId } = event.detail;
    this.apps = this.apps.filter(id => id !== appId);
    this.updateActiveIndicators();
  }

  /**
   * Update active app indicators
   */
  updateActiveIndicators() {
    document.querySelectorAll('.taskbar-button').forEach(btn => {
      const appId = btn.dataset.appId;
      btn.classList.toggle('active', this.apps.includes(appId));
    });
  }

  /**
   * Launch application
   */
  async launchApp(appId) {
    try {
      this.app.launchApp(appId);
    } catch (error) {
      console.error('Failed to launch app:', error);
    }
  }

  /**
   * Switch to app
   */
  switchToApp(appId) {
    const windowManager = this.app.getModule('window-manager');
    if (windowManager) {
      windowManager.focusApp(appId);
    }
  }

  /**
   * Cleanup
   */
  async destroy() {
    window.removeEventListener('app:launched', this.handleAppLaunched);
    window.removeEventListener('app:closed', this.handleAppClosed);
  }
}

export default Taskbar;
