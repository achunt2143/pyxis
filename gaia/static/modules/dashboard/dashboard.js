/**
 * Dashboard Component
 * File: gaia/static/modules/dashboard/dashboard.js
 */

import { ModuleLoader } from '../../scripts/module-loader.js';

class Dashboard {
  constructor(options) {
    this.element = options.element;
    this.widgets = [];
  }

  async initialize() {
    await ModuleLoader.loadStyles('dashboard');
    this.createStructure();
    this.loadWidgets();
  }

  createStructure() {
    this.element.innerHTML = `
      <div class="dashboard-container">
        <h1>Welcome to Gaia Shell</h1>
        <div class="widgets-grid" id="widgets-grid"></div>
      </div>
    `;
  }

  loadWidgets() {
    const grid = document.getElementById('widgets-grid');
    const widgets = [
      { title: 'System Status', content: 'CPU: 15% RAM: 4GB/8GB' },
      { title: 'Recent Files', content: 'No recent files' },
      { title: 'Quick Actions', content: 'Open File Manager' }
    ];

    grid.innerHTML = widgets.map(w => `
      <div class="widget">
        <h3>${w.title}</h3>
        <p>${w.content}</p>
      </div>
    `).join('');
  }

  async destroy() {}
}
export default Dashboard;