/**
 * Themes Component
 * File: gaia/static/modules/settings/themes.js
 * Purpose: Theme management
 */
class ThemeManager {
  constructor(options) {
    this.container = options.container;
    this.currentTheme = localStorage.getItem('gaia-theme') || 'light';
  }

  render() {
    const themes = [
      { id: 'light', name: 'Light Theme', icon: '☀️' },
      { id: 'dark', name: 'Dark Theme', icon: '🌙' },
      { id: 'auto', name: 'Auto (System)', icon: '🔄' },
    ];

    const html = `
      <div class="themes-grid">
        ${themes.map(theme => `
          <div class="theme-card ${this.currentTheme === theme.id ? 'active' : ''}">
            <div class="theme-icon">${theme.icon}</div>
            <h3>${theme.name}</h3>
            <button class="theme-select-btn" data-theme="${theme.id}">
              ${this.currentTheme === theme.id ? '✓ Active' : 'Select'}
            </button>
          </div>
        `).join('')}
      </div>
    `;

    this.container.innerHTML = html;
    this.attachListeners();
  }

  attachListeners() {
    const buttons = this.container.querySelectorAll('.theme-select-btn');
    buttons.forEach(btn => {
      btn.addEventListener('click', () => {
        const theme = btn.dataset.theme;
        this.setTheme(theme);
      });
    });
  }

  setTheme(theme) {
    this.currentTheme = theme;
    localStorage.setItem('gaia-theme', theme);
    document.documentElement.dataset.colorScheme = theme;
    window.dispatchEvent(new CustomEvent('theme:changed', { detail: { theme } }));
    this.render();
  }
}

export default ThemeManager;