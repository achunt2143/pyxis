/**
 * Tiling Component
 * File: gaia/static/modules/window-manager/tiling.js
 * Purpose: Window tiling and layout management
 */
class TilingManager {
  constructor(container) {
    this.container = container;
    this.windows = [];
    this.layout = 'grid'; // grid, horizontal, vertical
  }

  addWindow(window) {
    this.windows.push(window);
    this.applyLayout();
  }

  removeWindow(windowId) {
    this.windows = this.windows.filter(w => w.id !== windowId);
    this.applyLayout();
  }

  applyLayout() {
    const containerWidth = this.container.offsetWidth;
    const containerHeight = this.container.offsetHeight;
    const count = this.windows.length;

    if (count === 0) return;

    switch (this.layout) {
      case 'grid':
        this.applyGridLayout(containerWidth, containerHeight, count);
        break;
      case 'horizontal':
        this.applyHorizontalLayout(containerWidth, containerHeight, count);
        break;
      case 'vertical':
        this.applyVerticalLayout(containerWidth, containerHeight, count);
        break;
    }
  }

  applyGridLayout(width, height, count) {
    const cols = Math.ceil(Math.sqrt(count));
    const rows = Math.ceil(count / cols);
    const winWidth = width / cols;
    const winHeight = height / rows;

    this.windows.forEach((window, index) => {
      const col = index % cols;
      const row = Math.floor(index / cols);

      if (window.element) {
        window.element.style.width = `${winWidth}px`;
        window.element.style.height = `${winHeight}px`;
        window.element.style.left = `${col * winWidth}px`;
        window.element.style.top = `${row * winHeight}px`;
      }
    });
  }

  applyHorizontalLayout(width, height, count) {
    const winWidth = width / count;

    this.windows.forEach((window, index) => {
      if (window.element) {
        window.element.style.width = `${winWidth}px`;
        window.element.style.height = `${height}px`;
        window.element.style.left = `${index * winWidth}px`;
        window.element.style.top = '0px';
      }
    });
  }

  applyVerticalLayout(width, height, count) {
    const winHeight = height / count;

    this.windows.forEach((window, index) => {
      if (window.element) {
        window.element.style.width = `${width}px`;
        window.element.style.height = `${winHeight}px`;
        window.element.style.left = '0px';
        window.element.style.top = `${index * winHeight}px`;
      }
    });
  }

  setLayout(layoutType) {
    this.layout = layoutType;
    this.applyLayout();
  }
}

export default TilingManager;