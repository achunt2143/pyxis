/**
 * Launcher Search Component
 * File: gaia/static/modules/launcher/search.js
 * Purpose: App search functionality
 */
class LauncherSearch {
  constructor(options) {
    this.container = options.container;
    this.apps = options.apps || [];
  }

  render() {
    const html = `
      <div class="launcher-search-wrapper">
        <input 
          type="text" 
          class="launcher-search-input" 
          placeholder="Search applications..."
          id="launcher-search"
        >
        <div id="search-results" class="search-results"></div>
      </div>
    `;

    this.container.innerHTML = html;
    this.attachListeners();
  }

  attachListeners() {
    const input = this.container.querySelector('#launcher-search');
    const results = this.container.querySelector('#search-results');

    input.addEventListener('input', (e) => {
      const query = e.target.value.toLowerCase();
      this.performSearch(query, results);
    });
  }

  performSearch(query, resultsContainer) {
    if (!query) {
      resultsContainer.innerHTML = '';
      return;
    }

    const filtered = this.apps.filter(app =>
      app.name.toLowerCase().includes(query) ||
      app.description?.toLowerCase().includes(query)
    );

    const html = filtered.map(app => `
      <div class="search-result" data-app-id="${app.id}">
        <div class="result-icon">${app.icon}</div>
        <div class="result-info">
          <div class="result-name">${app.name}</div>
          <div class="result-desc">${app.description || ''}</div>
        </div>
      </div>
    `).join('');

    resultsContainer.innerHTML = html;
    this.attachResultListeners(resultsContainer);
  }

  attachResultListeners(resultsContainer) {
    const results = resultsContainer.querySelectorAll('.search-result');
    results.forEach(result => {
      result.addEventListener('click', () => {
        const appId = result.dataset.appId;
        window.gaiaApp.launchApp(appId);
      });
    });
  }
}

export default LauncherSearch;