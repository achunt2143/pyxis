/**
 * GeckoOS System API Bridge
 *
 * Provides Gaia web apps access to system services through Echo IPC bus.
 * Similar to Firefox OS Open Web APIs and webOS Luna SysCalls.
 *
 * Injected into privileged Gaia context by GeckoOS extension.
 */

class GeckoOSAPIBridge {
    constructor() {
        this.requestId = 0;
        this.pendingRequests = new Map();
        this.eventListeners = new Map();
        this.initialized = false;
        this.messagePort = null;

        // Bind methods to preserve 'this' context
        this.handleMessage = this.handleMessage.bind(this);
        this.handleError = this.handleError.bind(this);
    }

    /**
     * Initialize the bridge by establishing connection to extension
     */
    async init() {
        return new Promise((resolve, reject) => {
            try {
                // Create a one-time listener for handshake ONLY
                const handshakeListener = (event) => {
                    if (event.source !== window) return;
                    if (event.data.type === 'GECKOOSPBRIDGE_INIT_RESPONSE') {
                        console.log('[GeckoOS Bridge] ✓ Got GECKOOSPBRIDGE_INIT_RESPONSE');
                        
                        // Remove the temporary handshake listener
                        window.removeEventListener('message', handshakeListener);
                        
                        // NOW register the persistent message handler with proper binding
                        window.addEventListener('message', (event) => {
                            try {
                                this.handleMessage(event);
                            } catch (error) {
                                console.error('[GeckoOS Bridge] Error in handleMessage:', error);
                            }
                        });
                        
                        this.initialized = true;
                        console.log('[GeckoOS Bridge] ✓ Message listener registered, initialization complete');
                        resolve();
                    }
                };

                // Register the temporary handshake listener
                window.addEventListener('message', handshakeListener);
                window.addEventListener('error', (event) => this.handleError(event.error));

                // Request initialization from extension
                console.log('[GeckoOS Bridge] Sending GECKOOSPBRIDGE_INIT_REQUEST to extension');
                window.postMessage({
                    type: 'GECKOOSPBRIDGE_INIT_REQUEST',
                    source: 'gaia'
                }, '*');

                // Timeout after 5 seconds
                setTimeout(() => {
                    if (!this.initialized) {
                        window.removeEventListener('message', handshakeListener);
                        reject(new Error('GeckoOS bridge initialization timeout'));
                    }
                }, 5000);

            } catch (error) {
                reject(error);
                console.error('✗ Failed to initialize GeckoOS API bridge:', error);
                this.handleError(error);
            }
        });
    }

    /**
     * Call a service method through Echo IPC
     * @param {string} service - Service name (e.g., 'filesystem')
     * @param {string} method - Method name (e.g., 'readDirectory')
     * @param {object} params - Method parameters
     * @returns {Promise} Promise that resolves with service response
     */
    async callService(service, method, params = {}) {
        if (!this.initialized) {
            throw new Error('GeckoOS bridge not initialized. Call init() first.');
        }

        const requestId = String(++this.requestId);
        const message = {
            type: 'GECKOOSPBRIDGE_CALL',
            requestId: requestId,
            service: service,
            method: method,
            params: params,
            timestamp: Date.now()
        };

        console.log(`[GeckoOS Bridge] Calling service: ${service}.${method} (requestId: ${requestId})`);

        return new Promise((resolve, reject) => {
            // Store pending request with timeout
            const timeoutId = setTimeout(() => {
                if (this.pendingRequests.has(requestId)) {
                    this.pendingRequests.delete(requestId);
                    const errorMsg = `GeckoOS service call timeout (${service}.${method})`;
                    console.error(`[GeckoOS Bridge] ✗ ${errorMsg}`);
                    reject(new Error(errorMsg));
                }
            }, 30000); // 30 second timeout for service calls

            this.pendingRequests.set(requestId, {
                resolve,
                reject,
                timeoutId,
                service,
                method,
                startTime: Date.now()
            });

            // Send message to extension
            console.log(`[GeckoOS Bridge] Posting message to extension:`, message);
            window.postMessage(message, '*');
        });
    }

    /**
     * Handle messages from extension (service responses)
     */
    handleMessage(event) {
        // Security check: only accept messages from window itself
        if (event.source !== window) {
            console.warn('[GeckoOS Bridge] Received message from non-window source, ignoring');
            return;
        }

        const { data } = event;

        if (data.type === 'GECKOOSPBRIDGE_CALL') {
    console.log('[GeckoOS Bridge] Filtering out echo of our own GECKOOSPBRIDGE_CALL message');
    return;
}

        // Validate data exists
        if (!data || typeof data !== 'object') {
            console.warn('[GeckoOS Bridge] Received invalid message data (not an object)');
            return;
        }

        console.log(`[GeckoOS Bridge] Received message of type: ${data.type}`);

        // Handle service response
        if (data.type === 'GECKOOSPBRIDGE_RESPONSE') {
            const { requestId, result, error } = data;
            console.log(`[GeckoOS Bridge] Processing GECKOOSPBRIDGE_RESPONSE for requestId: ${requestId}`);
            console.log('data', data);
            
            const pending = this.pendingRequests.get(requestId);
            
            if (pending) {
                const duration = Date.now() - pending.startTime;
                clearTimeout(pending.timeoutId);
                this.pendingRequests.delete(requestId);
                
                if (error) {
                    console.error(`[GeckoOS Bridge] ✗ Service error for ${pending.service}.${pending.method}: ${error}`);
                    pending.reject(new Error(error));
                } else {
                    console.log(`[GeckoOS Bridge] ✓ Service success for ${pending.service}.${pending.method} (${duration}ms)`);
                    pending.resolve(result);
                }
            } else {
                console.warn(`[GeckoOS Bridge] ⚠️ Received response for unknown requestId: ${requestId}`);
            }
            return;
        }

        // Handle service events/signals
        if (data.type === 'GECKOOSPBRIDGE_EVENT') {
            const { service, method: eventName, data: eventData } = data;
            console.log(`[GeckoOS Bridge] Event from ${service}: ${eventName}`);
            
            const listeners = this.eventListeners.get(`${service}:${eventName}`) || [];
            if (listeners.length > 0) {
                listeners.forEach(callback => {
                    try {
                        callback(eventData);
                    } catch (error) {
                        console.error(`Error in GeckoOS event listener for ${service}:${eventName}:`, error);
                    }
                });
            } else {
                console.log(`[GeckoOS Bridge] No listeners registered for ${service}:${eventName}`);
            }
            return;
        }

        // Handle status messages
        if (data.type === 'GECKOOSPBRIDGE_STATUS') {
            console.log(`[GeckoOS Bridge] Status update:`, data);
            return;
        }

        // Unknown message type
        if (data.type && data.type !== 'GECKOOSPBRIDGE_INIT_RESPONSE') {
            console.warn(`[GeckoOS Bridge] Unknown message type: ${data.type}`);
        }
    }

    /**
     * Handle errors from service calls
     */
    handleError(error) {
        console.error('[GeckoOS Bridge] Error:', error);
        
        // Dispatch error event for apps that want to listen
        window.dispatchEvent(new CustomEvent('geckoOSError', {
            detail: {
                message: error.message || String(error),
                timestamp: Date.now()
            }
        }));
    }

    /**
     * Register listener for service events
     */
    addEventListener(service, eventName, callback) {
        const key = `${service}:${eventName}`;
        if (!this.eventListeners.has(key)) {
            this.eventListeners.set(key, []);
        }

        this.eventListeners.get(key).push(callback);
        console.log(`[GeckoOS Bridge] Registered listener for ${key}`);

        // Return unsubscribe function
        return () => {
            const listeners = this.eventListeners.get(key);
            const index = listeners.indexOf(callback);
            if (index > -1) {
                listeners.splice(index, 1);
                console.log(`[GeckoOS Bridge] Unregistered listener for ${key}`);
            }
        };
    }

    // ==================== API DEFINITIONS ====================

    /**
     * FILESYSTEM API
     */
    get filesystem() {
        return {
            readDirectory: async (path) => this.callService('filesystem', 'readDirectory', { path }),
            readFile: async (path, encoding = 'utf-8') => this.callService('filesystem', 'readFile', { path, encoding }),
            writeFile: async (path, content, encoding = 'utf-8') => this.callService('filesystem', 'writeFile', { path, content, encoding }),
            appendFile: async (path, content, encoding = 'utf-8') => this.callService('filesystem', 'appendFile', { path, content, encoding }),
            deleteFile: async (path) => this.callService('filesystem', 'deleteFile', { path }),
            deleteDirectory: async (path, recursive = false) => this.callService('filesystem', 'deleteDirectory', { path, recursive }),
            createDirectory: async (path, mode = 0o755) => this.callService('filesystem', 'createDirectory', { path, mode }),
            exists: async (path) => this.callService('filesystem', 'exists', { path }),
            stat: async (path) => this.callService('filesystem', 'stat', { path }),
            copy: async (source, destination) => this.callService('filesystem', 'copy', { source, destination }),
            move: async (source, destination) => this.callService('filesystem', 'move', { source, destination }),
            chmod: async (path, mode) => this.callService('filesystem', 'chmod', { path, mode }),
            watch: (path, callback) => this.addEventListener('filesystem', `watch:${path}`, callback)
        };
    }

    /**
     * APP MANAGER API
     */
    get appManager() {
        return {
            launch: async (appId, params = {}) => this.callService('appManager', 'launch', { appId, params }),
            close: async (appId) => this.callService('appManager', 'close', { appId }),
            list: async () => this.callService('appManager', 'list', {}),
            getInfo: async (appId) => this.callService('appManager', 'getInfo', { appId }),
            install: async (appPath) => this.callService('appManager', 'install', { appPath }),
            uninstall: async (appId) => this.callService('appManager', 'uninstall', { appId }),
            kill: async (appId) => this.callService('appManager', 'kill', { appId }),
            restart: async (appId) => this.callService('appManager', 'restart', { appId }),
            setActive: async (appId) => this.callService('appManager', 'setActive', { appId }),
            getActive: async () => this.callService('appManager', 'getActive', {}),
            onLaunched: (callback) => this.addEventListener('appManager', 'launched', callback),
            onClosed: (callback) => this.addEventListener('appManager', 'closed', callback),
            onInstalled: (callback) => this.addEventListener('appManager', 'installed', callback)
        };
    }

    /**
     * POWER API
     */
    get power() {
        return {
            shutdown: async () => this.callService('power', 'shutdown', {}),
            reboot: async () => this.callService('power', 'reboot', {}),
            sleep: async () => this.callService('power', 'sleep', {}),
            wake: async () => this.callService('power', 'wake', {}),
            getBatteryStatus: async () => this.callService('power', 'getBatteryStatus', {}),
            setScreenBrightness: async (brightness) => this.callService('power', 'setScreenBrightness', { brightness }),
            getScreenBrightness: async () => this.callService('power', 'getScreenBrightness', {}),
            setScreenTimeout: async (timeout) => this.callService('power', 'setScreenTimeout', { timeout }),
            getScreenTimeout: async () => this.callService('power', 'getScreenTimeout', {}),
            onBatteryLow: (callback) => this.addEventListener('power', 'batteryLow', callback),
            onPowerStateChanged: (callback) => this.addEventListener('power', 'powerStateChanged', callback)
        };
    }

    /**
     * SETTINGS API
     */
    get settings() {
        return {
            get: async (key) => this.callService('settings', 'get', { key }),
            set: async (key, value) => this.callService('settings', 'set', { key, value }),
            getAll: async () => this.callService('settings', 'getAll', {}),
            delete: async (key) => this.callService('settings', 'delete', { key }),
            clear: async () => this.callService('settings', 'clear', {}),
            reset: async () => this.callService('settings', 'reset', {}),
            onChange: (key, callback) => this.addEventListener('settings', `change:${key}`, callback)
        };
    }

    /**
     * WINDOW MANAGER API
     */
    get windowManager() {
        return {
            open: async (url, options = {}) => this.callService('windowManager', 'open', { url, options }),
            close: async (windowId) => this.callService('windowManager', 'close', { windowId }),
            focus: async (windowId) => this.callService('windowManager', 'focus', { windowId }),
            blur: async (windowId) => this.callService('windowManager', 'blur', { windowId }),
            maximize: async (windowId) => this.callService('windowManager', 'maximize', { windowId }),
            minimize: async (windowId) => this.callService('windowManager', 'minimize', { windowId }),
            restore: async (windowId) => this.callService('windowManager', 'restore', { windowId }),
            setSize: async (windowId, width, height) => this.callService('windowManager', 'setSize', { windowId, width, height }),
            setPosition: async (windowId, x, y) => this.callService('windowManager', 'setPosition', { windowId, x, y }),
            list: async () => this.callService('windowManager', 'list', {}),
            onWindowOpened: (callback) => this.addEventListener('windowManager', 'windowOpened', callback),
            onWindowClosed: (callback) => this.addEventListener('windowManager', 'windowClosed', callback)
        };
    }

    /**
     * NETWORK API
     */
    get network() {
        return {
            getStatus: async () => this.callService('network', 'getStatus', {}),
            getType: async () => this.callService('network', 'getType', {}),
            getInterfaces: async () => this.callService('network', 'getInterfaces', {}),
            getIPAddress: async () => this.callService('network', 'getIPAddress', {}),
            getGateway: async () => this.callService('network', 'getGateway', {}),
            getDNS: async () => this.callService('network', 'getDNS', {}),
            onStatusChanged: (callback) => this.addEventListener('network', 'statusChanged', callback),
            onTypeChanged: (callback) => this.addEventListener('network', 'typeChanged', callback)
        };
    }

    /**
     * DEVICE INFO API
     */
    get deviceInfo() {
        return {
            getOS: async () => this.callService('deviceInfo', 'getOS', {}),
            getKernel: async () => this.callService('deviceInfo', 'getKernel', {}),
            getModel: async () => this.callService('deviceInfo', 'getModel', {}),
            getManufacturer: async () => this.callService('deviceInfo', 'getManufacturer', {}),
            getSerial: async () => this.callService('deviceInfo', 'getSerial', {}),
            getHardware: async () => this.callService('deviceInfo', 'getHardware', {}),
            getUUID: async () => this.callService('deviceInfo', 'getUUID', {}),
            getHostname: async () => this.callService('deviceInfo', 'getHostname', {}),
            getMemoryInfo: async () => this.callService('deviceInfo', 'getMemoryInfo', {}),
            getStorageInfo: async () => this.callService('deviceInfo', 'getStorageInfo', {}),
            getCPUInfo: async () => this.callService('deviceInfo', 'getCPUInfo', {})
        };
    }

    /**
     * NOTIFICATION API
     */
    get notification() {
        return {
            show: async (title, options = {}) => this.callService('notification', 'show', { title, options }),
            close: async (notificationId) => this.callService('notification', 'close', { notificationId }),
            closeAll: async () => this.callService('notification', 'closeAll', {}),
            onClicked: (notificationId, callback) => this.addEventListener('notification', `clicked:${notificationId}`, callback),
            onClosed: (notificationId, callback) => this.addEventListener('notification', `closed:${notificationId}`, callback)
        };
    }

    /**
     * PROCESS API
     */
    get process() {
        return {
            spawn: async (command, args = [], options = {}) => this.callService('process', 'spawn', { command, args, options }),
            kill: async (pid) => this.callService('process', 'kill', { pid }),
            killGroup: async (pgid) => this.callService('process', 'killGroup', { pgid }),
            list: async () => this.callService('process', 'list', {}),
            getInfo: async (pid) => this.callService('process', 'getInfo', { pid }),
            onProcessSpawned: (callback) => this.addEventListener('process', 'spawned', callback),
            onProcessTerminated: (callback) => this.addEventListener('process', 'terminated', callback)
        };
    }

    /**
     * Get system info
     */
    async getSystemInfo() {
        return this.callService('system', 'getInfo', {});
    }

    /**
     * List all available services
     */
    async listServices() {
        return this.callService('system', 'listServices', {});
    }
}

// Create global instance
const geckoOS = new GeckoOSAPIBridge();

// Auto-initialize when script loads
window.addEventListener('load', async () => {
    try {
        console.log('[GeckoOS Bridge] Starting initialization...');
        await geckoOS.init();
        console.log('✓ GeckoOS API bridge initialized successfully');

        // Dispatch custom event for apps that want to know when it's ready
        window.dispatchEvent(new CustomEvent('geckoOSReady', { detail: geckoOS }));
    } catch (error) {
        console.error('✗ Failed to initialize GeckoOS API bridge:', error);
    }
});

// Expose globally
window.navigator.geckoOS = geckoOS;
window.geckoOS = geckoOS;

// Also support CommonJS exports
if (typeof module !== 'undefined' && module.exports) {
    module.exports = geckoOS;
}
