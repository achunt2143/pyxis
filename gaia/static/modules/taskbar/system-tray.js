/**
 * System Tray Component
 * File: gaia/static/modules/taskbar/system-tray.js
 */
class SystemTray {
  constructor(options) {
    this.container = options.container;
  }

  async render() {
    this.container.innerHTML = `
      <div class="tray-items">
        <div class="tray-item network" title="Network">📡</div>
        <div class="tray-item volume" title="Volume">🔊</div>
        <div class="tray-item battery" title="Battery">🔋</div>
      </div>
    `;
  }
}
export default SystemTray;