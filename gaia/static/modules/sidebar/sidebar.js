/**
 * Sidebar Component
 * File: gaia/static/modules/sidebar/sidebar.js
 */

import { ModuleLoader } from '../../scripts/module-loader.js';

class Sidebar {
  constructor(options) {
    this.element = options.element;
    this.app = options.app;
  }

  async initialize() {
    await ModuleLoader.loadStyles('sidebar');
    this.createStructure();
    this.loadSubModules();
  }

  createStructure() {
    this.element.innerHTML = `
      <div class="sidebar-container">
        <div class="sidebar-header">
          <div class="logo">G</div>
        </div>
        <nav class="sidebar-nav" id="sidebar-nav"></nav>
        <div class="sidebar-footer" id="sidebar-footer"></div>
      </div>
    `;
  }

  async loadSubModules() {
    try {
      const AppLauncher = await import('./app-launcher.js').then(m => m.default);
      const launcher = new AppLauncher({ container: document.getElementById('sidebar-nav') });
      await launcher.render();

      const Favorites = await import('./favorites.js').then(m => m.default);
      const favorites = new Favorites({ container: document.getElementById('sidebar-footer') });
      await favorites.render();
    } catch (error) {
      console.error('Sidebar submodules failed:', error);
    }
  }

  async destroy() {}
}
export default Sidebar;