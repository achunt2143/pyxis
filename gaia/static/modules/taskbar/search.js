/**
 * Search Component
 * File: gaia/static/modules/taskbar/search.js
 * Purpose: Quick search functionality
 */
class TaskbarSearch {
  constructor(options) {
    this.container = options.container;
    this.placeholder = options.placeholder || 'Search...';
  }

  render() {
    const searchHTML = `
      <div class="taskbar-search">
        <input 
          type="text" 
          class="search-input" 
          placeholder="${this.placeholder}"
          aria-label="Search"
        >
        <button class="search-button" aria-label="Search button">
          🔍
        </button>
      </div>
    `;

    this.container.innerHTML = searchHTML;
    this.attachListeners();
  }

  attachListeners() {
    const input = this.container.querySelector('.search-input');
    const button = this.container.querySelector('.search-button');

    input.addEventListener('keypress', (e) => {
      if (e.key === 'Enter') {
        this.performSearch(input.value);
      }
    });

    button.addEventListener('click', () => {
      this.performSearch(input.value);
    });
  }

  performSearch(query) {
    window.dispatchEvent(new CustomEvent('search:execute', {
      detail: { query }
    }));
  }
}

export default TaskbarSearch;