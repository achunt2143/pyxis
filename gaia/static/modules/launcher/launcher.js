/**
 * Launcher Component
 * File: gaia/static/modules/launcher/launcher.js
 */

import { ModuleLoader } from '../../scripts/module-loader.js';

class Launcher {
  constructor(options) {
    this.element = options.element;
    this.apps = [];
  }

  async initialize() {
    await ModuleLoader.loadStyles('launcher');
    this.createStructure();
  }

  createStructure() {
    this.element.innerHTML = `
      <div class="launcher-container">
        <input type="text" class="launcher-search" id="launcher-search" placeholder="Search applications...">
        <div class="app-grid" id="app-grid"></div>
      </div>
    `;

    this.populateApps();
  }

  populateApps() {
    const apps = [
      { name: 'Files', icon: '📁' },
      { name: 'Terminal', icon: '⌨️' },
      { name: 'Editor', icon: '📝' },
      { name: 'Media Player', icon: '🎵' },
      { name: 'Email', icon: '📧' },
      { name: 'Settings', icon: '⚙️' }
    ];

    const grid = document.getElementById('app-grid');
    grid.innerHTML = apps.map(app => `
      <div class="app-icon">
        <div>${app.icon}</div>
        <span>${app.name}</span>
      </div>
    `).join('');
  }

  async destroy() {}
}

export default Launcher;