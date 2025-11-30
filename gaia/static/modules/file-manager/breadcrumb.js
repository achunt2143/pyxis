/**
 * Breadcrumb Component
 * File: gaia/static/modules/file-manager/breadcrumb.js
 * Purpose: Navigation breadcrumb
 */
class Breadcrumb {
  constructor(options) {
    this.container = options.container;
    this.currentPath = options.currentPath || '/home/user';
  }

  render() {
    const parts = this.currentPath.split('/').filter(p => p);
    const breadcrumbs = [
      { label: '/', path: '/' },
      ...parts.map((part, index) => {
        const path = '/' + parts.slice(0, index + 1).join('/');
        return { label: part, path };
      })
    ];

    const html = breadcrumbs.map((crumb, index) => `
      <div class="breadcrumb-item" data-path="${crumb.path}">
        <a href="#" class="breadcrumb-link">${crumb.label}</a>
        ${index < breadcrumbs.length - 1 ? '<span class="breadcrumb-sep">/</span>' : ''}
      </div>
    `).join('');

    this.container.innerHTML = html;
    this.attachListeners();
  }

  attachListeners() {
    const items = this.container.querySelectorAll('.breadcrumb-link');
    items.forEach(item => {
      item.addEventListener('click', (e) => {
        e.preventDefault();
        const path = item.closest('.breadcrumb-item').dataset.path;
        window.dispatchEvent(new CustomEvent('breadcrumb:navigate', { detail: { path } }));
      });
    });
  }

  setPath(path) {
    this.currentPath = path;
    this.render();
  }
}

export { Breadcrumb };