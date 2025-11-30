/**
 * Window Component
 * File: gaia/static/modules/window-manager/window.js
 * Purpose: Individual window management
 */
class Window {
  constructor(options) {
    this.id = options.id;
    this.title = options.title;
    this.width = options.width || 600;
    this.height = options.height || 400;
    this.x = options.x || 100;
    this.y = options.y || 100;
    this.minimized = false;
    this.maximized = false;
    this.zIndex = options.zIndex || 1000;
    this.element = null;
  }

  create() {
    this.element = document.createElement('div');
    this.element.id = this.id;
    this.element.className = 'gaia-window';
    this.element.style.width = `${this.width}px`;
    this.element.style.height = `${this.height}px`;
    this.element.style.left = `${this.x}px`;
    this.element.style.top = `${this.y}px`;
    this.element.style.zIndex = this.zIndex;

    this.element.innerHTML = `
      <div class="window-header">
        <h3 class="window-title">${this.title}</h3>
        <div class="window-controls">
          <button class="window-minimize" data-window-id="${this.id}">_</button>
          <button class="window-maximize" data-window-id="${this.id}">□</button>
          <button class="window-close" data-window-id="${this.id}">✕</button>
        </div>
      </div>
      <div class="window-content"></div>
    `;

    this.attachEventListeners();
    return this.element;
  }

  attachEventListeners() {
    const header = this.element.querySelector('.window-header');
    const minimizeBtn = this.element.querySelector('.window-minimize');
    const maximizeBtn = this.element.querySelector('.window-maximize');
    const closeBtn = this.element.querySelector('.window-close');

    minimizeBtn.addEventListener('click', () => this.minimize());
    maximizeBtn.addEventListener('click', () => this.maximize());
    closeBtn.addEventListener('click', () => this.close());

    // Drag functionality
    header.addEventListener('mousedown', (e) => this.startDrag(e));
  }

  startDrag(e) {
    const pos3 = e.clientX;
    const pos4 = e.clientY;

    const drag = (e) => {
      const pos1 = pos3 - e.clientX;
      const pos2 = pos4 - e.clientY;

      this.element.style.top = (this.element.offsetTop - pos2) + 'px';
      this.element.style.left = (this.element.offsetLeft - pos1) + 'px';
    };

    document.addEventListener('mousemove', drag);
    document.addEventListener('mouseup', () => {
      document.removeEventListener('mousemove', drag);
    });
  }

  minimize() {
    this.minimized = !this.minimized;
    this.element.style.display = this.minimized ? 'none' : 'flex';
  }

  maximize() {
    this.maximized = !this.maximized;
    if (this.maximized) {
      this.element.style.width = '100%';
      this.element.style.height = '100%';
      this.element.style.top = '0';
      this.element.style.left = '0';
    } else {
      this.element.style.width = `${this.width}px`;
      this.element.style.height = `${this.height}px`;
      this.element.style.top = `${this.y}px`;
      this.element.style.left = `${this.x}px`;
    }
  }

  close() {
    this.element.remove();
    window.dispatchEvent(new CustomEvent('window:closed', {
      detail: { windowId: this.id }
    }));
  }

  setContent(content) {
    const contentEl = this.element.querySelector('.window-content');
    contentEl.innerHTML = content;
  }

  focus() {
    this.element.style.zIndex = this.zIndex++;
  }
}

export default Window;