/**
 * Preferences Component
 * File: gaia/static/modules/settings/preferences.js
 * Purpose: User preferences
 */
class PreferencesPanel {
  constructor(options) {
    this.container = options.container;
    this.preferences = this.loadPreferences();
  }

  loadPreferences() {
    const saved = localStorage.getItem('gaia-preferences');
    return saved ? JSON.parse(saved) : {
      language: 'en',
      notifications: true,
      soundEnabled: true,
      autoUpdate: true,
      compactMode: false,
    };
  }

  render() {
    const html = `
      <div class="preferences-form">
        <div class="pref-group">
          <label>Language:</label>
          <select class="pref-select" data-pref="language">
            <option value="en" ${this.preferences.language === 'en' ? 'selected' : ''}>English</option>
            <option value="es" ${this.preferences.language === 'es' ? 'selected' : ''}>Español</option>
            <option value="fr" ${this.preferences.language === 'fr' ? 'selected' : ''}>Français</option>
          </select>
        </div>

        <div class="pref-group">
          <label>
            <input type="checkbox" class="pref-checkbox" data-pref="notifications" 
              ${this.preferences.notifications ? 'checked' : ''}>
            Enable Notifications
          </label>
        </div>

        <div class="pref-group">
          <label>
            <input type="checkbox" class="pref-checkbox" data-pref="soundEnabled"
              ${this.preferences.soundEnabled ? 'checked' : ''}>
            Enable Sound
          </label>
        </div>

        <div class="pref-group">
          <label>
            <input type="checkbox" class="pref-checkbox" data-pref="autoUpdate"
              ${this.preferences.autoUpdate ? 'checked' : ''}>
            Auto Update
          </label>
        </div>

        <div class="pref-group">
          <label>
            <input type="checkbox" class="pref-checkbox" data-pref="compactMode"
              ${this.preferences.compactMode ? 'checked' : ''}>
            Compact Mode
          </label>
        </div>

        <button class="pref-save-btn">Save Preferences</button>
      </div>
    `;

    this.container.innerHTML = html;
    this.attachListeners();
  }

  attachListeners() {
    const selects = this.container.querySelectorAll('.pref-select');
    const checkboxes = this.container.querySelectorAll('.pref-checkbox');
    const saveBtn = this.container.querySelector('.pref-save-btn');

    selects.forEach(select => {
      select.addEventListener('change', (e) => {
        this.preferences[e.target.dataset.pref] = e.target.value;
      });
    });

    checkboxes.forEach(checkbox => {
      checkbox.addEventListener('change', (e) => {
        this.preferences[e.target.dataset.pref] = e.target.checked;
      });
    });

    saveBtn.addEventListener('click', () => this.savePreferences());
  }

  savePreferences() {
    localStorage.setItem('gaia-preferences', JSON.stringify(this.preferences));
    window.dispatchEvent(new CustomEvent('preferences:saved', { detail: this.preferences }));
    alert('Preferences saved!');
  }
}

export default PreferencesPanel;