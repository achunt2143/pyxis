/**
 * Window Manager Component
 * File: gaia/static/modules/window-manager/windows.js
 */

import { ModuleLoader } from '../../scripts/module-loader.js';

class WindowManager {
  constructor(options) {
    this.element = options.element;
    this.windows = new Map();
    this.zIndex = 100;
  }

  async initialize() {
    await ModuleLoader.loadStyles('window-manager');
    this.attachListeners();
  }

  attachListeners() {
    window.addEventListener('app:launched', (e) => this.createWindow(e.detail));
  }

  createWindow(appInfo) {
    const windowId = `window-${Date.now()}`;
    const windowEl = this.createWindowElement(windowId, appInfo);
    this.element.appendChild(windowEl);
    this.windows.set(windowId, windowEl);
    this.makeWindowDraggable(windowEl);
  }

  createWindowElement(id, appInfo) {
    const div = document.createElement('div');
    div.className = 'gaia-window';
    div.id = id;
    div.innerHTML = `
      <div class="window-header">
        <h3 class="window-title">${appInfo.title || 'Application'}</h3>
        <div class="window-controls">
          <button class="window-minimize">_</button>
          <button class="window-maximize">□</button>
          <button class="window-close">✕</button>
        </div>
      </div>
      <div class="window-content"></div>
    `;
    return div;
  }

  makeWindowDraggable(windowEl) {
    const header = windowEl.querySelector('.window-header');
    let pos1 = 0, pos2 = 0, pos3 = 0, pos4 = 0;

    header.addEventListener('mousedown', (e) => {
      pos3 = e.clientX;
      pos4 = e.clientY;

      const drag = (e) => {
        pos1 = pos3 - e.clientX;
        pos2 = pos4 - e.clientY;
        pos3 = e.clientX;
        pos4 = e.clientY;

        windowEl.style.top = (windowEl.offsetTop - pos2) + 'px';
        windowEl.style.left = (windowEl.offsetLeft - pos1) + 'px';
      };

      document.addEventListener('mousemove', drag);
      document.addEventListener('mouseup', () => {
        document.removeEventListener('mousemove', drag);
      });
    });
  }

  focusApp(appId) {
    const windows = Array.from(this.windows.values());
    windows.forEach(w => w.style.zIndex = this.zIndex++);
  }

  async destroy() {}
}

export default WindowManager;