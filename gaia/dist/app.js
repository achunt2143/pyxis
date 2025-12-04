(() => {
  // static/services/api-service.js
  var APIService = class {
    constructor(baseUrl = "http://localhost:3000/api") {
      this.baseUrl = baseUrl;
      this.headers = {
        "Content-Type": "application/json",
        "X-Client": "Gaia-Shell"
      };
      this.timeout = 1e4;
    }
    /**
     * Perform GET request
     */
    async get(endpoint, options = {}) {
      return this.request(endpoint, { ...options, method: "GET" });
    }
    /**
     * Perform POST request
     */
    async post(endpoint, data, options = {}) {
      return this.request(endpoint, {
        ...options,
        method: "POST",
        body: JSON.stringify(data)
      });
    }
    /**
     * Perform PUT request
     */
    async put(endpoint, data, options = {}) {
      return this.request(endpoint, {
        ...options,
        method: "PUT",
        body: JSON.stringify(data)
      });
    }
    /**
     * Perform DELETE request
     */
    async delete(endpoint, options = {}) {
      return this.request(endpoint, { ...options, method: "DELETE" });
    }
    /**
     * Core request method
     */
    async request(endpoint, options = {}) {
      const url = `${this.baseUrl}${endpoint}`;
      const config = {
        method: options.method || "GET",
        headers: { ...this.headers, ...options.headers },
        ...options
      };
      try {
        const controller = new AbortController();
        const timeoutId = setTimeout(() => controller.abort(), this.timeout);
        const response = await fetch(url, {
          ...config,
          signal: controller.signal
        });
        clearTimeout(timeoutId);
        if (!response.ok) {
          throw new Error(`HTTP ${response.status}: ${response.statusText}`);
        }
        const data = await response.json();
        return data;
      } catch (error) {
        console.error("API request failed:", error);
        throw error;
      }
    }
    /**
     * Upload file
     */
    async uploadFile(endpoint, file) {
      const formData = new FormData();
      formData.append("file", file);
      const response = await fetch(`${this.baseUrl}${endpoint}`, {
        method: "POST",
        body: formData,
        headers: {
          "X-Client": "Gaia-Shell"
        }
      });
      if (!response.ok) {
        throw new Error(`Upload failed: ${response.statusText}`);
      }
      return response.json();
    }
    /**
     * Batch requests
     */
    async batch(requests) {
      return Promise.all(requests.map((req) => this.request(req.endpoint, req.options)));
    }
  };

  // static/services/echo-service.js
  var EchoService = class {
    constructor(socketUrl = "ws://localhost:8080") {
      this.socketUrl = socketUrl;
      this.socket = null;
      this.connected = false;
      this.services = /* @__PURE__ */ new Map();
      this.methods = /* @__PURE__ */ new Map();
      this.signals = /* @__PURE__ */ new Map();
      this.requestId = 0;
      this.pendingRequests = /* @__PURE__ */ new Map();
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
            console.log("\u2705 Connected to Echo IPC");
            resolve();
          };
          this.socket.onmessage = (event) => this.handleMessage(event);
          this.socket.onerror = (error) => {
            console.error("Echo IPC error:", error);
            reject(error);
          };
          this.socket.onclose = () => {
            this.connected = false;
            console.log("Disconnected from Echo IPC");
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
      return this.call("org.geckoOS.Echo", "registerService", {
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
        throw new Error("Not connected to Echo IPC");
      }
      const requestId = ++this.requestId;
      const message = {
        type: "call",
        id: requestId,
        service: serviceName,
        method,
        args
      };
      return new Promise((resolve, reject) => {
        const timeout = setTimeout(() => {
          this.pendingRequests.delete(requestId);
          reject(new Error("Request timeout"));
        }, 1e4);
        this.pendingRequests.set(requestId, { resolve, reject, timeout });
        this.socket.send(JSON.stringify(message));
      });
    }
    /**
     * Emit signal
     */
    emit(signalName, data) {
      if (!this.connected)
        return;
      const message = {
        type: "signal",
        name: signalName,
        data
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
        if (message.type === "response") {
          this.handleResponse(message);
        } else if (message.type === "signal") {
          this.handleSignal(message);
        }
      } catch (error) {
        console.error("Message handling error:", error);
      }
    }
    /**
     * Handle response to request
     */
    handleResponse(message) {
      const request = this.pendingRequests.get(message.id);
      if (!request)
        return;
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
      callbacks.forEach((callback) => {
        try {
          callback(message.data);
        } catch (error) {
          console.error("Signal callback error:", error);
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
  };

  // static/services/cache-service.js
  var CacheService = class {
    constructor(maxSize = 100) {
      this.cache = /* @__PURE__ */ new Map();
      this.maxSize = maxSize;
      this.stats = {
        hits: 0,
        misses: 0,
        evictions: 0
      };
    }
    /**
     * Set cache entry with optional TTL
     */
    set(key, value, ttl = null) {
      if (this.cache.has(key)) {
        this.cache.delete(key);
      }
      if (this.cache.size >= this.maxSize) {
        const firstKey = this.cache.keys().next().value;
        this.cache.delete(firstKey);
        this.stats.evictions++;
      }
      const entry = {
        value,
        timestamp: Date.now(),
        ttl
      };
      this.cache.set(key, entry);
      if (ttl) {
        setTimeout(() => this.delete(key), ttl);
      }
    }
    /**
     * Get cache entry
     */
    get(key) {
      if (!this.cache.has(key)) {
        this.stats.misses++;
        return null;
      }
      const entry = this.cache.get(key);
      if (entry.ttl && Date.now() - entry.timestamp > entry.ttl) {
        this.cache.delete(key);
        this.stats.misses++;
        return null;
      }
      this.cache.delete(key);
      this.cache.set(key, entry);
      this.stats.hits++;
      return entry.value;
    }
    /**
     * Delete entry
     */
    delete(key) {
      return this.cache.delete(key);
    }
    /**
     * Check if key exists
     */
    has(key) {
      return this.cache.has(key);
    }
    /**
     * Clear all cache
     */
    clear() {
      this.cache.clear();
      this.stats = { hits: 0, misses: 0, evictions: 0 };
    }
    /**
     * Get hit rate
     */
    getHitRate() {
      const total = this.stats.hits + this.stats.misses;
      return total === 0 ? 0 : this.stats.hits / total * 100;
    }
    /**
     * Get cache statistics
     */
    getStats() {
      return {
        ...this.stats,
        size: this.cache.size,
        maxSize: this.maxSize,
        hitRate: this.getHitRate().toFixed(2) + "%"
      };
    }
  };

  // static/services/performance-service.js
  var PerformanceService = class {
    constructor() {
      this.metrics = {
        startTime: performance.now(),
        frameCount: 0,
        fps: 0,
        memory: {}
      };
    }
    /**
     * Get current metrics
     */
    getMetrics() {
      if (performance.memory) {
        this.metrics.memory = {
          usedJSHeapSize: performance.memory.usedJSHeapSize,
          totalJSHeapSize: performance.memory.totalJSHeapSize,
          jsHeapSizeLimit: performance.memory.jsHeapSizeLimit
        };
      }
      return this.metrics;
    }
    /**
     * Measure operation
     */
    measure(name, fn) {
      const start = performance.now();
      const result = fn();
      const duration = performance.now() - start;
      console.log(`${name}: ${duration.toFixed(2)}ms`);
      return result;
    }
    /**
     * Get page load time
     */
    getPageLoadTime() {
      const perfData = performance.timing;
      return perfData.loadEventEnd - perfData.navigationStart;
    }
  };

  // static/scripts/module_loader.js
  var ModuleLoader = class {
    static modules = /* @__PURE__ */ new Map();
    static loading = /* @__PURE__ */ new Map();
    static cache = /* @__PURE__ */ new Map();
    /**
     * Load a module dynamically
     */
    static async load(moduleName) {
      if (this.modules.has(moduleName)) {
        return this.modules.get(moduleName);
      }
      if (this.loading.has(moduleName)) {
        return this.loading.get(moduleName);
      }
      const loadPromise = this._loadModule(moduleName);
      this.loading.set(moduleName, loadPromise);
      try {
        const module = await loadPromise;
        this.modules.set(moduleName, module);
        return module;
      } finally {
        this.loading.delete(moduleName);
      }
    }
    /**
     * Internal module loading logic
     */
    static async _loadModule(moduleName) {
      const modulePath = `/gaia/static/modules/${moduleName}/${moduleName}.js`;
      try {
        const moduleExport = await import(modulePath);
        const ModuleClass = moduleExport.default || moduleExport[this._toCamelCase(moduleName)];
        if (!ModuleClass) {
          throw new Error(`Module ${moduleName} does not export default class`);
        }
        return ModuleClass;
      } catch (error) {
        console.error(`Failed to load module ${moduleName}:`, error);
        throw new Error(`Module loading failed: ${moduleName}`);
      }
    }
    /**
     * Load module stylesheet
     */
    static async loadStyles(moduleName) {
      const cacheKey = `styles-${moduleName}`;
      if (this.cache.has(cacheKey)) {
        return;
      }
      const stylePath = `/gaia/static/modules/${moduleName}/${moduleName}.css`;
      return new Promise((resolve, reject) => {
        const link = document.createElement("link");
        link.rel = "stylesheet";
        link.href = stylePath;
        link.onload = () => {
          this.cache.set(cacheKey, true);
          resolve();
        };
        link.onerror = reject;
        document.head.appendChild(link);
      });
    }
    /**
     * Preload multiple modules
     */
    static async preload(moduleNames) {
      return Promise.all(moduleNames.map((name) => this.load(name)));
    }
    /**
     * Clear cache
     */
    static clearCache() {
      this.modules.clear();
      this.cache.clear();
    }
    /**
     * Convert kebab-case to camelCase
     */
    static _toCamelCase(str) {
      return str.replace(/-([a-z])/g, (g) => g[1].toUpperCase());
    }
    /**
     * Get loading progress
     */
    static getProgress() {
      return {
        total: this.modules.size,
        loading: this.loading.size,
        cached: this.cache.size
      };
    }
  };

  // static/scripts/app.js
  var GaiaApp = class {
    constructor() {
      this.modules = /* @__PURE__ */ new Map();
      this.services = /* @__PURE__ */ new Map();
      this.config = {
        debug: true,
        apiUrl: "http://localhost:3000/api",
        echoSocket: "ws://localhost:8080"
      };
      this.eventBus = new EventTarget();
    }
    /**
     * Bootstrap the application
     */
    async bootstrap() {
      try {
        console.log("\u{1F680} Gaia Shell v2.0 Bootstrapping...");
        await this.initializeServices();
        await this.initializeModules();
        await this.setupEventListeners();
        console.log("\u2705 Gaia Shell Ready");
        this.emit("app:ready");
      } catch (error) {
        console.error("\u274C Bootstrap failed:", error);
        this.showError("Failed to initialize Gaia Shell");
      }
    }
    /**
     * Initialize services
     */
    async initializeServices() {
      console.log("\u{1F4E6} Initializing services...");
      const apiService = new APIService(this.config.apiUrl);
      this.services.set("api", apiService);
      const echoService = new EchoService(this.config.echoSocket);
      await echoService.connect();
      this.services.set("echo", echoService);
      const cacheService = new CacheService();
      this.services.set("cache", cacheService);
      const perfService = new PerformanceService();
      this.services.set("performance", perfService);
    }
    /**
     * Initialize modules
     */
    async initializeModules() {
      console.log("\u{1F3A8} Initializing modules...");
      const modulesToLoad = [
        { name: "taskbar", selector: "#taskbar" },
        { name: "sidebar", selector: "#sidebar" },
        { name: "dashboard", selector: "#dashboard" },
        { name: "window-manager", selector: "#windows-container" }
      ];
      for (const module of modulesToLoad) {
        try {
          const ModuleClass = await ModuleLoader.load(module.name);
          const instance = new ModuleClass({
            element: document.querySelector(module.selector),
            app: this,
            services: this.services
          });
          await instance.initialize();
          this.modules.set(module.name, instance);
          console.log(`\u2713 Loaded: ${module.name}`);
        } catch (error) {
          console.error(`\u2717 Failed to load module: ${module.name}`, error);
        }
      }
    }
    /**
     * Setup event listeners
     */
    setupEventListeners() {
      document.addEventListener("contextmenu", (e) => this.handleContextMenu(e));
      document.addEventListener("keydown", (e) => this.handleKeyboard(e));
      window.addEventListener("focus", () => this.emit("app:focused"));
      window.addEventListener("blur", () => this.emit("app:blurred"));
    }
    /**
     * Handle context menu
     */
    handleContextMenu(event) {
      event.preventDefault();
      const contextMenu = document.getElementById("context-menu");
      contextMenu.style.left = event.pageX + "px";
      contextMenu.style.top = event.pageY + "px";
      contextMenu.classList.add("active");
    }
    /**
     * Handle keyboard shortcuts
     */
    handleKeyboard(event) {
      if (event.ctrlKey && event.altKey && event.key === "t") {
        event.preventDefault();
        this.launchApp("terminal");
      }
      if (event.ctrlKey && event.altKey && event.key === "f") {
        event.preventDefault();
        this.launchApp("file-manager");
      }
      if (event.altKey && event.key === "Tab") {
        event.preventDefault();
        this.switchWindow();
      }
    }
    /**
     * Launch application
     */
    async launchApp(appId) {
      try {
        const echo = this.services.get("echo");
        const result = await echo.call("org.geckoOS.Persephone", "executeApplication", {
          appId
        });
        this.emit("app:launched", { appId, result });
      } catch (error) {
        console.error("Failed to launch app:", error);
      }
    }
    /**
     * Switch windows
     */
    switchWindow() {
      const windowManager = this.modules.get("window-manager");
      if (windowManager) {
        windowManager.switchWindow();
      }
    }
    /**
     * Show error notification
     */
    showError(message) {
      const notification = document.createElement("div");
      notification.className = "alert alert-danger";
      notification.textContent = message;
      const container = document.getElementById("notifications");
      container.appendChild(notification);
      setTimeout(() => notification.remove(), 5e3);
    }
    /**
     * Emit event
     */
    emit(eventName, detail = {}) {
      const event = new CustomEvent(eventName, { detail });
      this.eventBus.dispatchEvent(event);
      window.dispatchEvent(event);
    }
    /**
     * Subscribe to event
     */
    on(eventName, callback) {
      this.eventBus.addEventListener(eventName, callback);
    }
    /**
     * Get service
     */
    getService(name) {
      return this.services.get(name);
    }
    /**
     * Get module
     */
    getModule(name) {
      return this.modules.get(name);
    }
  };
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", () => {
      window.gaiaApp = new GaiaApp();
      window.gaiaApp.bootstrap();
    });
  } else {
    window.gaiaApp = new GaiaApp();
    window.gaiaApp.bootstrap();
  }
})();
