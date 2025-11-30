/**
 * Gaia Echo IPC Service
 * File: gaia/static/services/echo-service.js
 * Purpose: Communication with Echo IPC system and other applications
 */

class EchoService {
  constructor(socketUrl = 'ws://localhost:8080') {
    this.socketUrl = socketUrl;
    this.socket = null;
    this.connected = false;
    this.services = new Map();
    this.methods = new Map();
    this.signals = new Map();
    this.requestId = 0;
    this.pendingRequests = new Map();
  }

  /**
   * Connect to Echo IPC system
   */
  async connect() {
    return new Promise((resolve, reject) => {
      try {
        this.socket = new WebSocket(this.socketUrl);
        
        this.socket.onopen = () => {
          this.connected = true;
          console.log('✅ Connected to Echo IPC');
          resolve();
        };
        
        this.socket.onmessage = (event) => this.handleMessage(event);
        this.socket.onerror = (error) => {
          console.error('Echo IPC error:', error);
          reject(error);
        };
        
        this.socket.onclose = () => {
          this.connected = false;
          console.log('Disconnected from Echo IPC');
        };
      } catch (error) {
        reject(error);
      }
    });
  }

  /**
   * Register service with Echo
   */
  async registerService(serviceName, config) {
    return this.call('org.geckoOS.Echo', 'registerService', {
      name: serviceName,
      methods: config.methods || [],
      signals: config.signals || []
    });
  }

  /**
   * Call remote method
   */
  async call(serviceName, method, args = {}) {
    if (!this.connected) {
      throw new Error('Not connected to Echo IPC');
    }

    const requestId = ++this.requestId;
    const message = {
      type: 'call',
      id: requestId,
      service: serviceName,
      method: method,
      args: args
    };

    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        this.pendingRequests.delete(requestId);
        reject(new Error('Request timeout'));
      }, 10000);

      this.pendingRequests.set(requestId, { resolve, reject, timeout });
      this.socket.send(JSON.stringify(message));
    });
  }

  /**
   * Emit signal
   */
  emit(signalName, data) {
    if (!this.connected) return;
    
    const message = {
      type: 'signal',
      name: signalName,
      data: data
    };
    
    this.socket.send(JSON.stringify(message));
  }

  /**
   * Listen to signal
   */
  on(signalName, callback) {
    if (!this.signals.has(signalName)) {
      this.signals.set(signalName, []);
    }
    this.signals.get(signalName).push(callback);
  }

  /**
   * Handle incoming messages
   */
  handleMessage(event) {
    try {
      const message = JSON.parse(event.data);
      
      if (message.type === 'response') {
        this.handleResponse(message);
      } else if (message.type === 'signal') {
        this.handleSignal(message);
      }
    } catch (error) {
      console.error('Message handling error:', error);
    }
  }

  /**
   * Handle response to request
   */
  handleResponse(message) {
    const request = this.pendingRequests.get(message.id);
    if (!request) return;

    clearTimeout(request.timeout);
    this.pendingRequests.delete(message.id);

    if (message.error) {
      request.reject(new Error(message.error));
    } else {
      request.resolve(message.result);
    }
  }

  /**
   * Handle incoming signal
   */
  handleSignal(message) {
    const callbacks = this.signals.get(message.name) || [];
    callbacks.forEach(callback => {
      try {
        callback(message.data);
      } catch (error) {
        console.error('Signal callback error:', error);
      }
    });
  }

  /**
   * Disconnect from Echo
   */
  disconnect() {
    if (this.socket) {
      this.socket.close();
      this.connected = false;
    }
  }
}

export { EchoService };
