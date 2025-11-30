/**
 * Settings Component
 * File: gaia/static/modules/settings/settings.js
 */

import { ModuleLoader } from '../../scripts/module-loader.js';

class Settings {
  constructor(options) {
    this.element = options.element;
    this.preferences = {};
  }

  async initialize() {
    await ModuleLoader.loadStyles('settings');
    this.createStructure();
  }

  createStructure() {
    this.element.innerHTML = `
      <div class="settings-container">
        <div class="settings-sidebar" id="settings-sidebar"></div>
        <div class="settings-panel" id="settings-panel"></div>
      </div>
    `;

    const sidebar = document.getElementById('settings-sidebar');
    sidebar.innerHTML = `
      <div class="settings-item">🎨 Themes</div>
      <div class="settings-item">⚙️ Preferences</div>
      <div class="settings-item">🔒 Security</div>
      <div class="settings-item">ℹ️ About</div>
    `;
  }

  async destroy() {}
}
export default Settings;