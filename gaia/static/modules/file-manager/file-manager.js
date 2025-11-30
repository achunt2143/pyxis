/**
 * File Manager Component
 * File: gaia/static/modules/file-manager/file-manager.js
 */

import { ModuleLoader } from '../../scripts/module-loader.js';

class FileManager {
  constructor(options) {
    this.element = options.element;
    this.services = options.services;
    this.currentPath = '/home/user';
    this.files = [];
  }

  async initialize() {
    await ModuleLoader.loadStyles('file-manager');
    this.createStructure();
    await this.loadDirectory();
  }

  createStructure() {
    this.element.innerHTML = `
      <div class="file-manager-container">
        <div class="file-toolbar" id="file-toolbar"></div>
        <div class="breadcrumb" id="breadcrumb"></div>
        <div class="file-list" id="file-list"></div>
        <div class="file-preview" id="file-preview"></div>
      </div>
    `;
  }

  async loadDirectory() {
    try {
      const fileService = this.services.get('file-service');
      this.files = await fileService.listDirectory(this.currentPath);
      this.renderFiles();
    } catch (error) {
      console.error('Failed to load directory:', error);
    }
  }

  renderFiles() {
    const listEl = document.getElementById('file-list');
    listEl.innerHTML = this.files.map(file => `
      <div class="file-item" data-path="${file.path}">
        <span class="file-icon">${file.isDirectory ? '📁' : '📄'}</span>
        <span class="file-name">${file.name}</span>
        <span class="file-size">${file.size}</span>
      </div>
    `).join('');
  }

  async destroy() {}
}
export default FileManager;