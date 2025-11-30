/**
 * App Buttons Component
 * File: gaia/static/modules/taskbar/app-buttons.js
 */
class AppButtons {
  constructor(options) {
    this.container = options.container;
    this.taskbar = options.taskbar;
    this.buttons = [];
  }

  async render() {
    const apps = [
      { id: 'file-manager', icon: '📁', label: 'Files' },
      { id: 'terminal', icon: '⌨️', label: 'Terminal' },
      { id: 'media-player', icon: '🎵', label: 'Media' },
      { id: 'editor', icon: '📝', label: 'Editor' },
      { id: 'email', icon: '📧', label: 'Email' },
      { id: 'settings', icon: '⚙️', label: 'Settings' }
    ];

    this.container.innerHTML = apps.map(app => `
      <button class="taskbar-button" data-app-id="${app.id}" title="${app.label}">
        <span class="icon">${app.icon}</span>
      </button>
    `).join('');

    this.attachListeners();
  }

  attachListeners() {
    this.container.querySelectorAll('.taskbar-button').forEach(btn => {
      btn.addEventListener('click', () => {
        const appId = btn.dataset.appId;
        this.taskbar.launchApp(appId);
      });
    });
  }
}

export default AppButtons;