/*
 * App Launcher Component
 * File: gaia/static/modules/sidebar/app-launcher.js
 */
class AppLauncher {
  constructor(options) {
    this.container = options.container;
  }

  async render() {
    this.container.innerHTML = `
      <div class="app-list">
        <a class="app-nav-item" data-app="file-manager" title="File Manager">📁</a>
        <a class="app-nav-item" data-app="terminal" title="Terminal">⌨️</a>
        <a class="app-nav-item" data-app="editor" title="Editor">📝</a>
        <a class="app-nav-item" data-app="settings" title="Settings">⚙️</a>
      </div>
    `;

    this.container.querySelectorAll('.app-nav-item').forEach(item => {
      item.addEventListener('click', () => {
        window.gaiaApp.launchApp(item.dataset.app);
      });
    });
  }
}
export default AppLauncher;