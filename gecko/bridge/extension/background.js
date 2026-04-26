/**
 * GeckoOS Extension - Background Script (FIXED v2)
 * 
 * Properly handles native messaging protocol with correct message flow
 */

console.log('[GeckoOS Background] Background script loaded');

// Configuration
const CONFIG = {
    NATIVE_HOST: 'com.geckoOS.extension',
    CONNECT_TIMEOUT: 10000,
    CALL_TIMEOUT: 30000,
    MAX_RETRIES: 5,
    RETRY_DELAY: 2000
};

/**
 * Native messaging bridge to communicate with GeckoOS daemon
 */
class NativeMessagingBridge {
    constructor() {
        this.connected = false;
        this.connecting = false;
        this.pendingRequests = new Map();
        this.requestId = 0;
        this.port = null;
        this.retryCount = 0;
        this.eventListeners = new Map();
        this.handshakeResolved = false;

        // Bind methods
        this.handleNativeMessage = this.handleNativeMessage.bind(this);
        this.handleNativeDisconnect = this.handleNativeDisconnect.bind(this);
    }

    /**
     * Initialize connection to native host
     */
    async initialize() {
        if (this.connecting || this.connected) {
            console.log('[GeckoOS Background] Already connected or connecting');
            return;
        }

        this.connecting = true;
        this.retryCount = 0;

        try {
            console.log('[GeckoOS Background] Initializing native host connection...');
            await this.connectToNativeHost();
            this.connected = true;
            this.connecting = false;
            console.log('[GeckoOS Background] ✓ Successfully connected to native host');

            // Broadcast connection status
            this.broadcastToAllTabs({
                type: 'GECKOOSPBRIDGE_STATUS',
                status: 'connected'
            });
        } catch (error) {
            this.connecting = false;
            console.error('[GeckoOS Background] Failed to initialize native host:', error);

            // Retry connection with exponential backoff
            if (this.retryCount < CONFIG.MAX_RETRIES) {
                this.retryCount++;
                const delay = CONFIG.RETRY_DELAY * this.retryCount;
                console.log(`[GeckoOS Background] Retrying connection in ${delay}ms (attempt ${this.retryCount}/${CONFIG.MAX_RETRIES})`);
                setTimeout(() => {
                    this.initialize();
                }, delay);
            } else {
                console.error('[GeckoOS Background] Max connection retries exceeded');
                this.broadcastToAllTabs({
                    type: 'GECKOOSPBRIDGE_STATUS',
                    status: 'disconnected',
                    error: error.message
                });
            }
        }
    }

    /**
     * Connect to native host via browser.runtime.connectNative
     */
    async connectToNativeHost() {
        return new Promise((resolve, reject) => {
            try {
                console.log(`[GeckoOS Background] Connecting to native host: ${CONFIG.NATIVE_HOST}`);

                // Use connectNative for persistent connection
                this.port = browser.runtime.connectNative(CONFIG.NATIVE_HOST);

                if (!this.port) {
                    throw new Error('Failed to create native port');
                }

                console.log('[GeckoOS Background] Native port created successfully');

                // Setup disconnect listener FIRST
                this.port.onDisconnect.addListener(this.handleNativeDisconnect);
                console.log('[GeckoOS Background] Disconnect listener attached');

                // Setup message listener BEFORE sending handshake
                this.port.onMessage.addListener(this.handleNativeMessage);
                console.log('[GeckoOS Background] Permanent message listener attached');

                // Set timeout for handshake
                const handshakeTimeout = setTimeout(() => {
                    if (!this.handshakeResolved) {
                        reject(new Error('Native host handshake timeout'));
                    }
                }, CONFIG.CONNECT_TIMEOUT);

                // Store timeout ID for later cleanup
                this._handshakeTimeout = handshakeTimeout;

                console.log('[GeckoOS Background] Sending handshake to native host...');

                // Send handshake
                this.port.postMessage({
                    type: 'HANDSHAKE',
                    requestId: `handshake_${Date.now()}`,
                    version: '1.0.0',
                    timestamp: Date.now()
                });

                // Wait for response in handleNativeMessage
                // Once we get HANDSHAKEACK, we resolve this promise
                
                // Store resolve/reject for use in handleNativeMessage
                this._connectResolve = () => {
                    clearTimeout(handshakeTimeout);
                    this.handshakeResolved = true;
                    resolve();
                };

                this._connectReject = (err) => {
                    clearTimeout(handshakeTimeout);
                    reject(err);
                };

            } catch (error) {
                console.error('[GeckoOS Background] Connection error:', error);
                reject(error);
            }
        });
    }

    /**
     * Handle incoming messages from native host
     */
    handleNativeMessage(message) {
        console.log('[GeckoOS Background] [RCV]', message.type, JSON.stringify(message, null, 2));

        // Handle handshake ack
        if (message.type === 'HANDSHAKEACK') {
            console.log('[GeckoOS Background] ✓ Native host handshake acknowledged');
            if (this._connectResolve) {
                this._connectResolve();
                delete this._connectResolve;
                delete this._connectReject;
            }
            return;
        }

        // Handle service responses
        if (message.type === 'SUCCESS' || message.type === 'ERROR') {
            const requestId = String(message.request_id ?? message.requestId);
            console.log(`[GeckoOS Background] Service response for requestId: ${requestId}`);

            const pending = this.pendingRequests.get(requestId);
            if (pending) {
                clearTimeout(pending.timeoutId);
                this.pendingRequests.delete(requestId);

                if (message.type === 'ERROR') {
                    console.error(`[GeckoOS Background] ✗ Service error: ${message.error}`);
                    pending.reject(new Error(message.error || 'Unknown error'));
                } else {
                    console.log(`[GeckoOS Background] ✓ Service success`);
                    pending.resolve(message.result);
                }
            } else {
                console.warn(`[GeckoOS Background] ⚠️ Received response for unknown requestId: ${requestId}`);
            }
            return;
        }

        // Handle PONG (keep-alive response)
        if (message.type === 'PONG') {
            console.log('[GeckoOS Background] PONG received (connection alive)');
            return;
        }

        console.warn(`[GeckoOS Background] Unknown message type: ${message.type}`);
    }

    /**
     * Handle native host disconnect
     */
    handleNativeDisconnect() {
        console.error('[GeckoOS Background] Native host disconnected');
        this.connected = false;
        this.port = null;

        // Reject any pending requests
        for (const [requestId, pending] of this.pendingRequests) {
            clearTimeout(pending.timeoutId);
            pending.reject(new Error('Native host disconnected'));
        }
        this.pendingRequests.clear();

        // If we were still trying to connect, reject
        if (this._connectReject && !this.handshakeResolved) {
            this._connectReject(new Error('Native host disconnected during handshake'));
            delete this._connectResolve;
            delete this._connectReject;
        }

        // Broadcast disconnection
        this.broadcastToAllTabs({
            type: 'GECKOOSPBRIDGE_STATUS',
            status: 'disconnected'
        });

        // Try to reconnect
        setTimeout(() => {
            this.initialize();
        }, 5000);
    }

    /**
     * Call a service through the native host
     */
    async callService(service, method, params, requestId) {
        try {
            if (!this.connected) {
                throw new Error('Native host not connected');
            }

            const message = {
                type: 'ECHOSERVICECALL',
                request_id: requestId,
                service: service,
                method: method,
                params: params,
                timestamp: Date.now()
            };

            console.log(`[GeckoOS Background] Sending service call: ${service}.${method} (requestId: ${requestId})`);

            return new Promise((resolve, reject) => {
                const timeoutId = setTimeout(() => {
                    if (this.pendingRequests.has(requestId)) {
                        this.pendingRequests.delete(requestId);
                        const errorMsg = `Service call timeout (${service}.${method})`;
                        console.error(`[GeckoOS Background] ✗ ${errorMsg}`);
                        reject(new Error(errorMsg));
                    }
                }, CONFIG.CALL_TIMEOUT);

                this.pendingRequests.set(requestId, {
                    resolve,
                    reject,
                    timeoutId,
                    service,
                    method,
                    startTime: Date.now()
                });

                try {
                    this.port.postMessage(message);
                } catch (error) {
                    clearTimeout(timeoutId);
                    this.pendingRequests.delete(requestId);
                    reject(error);
                }
            });
        } catch (error) {
            console.error(`[GeckoOS Background] Failed to call service: ${error.message}`);
            return {
                requestId: requestId,
                result: null,
                error: error.message
            };
        }
    }

    /**
     * Broadcast message to all tabs
     */
    broadcastToAllTabs(message) {
        browser.tabs.query({}, (tabs) => {
            tabs.forEach(tab => {
                try {
                    browser.tabs.sendMessage(tab.id, message).catch(error => {
                        // Tab might not have content script loaded, ignore
                    });
                } catch (error) {
                    // Ignore errors for individual tabs
                }
            });
        });
    }

    /**
     * Get connection status
     */
    getStatus() {
        return {
            connected: this.connected,
            connecting: this.connecting,
            pendingRequests: this.pendingRequests.size,
            retryCount: this.retryCount,
            handshakeResolved: this.handshakeResolved
        };
    }
}

// Create global native bridge instance
const nativeBridge = new NativeMessagingBridge();

// Initialize on extension load
nativeBridge.initialize();

/**
 * Handle messages from content script
 */
browser.runtime.onMessage.addListener((message, sender, sendResponse) => {
    console.log(`[GeckoOS Background] [MSG] Received from tab ${sender.tab?.id}: ${message.type}`);

    // Handle bridge status request
    if (message.type === 'GECKOOSPBRIDGE_GET_STATUS') {
        sendResponse({
            type: 'GECKOOSPBRIDGE_STATUS',
            status: nativeBridge.connected ? 'connected' : 'disconnected',
            details: nativeBridge.getStatus()
        });
        return true;
    }

    // Handle service calls
    if (message.type === 'ECHO_SERVICE_CALL') {
        const requestId = String(message.requestId || ++nativeBridge.requestId);
        console.log(`[GeckoOS Background] [MSG] Service call for ${message.service}.${message.method} with requestId: ${requestId}`);

        nativeBridge.callService(
            message.service,
            message.method,
            message.params || {},
            requestId
        ).then(response => {
            console.log(`[GeckoOS Background] [MSG] Service call completed`);

            // Send response back to content script
            browser.tabs.sendMessage(sender.tab.id, {
                type: 'GECKOOSPBRIDGE_RESPONSE',
                requestId: requestId,
                result: response || null,
                error: null
            }).catch(error => {
                console.error('[GeckoOS Background] [MSG] Failed to send response to tab:', error);
            });

        }).catch(error => {
            console.error('[GeckoOS Background] [MSG] Service call error:', error);
            browser.tabs.sendMessage(sender.tab.id, {
                type: 'GECKOOSPBRIDGE_RESPONSE',
                requestId: requestId,
                result: null,
                error: error.message
            }).catch(err => {
                console.error('[GeckoOS Background] [MSG] Failed to send error to tab:', err);
            });
        });

        return false; // Don't use sendResponse callback
    }

    // Handle subscribe requests
    if (message.type === 'ECHO_SUBSCRIBE') {
        sendResponse({ success: true });
        return true;
    }

    // Handle unsubscribe requests
    if (message.type === 'ECHO_UNSUBSCRIBE') {
        console.log(`[GeckoOS Background] Unsubscribing from ${message.service} events`);
        sendResponse({ success: true });
        return true;
    }

    return false;
});

/**
 * Handle extension startup
 */
browser.runtime.onStartup.addListener(() => {
    console.log('[GeckoOS Background] Extension started, initializing native host');
    nativeBridge.initialize();
});

/**
 * Handle extension installation/update
 */
browser.runtime.onInstalled.addListener((details) => {
    if (details.reason === 'install') {
        console.log('[GeckoOS Background] Extension installed');
        nativeBridge.initialize();
    } else if (details.reason === 'update') {
        console.log('[GeckoOS Background] Extension updated');
        nativeBridge.initialize();
    }
});

/**
 * Keep-alive mechanism to maintain connection
 */
setInterval(() => {
    if (nativeBridge.connected && nativeBridge.port) {
        try {
            nativeBridge.port.postMessage({
                type: 'PING',
                timestamp: Date.now()
            });
        } catch (error) {
            console.error('[GeckoOS Background] Keep-alive ping failed:', error);
            nativeBridge.handleNativeDisconnect();
        }
    }
}, 30000); // Every 30 seconds

console.log('[GeckoOS Background] Background script initialized');