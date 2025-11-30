/**
 * Gaia Constants
 * File: gaia/static/utils/constants.js
 * Purpose: Application-wide constants and configuration
 */

const GAIA_CONSTANTS = {
  APP_NAME: 'Gaia Shell v2.0',
  APP_VERSION: '2.0.0',
  
  // API Configuration
  API: {
    BASE_URL: 'http://localhost:3000/api',
    TIMEOUT: 10000,
    RETRY_ATTEMPTS: 3
  },

  // Echo IPC Configuration
  ECHO: {
    SOCKET_URL: 'ws://localhost:8080',
    NAMESPACE: 'org.geckoOS'
  },

  // UI Configuration
  UI: {
    TASKBAR_HEIGHT: 48,
    SIDEBAR_WIDTH: 60,
    WINDOW_MIN_WIDTH: 300,
    WINDOW_MIN_HEIGHT: 200,
    MODAL_MAX_WIDTH: 600
  },

  // Performance
  PERFORMANCE: {
    TARGET_FPS: 60,
    CACHE_MAX_SIZE: 100,
    CACHE_TTL: 5000
  },

  // Color Palette
  COLORS: {
    PRIMARY: '#208094',
    SECONDARY: '#13575f',
    ACCENT: '#2da5c9',
    SUCCESS: '#27ae60',
    DANGER: '#e74c3c',
    WARNING: '#f39c12',
    INFO: '#3498db'
  },

  // Keyboard Shortcuts
  SHORTCUTS: {
    TERMINAL: { ctrl: true, alt: true, key: 't' },
    FILE_MANAGER: { ctrl: true, alt: true, key: 'f' },
    SETTINGS: { ctrl: true, alt: true, key: 's' },
    SWITCH_WINDOW: { alt: true, key: 'Tab' }
  },

  // Storage Keys
  STORAGE: {
    THEME: 'gaia-theme',
    LAYOUT: 'gaia-layout',
    PREFERENCES: 'gaia-preferences'
  }
};

export default GAIA_CONSTANTS;