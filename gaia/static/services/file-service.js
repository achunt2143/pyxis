/**
 * Gaia File Service
 * File: gaia/static/services/file-service.js
 * Purpose: File operations and directory management
 */

class FileService {
  constructor(apiService) {
    this.api = apiService;
  }

  /**
   * List directory contents
   */
  async listDirectory(path) {
    return this.api.get(`/files/list`, { path });
  }

  /**
   * Read file
   */
  async readFile(path) {
    return this.api.get(`/files/read`, { path });
  }

  /**
   * Write file
   */
  async writeFile(path, content) {
    return this.api.post(`/files/write`, { path, content });
  }

  /**
   * Delete file
   */
  async deleteFile(path) {
    return this.api.delete(`/files/${path}`);
  }

  /**
   * Get file info
   */
  async getFileInfo(path) {
    return this.api.get(`/files/info`, { path });
  }

  /**
   * Create directory
   */
  async createDirectory(path) {
    return this.api.post(`/files/mkdir`, { path });
  }

  /**
   * Upload file
   */
  async uploadFile(path, file) {
    return this.api.uploadFile(`/files/upload`, file);
  }
}

export default FileService;