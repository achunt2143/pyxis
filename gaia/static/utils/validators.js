/**
 * Gaia Validators
 * File: gaia/static/utils/validators.js
 * Purpose: Input validation and data verification
 */

const GaiaValidators = {
  /**
   * Validate email
   */
  isEmail(email) {
    const regex = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;
    return regex.test(email);
  },

  /**
   * Validate URL
   */
  isUrl(url) {
    try {
      new URL(url);
      return true;
    } catch {
      return false;
    }
  },

  /**
   * Validate file path
   */
  isValidPath(path) {
    return /^[a-zA-Z0-9.\/_-]+$/.test(path);
  },

  /**
   * Validate file name
   */
  isValidFilename(filename) {
    return /^[a-zA-Z0-9._-]+$/.test(filename) && filename.length > 0;
  },

  /**
   * Validate number
   */
  isNumber(value) {
    return !isNaN(parseFloat(value)) && isFinite(value);
  },

  /**
   * Validate required field
   */
  isRequired(value) {
    return value !== null && value !== undefined && value !== '';
  },

  /**
   * Validate minimum length
   */
  minLength(value, length) {
    return value && value.length >= length;
  },

  /**
   * Validate maximum length
   */
  maxLength(value, length) {
    return !value || value.length <= length;
  },

  /**
   * Validate file size
   */
  isFileSizeValid(size, maxMB = 50) {
    return size <= maxMB * 1024 * 1024;
  },

  /**
   * Validate file type
   */
  isFileTypeValid(filename, allowedExtensions) {
    const ext = filename.split('.').pop().toLowerCase();
    return allowedExtensions.includes(ext);
  }
};

export default GaiaValidators;