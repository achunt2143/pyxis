/**
 * File Operations Component
 * File: gaia/static/modules/file-manager/file-operations.js
 */
class FileOperations {
  static async copy(from, to) {
    console.log(`Copying ${from} to ${to}`);
  }

  static async move(from, to) {
    console.log(`Moving ${from} to ${to}`);
  }

  static async delete(path) {
    console.log(`Deleting ${path}`);
  }
}
export default FileOperations;