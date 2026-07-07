/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// SCAFFOLD NOTICE (Pyxis, 2026-07-04): this file supersedes an earlier draft
// of this scaffold that assumed compiled C++ JSWindowActor classes
// (PyxisBridgeChild.h/.cpp). Reading the REAL pinned tree (ESR 140.0.5,
// gecko-dev `release` @ c61a6579702f4c73ee99131bd5fa216ec8f7b72c) showed
// that JSWindowActors in this codebase are essentially always plain JS
// classes in `.sys.mjs` ES modules (extending the global `JSWindowActorChild`
// / `JSWindowActorParent`, which are exposed to privileged JS without an
// import), registered declaratively in
// toolkit/modules/ActorManagerParent.sys.mjs — NOT hand-written compiled
// C++. This is dramatically simpler than the plan doc's generic
// "implement a JSWindowActor" framing suggested, and is the real mechanism
// to use.
//
// Design precedent used directly: toolkit/actors/RemotePageChild.sys.mjs
// (about: pages' privileged-API injection: Cu.exportFunction +
// Cu.cloneInto in actorCreated(), sendQuery()/sendAsyncMessage() for
// parent RPC) is the closest real analogue to "inject a compiled-in API
// object into a privileged internal-scheme page" — exactly what Pyxis needs
// for pyxis://. toolkit/actors/WebChannelChild.sys.mjs was also read as a
// second precedent for a generic page<->chrome message-passing shape.
//
// Bridge API SHAPE requirement (pyxis-scheme-and-kiosk-plan.md §B3): must
// be byte-identical in JS-facing shape to the existing dev-time WebExtension
// bridge, gecko/bridge/extension/gaia-bridge.js, i.e. window.geckoOS with
// `callService(service, method, params)`, `.appManager.*`, `.filesystem.*`,
// `.power.*`, `.settings.*`, `.windowManager.*`, `.network.*`,
// `.deviceInfo.*`, `.notification.*`, `.process.*`, `addEventListener`. This
// file builds that exact object and installs it as window.wrappedJSObject.geckoOS
// (the content-window global) rather than gaia-bridge.js's postMessage-based
// approach — postMessage was needed there because content scripts run in a
// separate scope from the page; a JSWindowActorChild running at
// document-start has direct access to inject into the content global via
// Cu.exportFunction, which is strictly better (real function objects, not
// message-passing shims) and still gives identical call syntax to app code.

const lazy = {};

export class PyxisBridgeChild extends JSWindowActorChild {
  actorCreated() {
    this._nextRequestId = 1;
    this._pendingRequests = new Map(); // requestId -> {resolve, reject}
    this._eventListeners = new Map(); // "service:event" -> Set<callback>
    this._installBridge();
  }

  // --- Installing window.geckoOS into the content global ---
  //
  // Mirrors gaia-bridge.js's public API shape (see notice above) so app JS
  // written against the dev-time extension bridge runs unmodified under the
  // packaged pyxis:// runtime. Every leaf method here is a thin wrapper
  // around callService(), matching gaia-bridge.js method-for-method.
  _installBridge() {
    const window = this.contentWindow;
    if (!window) {
      return;
    }

    const callService = (service, method, params) =>
      this._callService(service, method, params);
    const addEventListener = (service, eventName, callback) =>
      this._addEventListener(service, eventName, callback);

    // Build the same nested-namespace shape as gaia-bridge.js's getters
    // (filesystem/appManager/power/settings/windowManager/network/
    // deviceInfo/notification/process), but as plain exported function
    // objects rather than JS getters — Cu.exportFunction/cloneInto need
    // concrete function values, not accessor properties, to cross the
    // privileged/content boundary cleanly.
    const api = {
      // gaia-bridge.js's `initialized` starts false and flips true only
      // once its ASYNC native-messaging-host handshake completes — real
      // work that takes real time. This bridge has no equivalent handshake
      // at all: by the time content code can ever observe `window.geckoOS`,
      // this whole synchronous _installBridge() call has already finished,
      // so it's ready from the very first read. Found as a real bug via a
      // real run: this field was missing entirely, so EchoClient.ts's
      // `waitForGeckoOSReady()` (which gates every single real Echo call on
      // `window.geckoOS.initialized` being truthy) could never see it as
      // ready except by luck-of-timing catching the one-shot
      // "geckoOSReady" event before it fired — meaning, in practice, EVERY
      // call silently sat out a 3-second timeout and then fell back to the
      // mock backend forever, even though the real bridge was present and
      // fully working the whole time. Setting this eagerly makes
      // `waitForGeckoOSReady`'s very first synchronous check
      // (`if (window.geckoOS.initialized) return true`) succeed
      // immediately, with no dependency on event timing at all.
      initialized: true,
      callService,
      addEventListener,
      filesystem: this._buildFilesystemApi(callService, addEventListener),
      appManager: this._buildAppManagerApi(callService, addEventListener),
      power: this._buildPowerApi(callService, addEventListener),
      settings: this._buildSettingsApi(callService, addEventListener),
      windowManager: this._buildWindowManagerApi(callService, addEventListener),
      network: this._buildNetworkApi(callService, addEventListener),
      deviceInfo: this._buildDeviceInfoApi(callService),
      notification: this._buildNotificationApi(callService, addEventListener),
      process: this._buildProcessApi(callService, addEventListener),
      getSystemInfo: () => callService("system", "getInfo", {}),
      listServices: () => callService("system", "listServices", {}),
    };

    const cloned = this._exportApiObject(window, api);
    // Match gaia-bridge.js's dual exposure (window.navigator.geckoOS and
    // window.geckoOS) via wrappedJSObject so content script sees a real,
    // callable object rather than an opaque Xray-wrapped proxy.
    window.wrappedJSObject.geckoOS = cloned;
    if (window.wrappedJSObject.navigator) {
      window.wrappedJSObject.navigator.geckoOS = cloned;
    }

    // gaia-bridge.js dispatches a "geckoOSReady" CustomEvent once
    // initialized; replicate that so app code using the event-based
    // ready-check (rather than assuming window.geckoOS exists synchronously)
    // keeps working unmodified.
    const readyEvent = new window.CustomEvent("geckoOSReady", {
      detail: cloned,
    });
    window.dispatchEvent(readyEvent);
  }

  // Recursively Cu.exportFunction's every function in a plain-object API
  // tree into `window`, returning a Cu.cloneInto'd-compatible structure the
  // content page can call into directly. Plain data values pass through
  // untouched (there aren't any at the leaves here — every leaf is a
  // function — but nested namespace objects need their OWN
  // cloneInto/exportFunction treatment since Cu.cloneInto alone does not
  // preserve function identity/callability across the boundary).
  _exportApiObject(window, obj) {
    const out = new window.Object();
    for (const [key, value] of Object.entries(obj)) {
      if (typeof value === "function") {
        // NOT `Cu.exportFunction(value, window, { defineAs: key })` +
        // `window[key]` + `delete window[key]` (the previous approach,
        // found to be a real bug via a real run): several leaf method
        // names here — most importantly `windowManager.open`/`.close` —
        // collide with genuine native `Window` methods
        // (`window.open`/`window.close`). Momentarily *defining* those
        // directly on the window (even before deleting them again) means
        // `Cu.exportFunction`'s internal `defineProperty` has to succeed
        // against whatever the real property descriptor for `open`/
        // `close` is on this specific window — and a kiosk/app-mode
        // window (which this pyxis:// window always is) is exactly the
        // kind of window that plausibly locks those down as
        // non-configurable for security. Omitting `defineAs` entirely
        // makes `Cu.exportFunction` just return the exported function
        // wrapper without ever touching a property on `window` at all, so
        // there is no collision to hit regardless of what `open`/`close`
        // look like on this particular window.
        //
        // Every leaf here (directly or via callService) is `async` and
        // returns a Promise. REAL BUG, found via a real browser-console
        // report (2026-07-07): `Cu.exportFunction(value, window)` alone
        // exports the FUNCTION correctly, but does nothing about the
        // VALUE it returns — when called, content received the raw
        // chrome-compartment Promise `value()` produced. Content code
        // `await`-ing that (every single caller in useEcho.ts/EchoClient.ts
        // does) makes the JS engine internally check `IsCallable(p.then)`,
        // which Xray semantics do not allow for an arbitrary chrome
        // Promise instance handed across the boundary this way — throwing
        // `Permission denied to access property "then"` every time,
        // whether or not the underlying Echo call itself actually
        // succeeded (it did: PyxisBridgeParent's real Echo round-trip
        // completes entirely on the chrome side before this ever matters,
        // which is exactly why this bug was invisible to backend-log-based
        // testing and only showed up in the content page's own devtools
        // console). Fix: wrap every exported function so its return value
        // is routed through _toContent(), which re-homes a thenable into
        // a genuine `window.Promise` (content's own realm) before handing
        // it back, resolving/rejecting with already-content-safe values.
        const fn = value;
        const bridged = (...args) => this._toContent(window, fn(...args));
        out[key] = Cu.exportFunction(bridged, window);
      } else if (typeof value === "object" && value !== null) {
        out[key] = this._exportApiObject(window, value);
      } else {
        out[key] = value;
      }
    }
    return out;
  }

  // Re-homes a chrome-compartment return value into content's own realm so
  // content can freely `await`/`.then()`/call it with no Xray boundary
  // issues. Handles the three shapes every API leaf here can return:
  // a Promise (the common case — every leaf routes through callService(),
  // which is async), a plain callable (addEventListener()'s unsubscribe
  // function), or plain cloneable data.
  _toContent(window, value) {
    if (value && typeof value.then === "function") {
      return new window.Promise((resolve, reject) => {
        value.then(
          (result) => resolve(this._toContent(window, result)),
          (error) => reject(new window.Error(String(error?.message ?? error)))
        );
      });
    }
    if (typeof value === "function") {
      return Cu.exportFunction(value, window);
    }
    if (value && typeof value === "object") {
      // Already content-safe by the time it gets here in every real case
      // (e.g. _callService's own Cu.cloneInto of the Echo response) —
      // passed through as-is rather than re-cloned.
      return value;
    }
    return value;
  }

  // --- Parent RPC plumbing ---
  //
  // callService() is the single choke point every API method routes
  // through, matching gaia-bridge.js's callService()-centric design
  // exactly. Uses the actor's sendQuery() (promise-based
  // request/response over the actor channel) rather than gaia-bridge.js's
  // hand-rolled postMessage + requestId/timeout bookkeeping — sendQuery
  // already provides request correlation and promise semantics natively,
  // so that bookkeeping (this._pendingRequests etc.) is NOT needed here the
  // way gaia-bridge.js needed it for its postMessage transport.
  async _callService(service, method, params = {}) {
    const window = this.contentWindow;
    try {
      const result = await this.sendQuery("PyxisBridge:CallService", {
        service,
        method,
        params,
      });
      return Cu.cloneInto(result, window);
    } catch (error) {
      // sendQuery rejects with whatever the parent actor's handler threw —
      // including PyxisBridgeParent's capability-allow-list denial (see
      // PyxisBridgeParent.sys.mjs), which is surfaced here as an ordinary
      // rejected promise, exactly like gaia-bridge.js's existing
      // service-error rejection path.
      throw new (window.Error)(String(error?.message ?? error));
    }
  }

  _addEventListener(service, eventName, callback) {
    const key = `${service}:${eventName}`;
    if (!this._eventListeners.has(key)) {
      this._eventListeners.set(key, new Set());
      // Tell the parent we now care about this service:event pair so it
      // starts forwarding matching Echo signals to us (avoids forwarding
      // every Echo signal to every page unconditionally).
      this.sendAsyncMessage("PyxisBridge:Subscribe", { service, eventName });
    }
    this._eventListeners.get(key).add(callback);

    // Return an unsubscribe function, matching gaia-bridge.js's
    // addEventListener() return contract.
    const listeners = this._eventListeners.get(key);
    return () => listeners.delete(callback);
  }

  // Parent -> child signal delivery (Echo events re-broadcast as `geckoOS`
  // events into content), analogous to gaia-bridge.js's
  // GECKOOSPBRIDGE_EVENT handling.
  receiveMessage(msg) {
    if (msg.name !== "PyxisBridge:Event") {
      return undefined;
    }
    const { service, eventName, data } = msg.data;
    const key = `${service}:${eventName}`;
    const listeners = this._eventListeners.get(key);
    if (!listeners || listeners.size === 0) {
      return undefined;
    }
    const window = this.contentWindow;
    const cloned = window ? Cu.cloneInto(data, window) : data;
    for (const callback of listeners) {
      try {
        callback(cloned);
      } catch (e) {
        console.error("[PyxisBridge] listener error:", e);
      }
    }
    return undefined;
  }

  // --- Namespace builders (mirror gaia-bridge.js method-for-method) ---
  // Kept as small factory functions rather than one giant object literal so
  // any single namespace can be unit-tested / diffed against
  // gaia-bridge.js's corresponding getter in isolation.

  _buildFilesystemApi(callService, addEventListener) {
    return {
      readDirectory: path => callService("filesystem", "readDirectory", { path }),
      readFile: (path, encoding = "utf-8") =>
        callService("filesystem", "readFile", { path, encoding }),
      writeFile: (path, content, encoding = "utf-8") =>
        callService("filesystem", "writeFile", { path, content, encoding }),
      appendFile: (path, content, encoding = "utf-8") =>
        callService("filesystem", "appendFile", { path, content, encoding }),
      deleteFile: path => callService("filesystem", "deleteFile", { path }),
      deleteDirectory: (path, recursive = false) =>
        callService("filesystem", "deleteDirectory", { path, recursive }),
      createDirectory: (path, mode = 0o755) =>
        callService("filesystem", "createDirectory", { path, mode }),
      exists: path => callService("filesystem", "exists", { path }),
      stat: path => callService("filesystem", "stat", { path }),
      copy: (source, destination) =>
        callService("filesystem", "copy", { source, destination }),
      move: (source, destination) =>
        callService("filesystem", "move", { source, destination }),
      chmod: (path, mode) => callService("filesystem", "chmod", { path, mode }),
      watch: (path, callback) =>
        addEventListener("filesystem", `watch:${path}`, callback),
    };
  }

  _buildAppManagerApi(callService, addEventListener) {
    return {
      launch: (appId, params = {}) =>
        callService("appManager", "launch", { appId, params }),
      close: appId => callService("appManager", "close", { appId }),
      list: () => callService("appManager", "list", {}),
      getInfo: appId => callService("appManager", "getInfo", { appId }),
      install: appPath => callService("appManager", "install", { appPath }),
      uninstall: appId => callService("appManager", "uninstall", { appId }),
      kill: appId => callService("appManager", "kill", { appId }),
      restart: appId => callService("appManager", "restart", { appId }),
      setActive: appId => callService("appManager", "setActive", { appId }),
      getActive: () => callService("appManager", "getActive", {}),
      onLaunched: cb => addEventListener("appManager", "launched", cb),
      onClosed: cb => addEventListener("appManager", "closed", cb),
      onInstalled: cb => addEventListener("appManager", "installed", cb),
    };
  }

  _buildPowerApi(callService, addEventListener) {
    return {
      shutdown: () => callService("power", "shutdown", {}),
      reboot: () => callService("power", "reboot", {}),
      sleep: () => callService("power", "sleep", {}),
      wake: () => callService("power", "wake", {}),
      getBatteryStatus: () => callService("power", "getBatteryStatus", {}),
      setScreenBrightness: brightness =>
        callService("power", "setScreenBrightness", { brightness }),
      getScreenBrightness: () => callService("power", "getScreenBrightness", {}),
      setScreenTimeout: timeout =>
        callService("power", "setScreenTimeout", { timeout }),
      getScreenTimeout: () => callService("power", "getScreenTimeout", {}),
      onBatteryLow: cb => addEventListener("power", "batteryLow", cb),
      onPowerStateChanged: cb => addEventListener("power", "powerStateChanged", cb),
    };
  }

  _buildSettingsApi(callService, addEventListener) {
    return {
      get: key => callService("settings", "get", { key }),
      set: (key, value) => callService("settings", "set", { key, value }),
      getAll: () => callService("settings", "getAll", {}),
      delete: key => callService("settings", "delete", { key }),
      clear: () => callService("settings", "clear", {}),
      reset: () => callService("settings", "reset", {}),
      onChange: (key, cb) => addEventListener("settings", `change:${key}`, cb),
    };
  }

  _buildWindowManagerApi(callService, addEventListener) {
    return {
      open: (url, options = {}) =>
        callService("windowManager", "open", { url, options }),
      close: windowId => callService("windowManager", "close", { windowId }),
      focus: windowId => callService("windowManager", "focus", { windowId }),
      blur: windowId => callService("windowManager", "blur", { windowId }),
      maximize: windowId => callService("windowManager", "maximize", { windowId }),
      minimize: windowId => callService("windowManager", "minimize", { windowId }),
      restore: windowId => callService("windowManager", "restore", { windowId }),
      setSize: (windowId, width, height) =>
        callService("windowManager", "setSize", { windowId, width, height }),
      setPosition: (windowId, x, y) =>
        callService("windowManager", "setPosition", { windowId, x, y }),
      list: () => callService("windowManager", "list", {}),
      onWindowOpened: cb => addEventListener("windowManager", "windowOpened", cb),
      onWindowClosed: cb => addEventListener("windowManager", "windowClosed", cb),
    };
  }

  _buildNetworkApi(callService, addEventListener) {
    return {
      getStatus: () => callService("network", "getStatus", {}),
      getType: () => callService("network", "getType", {}),
      getInterfaces: () => callService("network", "getInterfaces", {}),
      getIPAddress: () => callService("network", "getIPAddress", {}),
      getGateway: () => callService("network", "getGateway", {}),
      getDNS: () => callService("network", "getDNS", {}),
      onStatusChanged: cb => addEventListener("network", "statusChanged", cb),
      onTypeChanged: cb => addEventListener("network", "typeChanged", cb),
    };
  }

  _buildDeviceInfoApi(callService) {
    return {
      getOS: () => callService("deviceInfo", "getOS", {}),
      getKernel: () => callService("deviceInfo", "getKernel", {}),
      getModel: () => callService("deviceInfo", "getModel", {}),
      getManufacturer: () => callService("deviceInfo", "getManufacturer", {}),
      getSerial: () => callService("deviceInfo", "getSerial", {}),
      getHardware: () => callService("deviceInfo", "getHardware", {}),
      getUUID: () => callService("deviceInfo", "getUUID", {}),
      getHostname: () => callService("deviceInfo", "getHostname", {}),
      getMemoryInfo: () => callService("deviceInfo", "getMemoryInfo", {}),
      getStorageInfo: () => callService("deviceInfo", "getStorageInfo", {}),
      getCPUInfo: () => callService("deviceInfo", "getCPUInfo", {}),
    };
  }

  _buildNotificationApi(callService, addEventListener) {
    return {
      show: (title, options = {}) =>
        callService("notification", "show", { title, options }),
      close: notificationId =>
        callService("notification", "close", { notificationId }),
      closeAll: () => callService("notification", "closeAll", {}),
      onClicked: (notificationId, cb) =>
        addEventListener("notification", `clicked:${notificationId}`, cb),
      onClosed: (notificationId, cb) =>
        addEventListener("notification", `closed:${notificationId}`, cb),
    };
  }

  _buildProcessApi(callService, addEventListener) {
    return {
      spawn: (command, args = [], options = {}) =>
        callService("process", "spawn", { command, args, options }),
      kill: pid => callService("process", "kill", { pid }),
      killGroup: pgid => callService("process", "killGroup", { pgid }),
      list: () => callService("process", "list", {}),
      getInfo: pid => callService("process", "getInfo", { pid }),
      onProcessSpawned: cb => addEventListener("process", "spawned", cb),
      onProcessTerminated: cb => addEventListener("process", "terminated", cb),
    };
  }
}
