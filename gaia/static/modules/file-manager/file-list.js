/**
 * File List Component
 * File: gaia/static/modules/file-manager/file-list.js
 */
class FileList {
  constructor(options) {
    this.container = options.container;
    this.files = options.files || [];
  }

  render() {
    return this.files.map(f => `<div class="file-row">${f.name}</div>`).join('');
  }
}
export default FileList;