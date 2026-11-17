/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// SCAFFOLD NOTICE (Pyxis, 2026-07-04): see PyxisBridgeChild.sys.mjs for the
// full rationale on why this is plain-JS JSWindowActorParent rather than
// compiled C++ (real precedent: toolkit/actors/WebChannelParent.sys.mjs /
// RemotePageChild.sys.mjs's parent-side counterparts, read directly from the
// pinned ESR 140.0.5 tree).
//
// This is the chrome-privileged parent half. Per
// pyxis-scheme-and-kiosk-plan.md §B3, its two jobs are:
//   1. Own the connection to the Echo Unix socket and dispatch
//      `org.geckoOS.*` requests, replacing the Rust native-messaging host
//      (Synergy) FOR pyxis:// apps specifically (Synergy/the WebExtension
//      bridge stays for the general http/https browser app — see
//      pyxis-gecko-embedding-plan.md §4).
//   2. Be the enforcement point for the capability allow-list: "a call the
//      app didn't declare is refused here before it reaches Echo — the
//      LS2-role-file check, realized" (§B3). This is literally the single
//      biggest security-model gap called out in
//      pyxis-webos-parity-analysis.md and CLAUDE.md's project summary, so
//      the permission check below is written as a hard gate, not a stub —
//      even though the actual allow-list SOURCE (reading an app's declared
//      manifest.webapp `permissions[]`) has to come from Nova, which
//      doesn't exist yet (nova-runtime-host-plan.md — design-only, not
//      built). See _getDeclaredPermissions() below for exactly where that
//      wire-up plugs in once Nova exists.
//
// Echo transport: IMPLEMENTED (2026-07-05) using option (a) from the
// decision this file used to stub out — a privileged JS Unix-domain-socket
// client built on nsISocketTransportService.createUnixDomainTransport(),
// NOT a Rust/XPCOM wrapper crate. Why (a) over (b), decided after reading
// both the real Echo Rust source (echo/src/{lib,transport,message}.rs) and
// current mozilla-central precedent for both options at this exact pin
// (5836a062726f715fda621338a17b51aff30d0a8c, branch central-work):
//
//   - nsISocketTransportService.createUnixDomainTransport(nsIFile) is a
//     real, `scriptable` (JS-callable) method that already exists in this
//     tree (netwerk/base/nsISocketTransportService.idl) — no IDL to write,
//     no components.conf entry, no libxul relink. devtools/shared/security/
//     socket.js and devtools/shared/transport/transport.js already exercise
//     the sibling TCP path (createTransport + openInputStream/
//     openOutputStream + asyncWait/onInputStreamReady/onOutputStreamReady)
//     from plain chrome JS with no C++ glue — the Unix-domain variant is
//     the identical stream API, just a different transport constructor.
//   - Option (b) (a Rust XPCOM component wrapping geckoOS-echo's client)
//     WOULD be real and buildable in principle — this tree has genuine
//     precedent (toolkit/components/kvstore/src/lib.rs,
//     dom/webauthn/authrs_bridge/src/lib.rs use real
//     `#[xpcom(implement(...))]` structs) — but every one of those examples
//     requires a new .idl (XPIDL_SOURCES), a components.conf CID
//     registration, a C++ constructor shim header, AND adding the crate as
//     a dependency of toolkit/library/rust/shared/Cargo.toml so it links
//     into gkrust-shared/libxul. That's real Gecko-build-system surgery
//     this session cannot compile or verify (per this task's constraints),
//     for strictly more total effort than (a) — the opposite of what this
//     file's old comment guessed. So (a) wins on both "less total work" and
//     "actually verifiable this session."
//
// Wire protocol (verified directly against echo/src/{transport,message,
// lib}.rs and echo/src/bin/daemon.rs's handle_connection — NOT guessed):
//   - Framing: 4-byte little-endian length prefix, then that many bytes of
//     UTF-8 JSON (echo/src/transport.rs's send_message/recv_message; the
//     LE choice is called out there as "CRITICAL" and must match exactly).
//     nsIBinaryOutputStream/nsIBinaryInputStream's write32/read32 are
//     BIG-endian ("network order" per xpcom/io/nsIBinaryOutputStream.idl),
//     so the length prefix is hand-assembled/parsed byte-by-byte below
//     instead of using read32/write32.
//   - Body: a JSON-serialized `Message { header, body }` matching
//     echo/src/message.rs's Message/MessageHeader/MessageType exactly.
//     MessageType is serde(rename_all = "SCREAMING_SNAKE_CASE"), so
//     MethodCall/MethodReturn/Error/Handshake/Signal appear on the wire as
//     "METHOD_CALL"/"METHOD_RETURN"/"ERROR"/"HANDSHAKE"/"SIGNAL" — verified
//     against echo/src/bin/daemon.rs's handle_connection match arms and
//     gecko/bridge/native/geckoOS-native-host.rs's EchoMessageHeader (the
//     existing Rust client for the WebExtension bridge, which speaks this
//     exact same wire format and is the closest real precedent for the
//     message shape used here).
//   - Destination/interface naming: "org.geckoOS.<service>" for both
//     `destination` and `interface` (echo/src/bin/daemon.rs's
//     register_all_services registers handlers under keys like
//     "org.geckoOS.filesystem.readFile"; geckoOS-native-host.rs's
//     call_echo_service builds `destination = format!("org.geckoOS.{}",
//     service)` the same way) — so this file does the same
//     transformation from the bridge's plain `service` string (e.g.
//     "filesystem", "power") before sending.
//   - Socket path: $XDG_RUNTIME_DIR/geckoOS/echo.sock, falling back to
//     /tmp/geckoOS/echo.sock (echo/src/lib.rs's get_socket_path /
//     EchoClient::default_socket_path) — mirrored in _echoSocketPath()
//     below.
//   - Handshake: echo/src/bin/daemon.rs's handle_connection special-cases
//     MessageType::Handshake by replying immediately with a MethodReturn
//     carrying {status:"ok", version:"1.0.0"} — it is NOT required before
//     issuing a MethodCall (the daemon dispatches MethodCall regardless of
//     `handshake_completed`), but every existing Rust client
//     (geckoOS-native-host.rs's echo_broker_loop) sends one up front on
//     each new connection anyway, so this file does the same for
//     consistency/observability (it shows up in the daemon's connection
//     log) rather than because it's load-bearing.
//
// Connection lifecycle: one persistent connection per content process is
// intentionally NOT attempted here — each PyxisBridgeParent instance (one
// per pyxis:// top-level document actor) lazily opens and keeps its own
// Echo connection, matching the "one native-host-per-tab-ish" granularity
// gaia-bridge.js's dev-time bridge already has. Echo's daemon has no
// apparent per-connection resource ceiling in the code read here
// (active_connections is an unbounded DashMap), so this is not expected to
// be a scaling problem at the scale a desktop OS's app count implies.

const lazy = {};

ChromeUtils.defineLazyGetter(lazy, "socketTransportService", () => {
  return Cc["@mozilla.org/network/socket-transport-service;1"].getService(
    Ci.nsISocketTransportService
  );
});

// Every live PyxisBridgeParent, so that a signal arriving on the shared Echo
// connection is offered to each of them (dispatchSignalToSubscribers).
// Populated in actorCreated()/didDestroy().
//
// Each actor holds its own subscriptions (`_subscriptions`). They used to
// live in one table keyed by browsing context id. But a browsing context
// outlives its documents, and a subscription is allowed or refused by the
// app id of the document that asked for it (_handleSubscribe), so it has to
// go when that document goes. There is one actor per document.
const gLiveActors = new Set();

// Process-wide cache of app-id -> declared permissions[], populated by
// fetchDeclaredPermissions() below. Deliberately process-lifetime (not
// per-actor): every pyxis:// document for the same app-id shares the same
// declared permission set, so there is nothing actor-specific to key on.
const gPermissionsCache = new Map();
const gPermissionsInflight = new Map(); // appId -> { epoch, promise }
const gPermissionsAppEpoch = new Map(); // appId -> monotonic invalidation epoch
let gPermissionsGlobalEpoch = 0;
let gPermissionsEpochSeq = 0;

function currentPermissionsEpoch(appId) {
  return Math.max(
    gPermissionsGlobalEpoch,
    gPermissionsAppEpoch.get(appId) ?? 0
  );
}

function invalidatePermissionsCache(targetAppId = null) {
  gPermissionsEpochSeq += 1;
  if (targetAppId) {
    gPermissionsAppEpoch.set(targetAppId, gPermissionsEpochSeq);
    gPermissionsCache.delete(targetAppId);
    gPermissionsInflight.delete(targetAppId);
  } else {
    gPermissionsGlobalEpoch = gPermissionsEpochSeq;
    gPermissionsCache.clear();
    gPermissionsInflight.clear();
  }
}

async function fetchDeclaredPermissions(appId, allowRetry = true) {
  if (gPermissionsCache.has(appId)) {
    return gPermissionsCache.get(appId);
  }
  const epoch = currentPermissionsEpoch(appId);
  const existing = gPermissionsInflight.get(appId);
  if (existing && existing.epoch === epoch) {
    return existing.promise;
  }

  let shouldRefetch = false;
  const entry = { epoch, promise: null };
  entry.promise = (async () => {
    try {
      const connection = getEchoConnection();
      const info = await connection.callMethod("nova", "GetInfo", { appId });
      const perms = Array.isArray(info?.permissions) ? info.permissions : [];
      if (currentPermissionsEpoch(appId) === epoch) {
        gPermissionsCache.set(appId, perms);
        return perms;
      }
      if (gPermissionsCache.has(appId)) {
        return gPermissionsCache.get(appId);
      }
      shouldRefetch = true;
    } catch (e) {
      console.error(
        `[PyxisBridge] failed to resolve permissions for '${appId}' from ` +
          `Nova, denying all capability-gated calls: ${e}`
      );
      return [];
    } finally {
      if (gPermissionsInflight.get(appId) === entry) {
        gPermissionsInflight.delete(appId);
      }
    }
    if (shouldRefetch && allowRetry) {
      return fetchDeclaredPermissions(appId, false);
    }
    return [];
  })();

  gPermissionsInflight.set(appId, entry);
  return entry.promise;
}

// Window id -> pyxis app-id map maintained from evo ListWindows replies and
// WindowCreated/WindowClosed signals so evo visibility transitions can throttle
// or resume background app browsing contexts.
const gWindowIdToAppId = new Map();

function echoSocketPath() {
  const runtimeDir = Cc["@mozilla.org/process/environment;1"]
    .getService(Ci.nsIEnvironment)
    .get("XDG_RUNTIME_DIR");
  const file = Cc["@mozilla.org/file/local;1"].createInstance(Ci.nsIFile);
  if (runtimeDir) {
    file.initWithPath(runtimeDir);
    file.append("geckoOS");
    file.append("echo.sock");
  } else {
    file.initWithPath("/tmp/geckoOS/echo.sock");
  }
  return file;
}

/**
 * A single persistent connection to the Echo Unix-domain socket, built on
 * nsISocketTransportService.createUnixDomainTransport() (see the file-level
 * notice above for why this is the chosen transport mechanism rather than a
 * Rust/XPCOM wrapper). Handles Echo's real wire framing (4-byte
 * little-endian length prefix + JSON body, see echo/src/transport.rs) and
 * multiplexes concurrent method calls by `message_id`/`reply_to`
 * correlation, exactly like echo/src/bin/daemon.rs's own connection
 * handling and gecko/bridge/native/geckoOS-native-host.rs's echo_broker_loop
 * do on the Rust side.
 */
// Request timeouts for calls to Echo. Most calls answer quickly and keep the
// 25 s default. Some are HELD by Echo until the user answers the root-access
// dialog (up to 120 s), so they get a longer timeout that outlasts the hold.
const DEFAULT_CALL_TIMEOUT_MS = 25000;
const HELD_CALL_TIMEOUT_MS_VALUE = 150000;
const HELD_CALL_TIMEOUT_MS = new Set([
  "org.geckoOS.installer.Install",
  "org.geckoOS.installer.Remove",
  "org.geckoOS.nova.Install",
  "org.geckoOS.nova.Uninstall",
  "org.geckoOS.network.connectWifi",
  "org.geckoOS.network.disconnectWifi",
  "org.geckoOS.network.setInterfaceEnabled",
  "org.geckoOS.network.setDNS",
  "org.geckoOS.network.setHostname",
  "org.geckoOS.accounts.create",
  "org.geckoOS.accounts.delete",
]);

class EchoConnection {
  constructor() {
    this._transport = null;
    this._input = null; // nsIScriptableInputStream wrapping the raw async input
    this._rawInput = null; // nsIAsyncInputStream
    this._output = null; // nsIAsyncOutputStream
    this._connectPromise = null;
    this._pending = new Map(); // message_id -> {resolve, reject}
    this._writeQueue = []; // queued Uint8Array chunks awaiting flush
    this._readBuffer = []; // array of byte strings accumulated from the socket
    this._readBufferLength = 0;
    this._closed = false;
  }

  /** Lazily connect (idempotent — safe to call before every request). */
  async ensureConnected() {
    if (this._connectPromise) {
      return this._connectPromise;
    }
    const promise = this._connect().catch(err => {
      if (this._connectPromise === promise) {
        this._connectPromise = null;
      }
      throw err;
    });
    this._connectPromise = promise;
    return promise;
  }

  async _connect() {
    const path = echoSocketPath();
    tracingLog("Connecting to Echo daemon at", path.path);

    // Reset closed/buffer state so a fresh connection after _failAllPending()
    // or close() runs the read/write pumps normally with clean framing buffers.
    this._closed = false;
    this._writeQueue = [];
    this._readBuffer = [];
    this._readBufferLength = 0;

    const transport = lazy.socketTransportService.createUnixDomainTransport(
      path
    );
    this._transport = transport;

    this._rawInput = transport.openInputStream(0, 0, 0);
    this._output = transport.openOutputStream(0, 0, 0);
    this._input = Cc["@mozilla.org/scriptableinputstream;1"].createInstance(
      Ci.nsIScriptableInputStream
    );
    this._input.init(this._rawInput);

    this._waitForOutput(this._output);
    this._waitForInput(this._rawInput);

    // Fire-and-forget handshake, matching every existing Rust Echo client's
    // behavior (see file-level notice: Echo's daemon does not gate
    // MethodCall on it, but sending one keeps this client's connection
    // behavior consistent with and visible the same way as the others in
    // the daemon's logs).
    try {
      await this._sendHandshake();
    } catch (e) {
      console.error("[PyxisBridge] Echo handshake failed:", e);
      // If the socket itself died during handshake, fail connect so callers
      // don't wait out a full MethodCall timeout on a closed connection.
      if (this._closed) {
        throw e;
      }
      // Otherwise (e.g. handshake timeout), proceed since MethodCall does not require it.
    }
  }

  _sendHandshake() {
    const message = makeMessage("HANDSHAKE", {
      destination: "org.geckoOS.Echo",
      interface: "org.geckoOS.Echo",
      method: "HANDSHAKE",
      body: { version: "1.0.0", client: "pyxis-webapp-bridge" },
    });
    return this._sendAndAwait(message, /* timeoutMs */ 5000);
  }

  /**
   * Send a MethodCall for `service.method` and resolve with the response
   * body, or reject with an Error carrying Echo's error message.
   */
  async callMethod(service, method, params) {
    await this.ensureConnected();
    const destination = `org.geckoOS.${service}`;
    const message = makeMessage("METHOD_CALL", {
      destination,
      interface: destination,
      method,
      body: params ?? {},
    });
    const timeoutMs = HELD_CALL_TIMEOUT_MS.has(`${destination}.${method}`)
      ? HELD_CALL_TIMEOUT_MS_VALUE
      : DEFAULT_CALL_TIMEOUT_MS;
    const response = await this._sendAndAwait(message, timeoutMs);
    if (response.header.message_type === "ERROR") {
      throw new Error(
        response.body?.message ?? response.body?.error ?? "Echo error"
      );
    }
    return response.body;
  }

  _sendAndAwait(message, timeoutMs) {
    return new Promise((resolve, reject) => {
      const messageId = message.header.message_id;
      let timer = null;
      const cleanup = () => {
        this._pending.delete(messageId);
        if (timer) {
          timer.cancel();
          timer = null;
        }
      };
      this._pending.set(messageId, {
        resolve: msg => {
          cleanup();
          resolve(msg);
        },
        reject: err => {
          cleanup();
          reject(err);
        },
      });

      try {
        this._enqueueWrite(message);
      } catch (e) {
        cleanup();
        reject(e);
        return;
      }

      if (timeoutMs) {
        timer = Cc["@mozilla.org/timer;1"].createInstance(Ci.nsITimer);
        timer.initWithCallback(
          () => {
            const pending = this._pending.get(messageId);
            if (pending) {
              this._pending.delete(messageId);
              pending.reject(
                new Error(
                  `Echo request timed out after ${timeoutMs}ms (${message.header.destination}.${message.header.method})`
                )
              );
            }
          },
          timeoutMs,
          Ci.nsITimer.TYPE_ONE_SHOT
        );
      }
    });
  }

  // --- Outbound framing/write pump ---

  _enqueueWrite(message) {
    if (this._closed || !this._output) {
      throw new Error("Echo connection is closed");
    }
    const json = JSON.stringify(message);
    const bodyBytes = stringToUTF8Bytes(json);
    const framed = new Uint8Array(4 + bodyBytes.length);
    writeU32LE(framed, 0, bodyBytes.length);
    framed.set(bodyBytes, 4);
    this._writeQueue.push(framed);
    this._flushOutgoing();
  }

  _waitForOutput(output = this._output) {
    if (this._closed || !output || output !== this._output) {
      return;
    }
    output.asyncWait(
      {
        onOutputStreamReady: () => {
          if (output !== this._output) {
            return;
          }
          this._flushOutgoing();
        },
      },
      0,
      0,
      Services.tm.currentThread
    );
  }

  _flushOutgoing() {
    if (this._closed || !this._output || this._writeQueue.length === 0) {
      return;
    }
    while (this._writeQueue.length) {
      const chunk = this._writeQueue[0];
      try {
        const written = this._output.write(
          bytesToBinaryString(chunk),
          chunk.length
        );
        if (written < chunk.length) {
          this._writeQueue[0] = chunk.subarray(written);
          this._waitForOutput(this._output);
          return;
        }
        this._writeQueue.shift();
      } catch (e) {
        if (e.result === Cr.NS_BASE_STREAM_WOULD_BLOCK) {
          this._waitForOutput(this._output);
          return;
        }
        this._failAllPending(e);
        return;
      }
    }
  }

  // --- Inbound framing/read pump ---

  _waitForInput(rawInput = this._rawInput) {
    if (this._closed || !rawInput || rawInput !== this._rawInput) {
      return;
    }
    rawInput.asyncWait(
      { onInputStreamReady: () => this._onInputReady(rawInput) },
      0,
      0,
      Services.tm.currentThread
    );
  }

  _onInputReady(rawInput = this._rawInput) {
    if (this._closed || !rawInput || rawInput !== this._rawInput) {
      return;
    }
    try {
      let available;
      while ((available = this._rawInput.available()) > 0) {
        // NOTE: nsIScriptableInputStream.read() truncates at the first NUL
        // byte (see its IDL doc comment) — Echo's 4-byte length-prefix
        // header routinely contains NUL bytes (any body under 256 bytes
        // has a zero high byte), which would silently corrupt framing.
        // readBytes() is the NUL-safe counterpart and is what must be used
        // here.
        const chunk = this._input.readBytes(available);
        this._readBuffer.push(chunk);
        this._readBufferLength += chunk.length;
        this._drainMessages();
      }
      this._waitForInput(rawInput);
    } catch (e) {
      if (e.result === Cr.NS_BASE_STREAM_WOULD_BLOCK) {
        this._waitForInput(rawInput);
        return;
      }
      // NS_BASE_STREAM_CLOSED or any other error: treat as connection loss.
      this._failAllPending(e);
    }
  }

  /** Pull as many complete length-prefixed messages out of the read buffer as possible. */
  _drainMessages() {
    for (;;) {
      if (this._readBufferLength < 4) {
        return;
      }
      const header = this._peekBytes(4);
      const bodyLen = readU32LE(header, 0);
      if (this._readBufferLength < 4 + bodyLen) {
        return;
      }
      this._consumeBytes(4);
      const bodyBytes = this._consumeBytes(bodyLen);
      let message;
      try {
        message = JSON.parse(utf8BytesToString(bodyBytes));
      } catch (e) {
        console.error("[PyxisBridge] Failed to parse Echo message JSON:", e);
        continue;
      }
      this._handleIncoming(message);
    }
  }

  _handleIncoming(message) {
    const type = message.header?.message_type;
    if (type === "METHOD_RETURN" || type === "ERROR" || type === "HANDSHAKE_ACK") {
      const lookupId = message.header.reply_to ?? message.header.message_id;
      const pending = this._pending.get(lookupId);
      if (pending) {
        pending.resolve(message);
      } else {
        tracingLog(
          "[PyxisBridge] No pending Echo request for reply id",
          lookupId
        );
      }
    } else if (type === "SIGNAL") {
      dispatchSignalToSubscribers(message);
    }
    // Other message types (STATUS, PING, etc.) are not expected unsolicited
    // on this connection and are ignored.
  }

  _failAllPending(error) {
    this._closed = true;
    const rawInput = this._rawInput;
    const output = this._output;
    const transport = this._transport;
    this._rawInput = null;
    this._input = null;
    this._output = null;
    this._transport = null;
    this._writeQueue = [];
    this._readBuffer = [];
    this._readBufferLength = 0;

    try {
      rawInput?.close();
    } catch (e) {
      // ignore
    }
    try {
      output?.close();
    } catch (e) {
      // ignore
    }
    try {
      transport?.close(Cr.NS_OK);
    } catch (e) {
      // ignore
    }

    for (const pending of this._pending.values()) {
      pending.reject(error);
    }
    this._pending.clear();
    // Allow a fresh connection attempt next time a call comes in.
    this._connectPromise = null;
  }

  // --- Byte-buffer helpers over the array-of-strings read buffer ---

  _peekBytes(count) {
    // Concatenate just enough leading chunks to satisfy `count` bytes.
    let collected = "";
    let need = count;
    for (const chunk of this._readBuffer) {
      if (need <= 0) {
        break;
      }
      collected += chunk.slice(0, need);
      need -= chunk.length;
    }
    return collected;
  }

  _consumeBytes(count) {
    let remaining = count;
    let out = "";
    while (remaining > 0 && this._readBuffer.length) {
      const chunk = this._readBuffer[0];
      if (chunk.length <= remaining) {
        out += chunk;
        remaining -= chunk.length;
        this._readBuffer.shift();
      } else {
        out += chunk.slice(0, remaining);
        this._readBuffer[0] = chunk.slice(remaining);
        remaining = 0;
      }
    }
    this._readBufferLength -= count - remaining;
    return out;
  }

  close() {
    this._failAllPending(new Error("Echo connection closed"));
  }
}

// --- Wire-format helpers ---
//
// These operate on binary strings (each character's code point is one byte,
// 0-255) since that is what nsIOutputStream.write()/nsIScriptableInputStream
// .read() speak — the same idiom devtools/shared/transport/stream-utils.js
// and devtools/shared/transport/transport.js use for this exact class of
// problem in this tree.

function writeU32LE(uint8Array, offset, value) {
  uint8Array[offset] = value & 0xff;
  uint8Array[offset + 1] = (value >>> 8) & 0xff;
  uint8Array[offset + 2] = (value >>> 16) & 0xff;
  uint8Array[offset + 3] = (value >>> 24) & 0xff;
}

function readU32LE(binaryString, offset) {
  return (
    (binaryString.charCodeAt(offset) |
      (binaryString.charCodeAt(offset + 1) << 8) |
      (binaryString.charCodeAt(offset + 2) << 16) |
      (binaryString.charCodeAt(offset + 3) << 24)) >>>
    0
  );
}

function stringToUTF8Bytes(str) {
  return new TextEncoder().encode(str);
}

function utf8BytesToString(binaryString) {
  const bytes = new Uint8Array(binaryString.length);
  for (let i = 0; i < binaryString.length; i++) {
    bytes[i] = binaryString.charCodeAt(i);
  }
  return new TextDecoder().decode(bytes);
}

function bytesToBinaryString(uint8Array) {
  // String.fromCharCode with a spread is fine here — Echo frames are
  // JSON-sized IPC payloads, not bulk-data transfers, so this is not the
  // multi-megabyte hot path that would make this approach a problem.
  let out = "";
  const CHUNK = 0x4000;
  for (let i = 0; i < uint8Array.length; i += CHUNK) {
    out += String.fromCharCode.apply(
      null,
      uint8Array.subarray(i, i + CHUNK)
    );
  }
  return out;
}

let gMessageCounter = 0;
function nextMessageId() {
  // Real UUIDs aren't necessary here (Echo only needs message_id/reply_to
  // to be unique per-connection for correlation, per echo/src/message.rs);
  // a counter plus a random suffix is enough and avoids pulling in a UUID
  // dependency for chrome JS.
  gMessageCounter += 1;
  return `pyxis-${Date.now().toString(36)}-${gMessageCounter}-${Math.random()
    .toString(36)
    .slice(2, 8)}`;
}

/**
 * Builds a Message matching echo/src/message.rs's Message/MessageHeader
 * shape exactly (field names and casing are load-bearing — the daemon
 * deserializes this with serde using these exact keys).
 */
function makeMessage(messageType, { destination, interface: iface, method, body }) {
  return {
    header: {
      message_id: nextMessageId(),
      message_type: messageType,
      sender: null,
      destination: destination ?? "",
      interface: iface ?? "",
      method: method ?? "",
      signature: "",
      reply_to: null,
      timestamp: Date.now(),
    },
    body: body ?? {},
  };
}

function tracingLog(...args) {
  if (Services.prefs.getBoolPref("pyxis.bridge.verbose_logging", false)) {
    console.log("[PyxisBridge]", ...args);
  }
}

// --- Signal fan-out ---
//
// Echo sends every signal to every connection it has
// (IPCBus::broadcast_to_all_connections in echo/src/lib.rs), with no
// subscriber list, so this process's one connection hears all of them:
// every app's notifications, the root shell's output, which apps run. What
// reaches content is decided here alone. Each actor delivers a signal only
// if its document subscribed to it, and only if BRIDGE_SIGNALS lists it
// with a capability the document's app holds (forwardEventToContent).
function applyEvoVisibilityToActors(service, eventName, body) {
  if (service === "evo") {
    if (
      eventName === "WindowCreated" &&
      body?.window_id !== undefined &&
      typeof body?.app_id === "string"
    ) {
      gWindowIdToAppId.set(Number(body.window_id), body.app_id);
    } else if (eventName === "WindowClosed" && body?.window_id !== undefined) {
      gWindowIdToAppId.delete(Number(body.window_id));
    } else if (eventName === "WindowMinimized") {
      const targetAppId =
        typeof body?.app_id === "string"
          ? body.app_id
          : gWindowIdToAppId.get(Number(body?.window_id));
      const visible = body?.minimized === false;
      if (targetAppId) {
        for (const actor of gLiveActors) {
          if (actor._safeAppId() === targetAppId) {
            actor.setCompositorVisible(visible, visible ? "restored" : "minimized");
          }
        }
      }
    } else if (
      eventName === "WindowFocusChanged" &&
      body?.window_id !== undefined &&
      body?.window_id !== null
    ) {
      const targetAppId = gWindowIdToAppId.get(Number(body.window_id));
      if (targetAppId) {
        for (const actor of gLiveActors) {
          if (actor._safeAppId() === targetAppId) {
            actor.setCompositorVisible(true, "focused");
          }
        }
      }
    }
  } else if (service === "session" && eventName === "LockChanged") {
    const visible = !body?.locked;
    for (const actor of gLiveActors) {
      actor.setCompositorVisible(visible, "session-lock");
    }
  }
}

function dispatchSignalToSubscribers(message) {
  const service = message.header.interface?.replace(/^org\.geckoOS\./, "");
  const eventName = message.header.method;
  if (
    service === "nova" &&
    (eventName === "RegistryChanged" || eventName === "PermissionsChanged")
  ) {
    const targetAppId =
      typeof message.body?.appId === "string" && message.body.appId
        ? message.body.appId
        : null;
    invalidatePermissionsCache(targetAppId);
    if (
      eventName === "PermissionsChanged" &&
      targetAppId &&
      Array.isArray(message.body?.effective)
    ) {
      gPermissionsCache.set(
        targetAppId,
        message.body.effective.filter(p => typeof p === "string")
      );
    } else {
      // Refresh cached permissions for live subscribed appIds asynchronously
      // so subsequent capability-gated signals have a warm cache rather than
      // sitting empty until the next callService.
      const refreshAppIds = new Set();
      for (const actor of gLiveActors) {
        if (actor._subscriptions.size === 0) {
          continue;
        }
        const appId = actor._safeAppId();
        if (appId && (!targetAppId || appId === targetAppId)) {
          refreshAppIds.add(appId);
        }
      }
      for (const appId of refreshAppIds) {
        void fetchDeclaredPermissions(appId);
      }
    }
  }
  applyEvoVisibilityToActors(service, eventName, message.body);
  for (const actor of gLiveActors) {
    // A document that is going away can throw here. That must not cost the
    // others the signal, or escape into _onInputReady, which would take it
    // for a lost connection and fail every call in flight.
    try {
      actor.forwardEventToContent(service, eventName, message.body);
    } catch (e) {
      console.error(`[PyxisBridge] could not deliver ${service}.${eventName}:`, e);
    }
  }
}

// Process-wide singleton connection. Sharing one EchoConnection across all
// PyxisBridgeParent actors (rather than one socket per actor) keeps Echo's
// connection table tidy and, more importantly, is required for signal
// fan-out to work at all once subscriptions are wired end-to-end — a
// signal has to arrive on SOME connection, and every actor needs a shot at
// matching it against its own subscriptions (see dispatchSignalToSubscribers
// above), which only works cleanly with a shared connection.
let gEchoConnection = null;
function getEchoConnection() {
  if (!gEchoConnection) {
    gEchoConnection = new EchoConnection();
  }
  return gEchoConnection;
}

// --- Capability allow-list ---
//
// Every (service, method) this bridge forwards to Echo, with the capability
// an app must hold to call it, or `null` where the call needs none. The
// strings are Nova's capability vocabulary: what an app holds is the
// `permissions` Nova's GetInfo returns for it (see _getDeclaredPermissions).
//
// Deny by default: a pair that is not listed is refused, the same way as a
// capability the app did not declare. Until 2026-09-27 it was the other way
// round. The table had only filesystem, power, notification and appManager,
// an unlisted pair came back as `null`, and `null` meant allowed. So every
// other service was open to every pyxis:// app, a third-party .ipk
// included: a root shell (terminal), any command as root (process.spawn),
// other apps' windows (evo), launching and killing apps (nova, appHost),
// package installs (installer), volume and output (audio), settings
// writes, and the bus's own RegisterService and EmitSignal. Now a new Echo
// method, or a whole new service, stays out of apps' reach until someone
// decides here what it needs.
//
// `null` is for calls that change nothing and expose nothing about the user
// or other apps: readings like the OS version or the battery level.
// Everything else names a capability, even where every app that calls it
// today holds that capability anyway.
//
// The vocabulary lives in three places that must change together: this
// table; Nova (echo/src/services/nova.rs: KNOWN_CAPABILITIES, its
// privileged subset PRIVILEGED_CAPABILITIES, and the dotted appinfo names
// that map onto them); and vroom (src/config.rs and
// schemas/{appinfo,pyxis}.schema.json), which checks the dotted names in
// packages. A test in nova.rs reads this table and fails on a capability
// Nova does not know.
//
// The privileged capabilities are notifications:manage, appManager:install,
// appManager:manage, settings:write, windows:manage and terminal, and the
// three that widen a filesystem call's reach, filesystem:user,
// filesystem:system and filesystem:apps (FILESYSTEM_SCOPES, below). Nova
// grants them only to an app on its trust list (PRIVILEGED_APP_IDS) at a
// root-mediated tier, so declaring one does not get it.
//
// Deliberately absent, so that no app can call them:
//   - process.*, which spawns any command, and signals any pid, as root;
//   - Echo.*, the bus itself: RegisterService would let an app take over a
//     service's name, and EmitSignal would let it forge any signal;
//   - Rockhopper.* and Shift.*, which those daemons register on the bus
//     (Rockhopper's ExecuteApplication runs the executable it is given);
//   - evo.SwitchWorkspace and evo.CreateWorkspace, which Evo refuses until
//     workspaces are real (evo.CreateWindow is gone: it spawned a program
//     as root for whatever app id the caller named);
//   - deviceInfo.getSerial and deviceInfo.getUUID, the DMI serial and
//     /etc/machine-id: stable identifiers that no app needs;
//   - windowManager.*, which PyxisBridgeChild exposes and no service
//     implements.
const BRIDGE_CAPABILITIES = {
  // What the OS is, and whether the bus answers. getInfo includes the
  // hostname, so deviceInfo.getHostname needs nothing either.
  system: {
    getInfo: null,
    ping: null,
    listServices: null,
  },
  // Files. Every call is also confined to one directory tree, its scope,
  // which _scopeFilesystemCall stamps on it (FILESYSTEM_SCOPES, below).
  filesystem: {
    readFile: "filesystem:read",
    readDirectory: "filesystem:read",
    exists: "filesystem:read",
    stat: "filesystem:read",
    writeFile: "filesystem:write",
    appendFile: "filesystem:write",
    deleteFile: "filesystem:write",
    deleteDirectory: "filesystem:write",
    createDirectory: "filesystem:write",
    copy: "filesystem:write",
    move: "filesystem:write",
    chmod: "filesystem:write",
  },
  power: {
    shutdown: "power",
    reboot: "power",
    sleep: "power",
    wake: "power",
    setScreenBrightness: "power",
    setScreenTimeout: "power",
    getBatteryStatus: null,
    getScreenBrightness: null,
    getScreenTimeout: null,
  },
  deviceInfo: {
    getOS: null,
    getKernel: null,
    getModel: null,
    getManufacturer: null,
    getHardware: null,
    getHostname: null,
    getMemoryInfo: null,
    getCPUInfo: null,
    // The root filesystem's size and free space. The caller used to name
    // the path `df` ran on, which told any app whether a path existed, so
    // _handleCallService drops whatever the page sends.
    getStorageInfo: null,
  },
  // Connection state and addresses. getStatus carries the Wi-Fi SSID, which
  // says where the device is. network:read covers the readings;
  // network:manage (privileged: join an access point, repoint DNS) the rest.
  // An app declaring legacy `network` is normalised to network:read by Nova.
  network: {
    getStatus: "network:read",
    getType: "network:read",
    getInterfaces: "network:read",
    getIPAddress: "network:read",
    getGateway: "network:read",
    getDNS: "network:read",
    scanWifi: "network:read",
    connectWifi: "network:manage",
    disconnectWifi: "network:manage",
    setInterfaceEnabled: "network:manage",
    setDNS: "network:manage",
    setHostname: "network:manage",
  },
  // Every app reads pyxis.theme as it starts (the SDK's AppShell), so
  // reading is open. That is only safe because writing is privileged: the
  // store holds what the shell and Settings put in it, UI preferences.
  // Nothing secret may ever be kept there.
  settings: {
    get: null,
    getAll: null,
    set: "settings:write",
    delete: "settings:write",
    clear: "settings:write",
    reset: "settings:write",
  },
  // The clock in the device's time zone, the zones there are, and where the
  // device is for the sun (a location the user set, else the zone's city).
  // Pages take the offset from here because vela keeps the zone it started
  // with. Changing the zone is a system setting, so settings:write.
  time: {
    getClock: null,
    listTimezones: null,
    getLocation: null,
    setTimezone: "settings:write",
  },
  // The lock screen's half of the session (echo/src/services/session.rs).
  // Echo owns the decision: it checks the password, rate-limits wrong ones and
  // tells Evo, and Evo, not the page, draws nothing but the lock page while
  // locked. Asking to lock or to try a password is the shell's alone
  // (windows:manage, held by the shell's ids), so no app can lock the user out
  // or guess their password; reading the state needs nothing, like a clock.
  // Settings' Users & Lock section locks and logs out too, so those two take
  // settings:write (privileged, as windows:manage is) rather than the shell's
  // own capability. Unlocking stays the shell's.
  session: {
    lock: "settings:write",
    unlock: "windows:manage",
    switchUser: "settings:write",
    getState: null,
    logout: "settings:write",
  },
  // Users: their names and looks, and changing a password. Settings' Users
  // section and the lock page use it; settings:write, like the other things
  // that change the device. A wrong current password counts against the same
  // limit as a wrong unlock.
  accounts: {
    list: "settings:write",
    create: "settings:write",
    delete: "settings:write",
    setPassword: "settings:write",
    setProfile: "settings:write",
  },
  // Privileged access requests & in-memory grants (`org.geckoOS.privilege`).
  // Any app may ask for privilege, check its state, or drop its grant, while
  // authenticating (submitting the user's password) and dismissing prompts stay
  // behind windows:manage so only the shell's overlay prompt can unlock.
  privilege: {
    Request: null,
    request: null,
    Authenticate: "windows:manage",
    authenticate: "windows:manage",
    Dismiss: "windows:manage",
    dismiss: "windows:manage",
    Revoke: null,
    revoke: null,
    GetState: null,
    getState: null,
  },
  // Raising a notification is an app's own business. Reading or
  // dismissing the centre is the shell's: `list` returns every app's
  // notifications, text included, so it is privileged
  // (`notifications:manage`, which Nova grants only to PRIVILEGED_APP_IDS).
  notification: {
    show: "notifications",
    list: "notifications:manage",
    close: "notifications:manage",
    closeAll: "notifications:manage",
  },
  // The system's volume, mute and output device, and its named sounds.
  audio: {
    getVolume: "audio",
    setVolume: "audio",
    setMute: "audio",
    listOutputs: "audio",
    setDefaultOutput: "audio",
    getInputVolume: "audio",
    setInputVolume: "audio",
    setInputMute: "audio",
    listInputs: "audio",
    setDefaultInput: "audio",
  },
  sound: {
    play: "audio",
    list: "audio",
  },
  // Other apps: launching, closing and raising them, and the registry of
  // what is installed, each app's permissions included.
  nova: {
    Launch: "appManager:manage",
    Close: "appManager:manage",
    Focus: "appManager:manage",
    List: "appManager:manage",
    GetInfo: "appManager:manage",
    // An installed app's own icon, read from its webroot by Nova (a third-party
    // pyxis:// origin can't load another's image). Whoever may list the apps
    // may draw them.
    GetAppIcon: "appManager:manage",
    GetPermissions: "settings:write",
    SetPermission: "settings:write",
    Install: "appManager:install",
    Uninstall: "appManager:install",
    Reload: "appManager:install",
    // Opens an http(s) URL (Nova refuses any other scheme) in a new browser
    // window. No click is needed, so this is more than following a link.
    OpenExternalBrowser: "browser",
  },
  appHost: {
    LaunchApp: "appManager:manage",
    CloseApp: "appManager:manage",
    KillApp: "appManager:manage",
    RestartApp: "appManager:manage",
    ListApps: "appManager:manage",
    ListRunningApps: "appManager:manage",
    GetAppState: "appManager:manage",
    BindSurface: "appManager:manage",
    UnbindSurface: "appManager:manage",
    ListSurfaceSessions: "appManager:manage",
    SubscribeAppLifecycle: "appManager:manage",
  },
  // The pre-AppHost shim over the same lifecycle.
  appManager: {
    launch: "appManager:manage",
    close: "appManager:manage",
    list: "appManager:manage",
    getInfo: "appManager:manage",
    kill: "appManager:manage",
    restart: "appManager:manage",
    setActive: "appManager:manage",
    getActive: "appManager:manage",
    install: "appManager:install",
    uninstall: "appManager:install",
  },
  installer: {
    ListAvailable: "appManager:install",
    Inspect: "appManager:install",
    ListInstalled: "appManager:install",
    Install: "appManager:install",
    Remove: "appManager:install",
  },
  // A root shell: echo-daemon runs as root and forks the shell itself.
  terminal: {
    Open: "terminal",
    Write: "terminal",
    Resize: "terminal",
    Close: "terminal",
  },
  // Evo's control of any app's windows, and the shell's hold on the screen:
  // the overlay's input region and keyboard (SetOverlayRegion,
  // SetOverlayFocus, GetOverlayState) and the edges apps must leave to the
  // top bar and dock (SetWorkArea). All of it is windows:manage, which only
  // the shell's own trust-listed ids hold, so no app can reserve the screen,
  // take the keyboard for the overlay, or learn what the overlay covers.
  evo: {
    ListWindows: "windows:manage",
    GetRenderingStatus: "windows:manage",
    FocusWindow: "windows:manage",
    MinimizeWindow: "windows:manage",
    RestoreWindow: "windows:manage",
    MaximizeWindow: "windows:manage",
    UnmaximizeWindow: "windows:manage",
    CloseWindow: "windows:manage",
    SetOverlayRegion: "windows:manage",
    SetOverlayFocus: "windows:manage",
    SetWorkArea: "windows:manage",
    // Where each running app's dock icon is, so a window minimizes into it
    // and restores out of it. The shell's dock reports it.
    SetMinimizeTargets: "windows:manage",
    // Each app's name, accent and icon, for its title bar (looked up by the
    // window's trusted app id). The shell's app list sends it; an app could
    // otherwise rename or recolour every other app's windows.
    SetAppIdentity: "windows:manage",
    // The lock screen (evo/src/shell/lock.rs). Locking is the shell's to ask
    // for, and so are the idle time and where the unlock's Clear focuses from.
    // Unlocking is not here and never will be: only echo-daemon's verified
    // LockChanged unlocks Evo. GetLockState is echo-cli only.
    Lock: "windows:manage",
    SetIdleLock: "windows:manage",
    SetLockOrigin: "windows:manage",
    GetOverlayState: "windows:manage",
    // A picture of each window, for the overview's and the switcher's cards.
    // What an app draws is its own business: only the shell may see others'.
    GetWindowThumbnails: "windows:manage",
    // Window animation (evo/src/anim): the mode is a user setting, so
    // writing it is settings:write; reading the mode and the counters
    // reveals nothing about any app, so it needs nothing.
    SetAnimationSettings: "settings:write",
    GetAnimationState: null,
    GetOutputs: null,
    SetOutputConfig: "settings:write",
  },
};

// --- Signal allow-list ---
//
// Every (service, event) this bridge delivers to apps, with the capability
// an app must hold to receive it. The vocabulary is BRIDGE_CAPABILITIES',
// and the nova.rs test that checks that table reads this one too. `null`
// means the signal needs no capability: settings.SettingChanged carries a key
// and its value, and every key is already readable with settings.get, which
// needs none.
//
// Deny by default, since 2026-09-28. Before, an app could subscribe to any
// service:event, and Echo sends every signal to every connection, so any
// pyxis:// app, a third-party .ipk included, heard all of them: every app's
// notifications, title and body included, which notification.list keeps to
// notifications:manage; the Terminal app's root shell, its output and the
// keystrokes it echoes back; which apps run, and what gets installed. Now a
// subscription is checked when it is made (_handleSubscribe) and each signal
// again as it is delivered (forwardEventToContent), so a signal that is not
// listed here reaches no app.
//
// A signal needs what the calls on the same data need: whoever may list the
// notifications, run the shell, manage apps or windows, or set the volume
// may hear them change. The listeners today are Bloom (the notification
// centre and toasts, the dock and launcher through appHost and nova, volume
// in quick settings), the Terminal app, Settings' Sound page and the
// Installer app. evo's window events have no listener yet.
//
// Deliberately absent, so that no app hears them:
//   - Echo's own (Shutdown), and Shift's process events (pids and app ids);
//   - evo's GeckoReady, and the events PyxisBridgeChild's namespace helpers
//     name (appManager.onLaunched, power.onBatteryLow, filesystem.watch
//     and the rest), which no service emits. settings.onChange is one of
//     those names; the signal settings actually emits is SettingChanged;
//   - any other interface. Echo passes a raw SIGNAL frame, or
//     Echo.EmitSignal, from any client on to every connection, under
//     whatever interface the client names.
const BRIDGE_SIGNALS = {
  notification: {
    NotificationShown: "notifications:manage",
    NotificationClosed: "notifications:manage",
  },
  audio: {
    VolumeChanged: "audio",
    OutputChanged: "audio",
    InputVolumeChanged: "audio",
    InputChanged: "audio",
  },
  // One per key a set/delete/clear/reset changed: {key, value, previous,
  // exists}. The Settings app and the shell are different origins, so a
  // BroadcastChannel can't carry a change between them; this does.
  settings: {
    SettingChanged: null,
  },
  // The zone changed: {timezone, utc_offset_minutes}. Needs nothing, like
  // time.getClock, which tells any app the same.
  time: {
    TimezoneChanged: null,
  },
  // The lock changed ({locked, seq, mac}) or a try failed ({attempts_left,
  // retry_at_ms}). Says when the user is away, so windows:manage, like the
  // window signals.
  session: {
    LockChanged: "windows:manage",
    UnlockFailed: "windows:manage",
  },
  privilege: {
    PrivilegeRequested: "windows:manage",
    PrivilegeChanged: null,
  },
  terminal: {
    TerminalOpened: "terminal",
    TerminalOutput: "terminal",
    TerminalClosed: "terminal",
  },
  appHost: {
    AppStarting: "appManager:manage",
    AppRunning: "appManager:manage",
    AppSuspended: "appManager:manage",
    AppStopped: "appManager:manage",
    AppCrashed: "appManager:manage",
    AppSurfaceMapped: "appManager:manage",
    AppSurfaceUnmapped: "appManager:manage",
  },
  nova: {
    RegistryChanged: "appManager:manage",
    AppInstalled: "appManager:manage",
    AppUninstalled: "appManager:manage",
    AppLaunched: "appManager:manage",
    AppSuspended: "appManager:manage",
    AppResumed: "appManager:manage",
    PermissionsChanged: "settings:write",
  },
  installer: {
    InstallStarted: "appManager:install",
    InstallProgress: "appManager:install",
    InstallCompleted: "appManager:install",
    InstallFailed: "appManager:install",
    RemoveCompleted: "appManager:install",
    RemoveFailed: "appManager:install",
  },
  evo: {
    WindowCreated: "windows:manage",
    WindowClosed: "windows:manage",
    WindowMoved: "windows:manage",
    WindowMovedToWorkspace: "windows:manage",
    WindowFocusChanged: "windows:manage",
    WindowMinimized: "windows:manage",
    WindowMaximized: "windows:manage",
    WindowFullscreen: "windows:manage",
    // A global shortcut Evo caught (Super tap, Super+Tab, Super+Space,
    // Super+D). Gated like the window signals: it says what the user is
    // doing while another app has the keyboard.
    ShortcutTriggered: "windows:manage",
    OverlayFocusChanged: "windows:manage",
    // Alt+Tab, for Bloom's overview to draw (open/step/commit/cancel with
    // the selected window and the MRU order). Evo owns the keys and the
    // order; this is how the shell hears them.
    SwitcherChanged: "windows:manage",
    // A minimize's orb reached its dock icon ("land") or a restore left it
    // ("rise"): Bloom ripples the icon. Says which windows come and go.
    MinimizeLanded: "windows:manage",
    WorkspaceChanged: "windows:manage",
    WorkspaceCreated: "windows:manage",
    WorkspaceDeleted: "windows:manage",
    OutputsChanged: "settings:write",
  },
};

// --- Filesystem scopes ---
//
// Where an app's filesystem calls can reach. Echo can't tell apps apart (a
// connection carries no credentials, and a message's sender is whatever the
// client wrote), so this bridge, which can, stamps every filesystem call
// with a `scope`, and echo confines the call to it: it resolves each path
// beneath the scope's root with openat2(RESOLVE_BENEATH), and runs the call
// as geckouser, not root (echo/src/services/filesystem/scope.rs). The scope
// is written over anything the page sent, as notification.show's appId is.
//
// An app's scope is its own data, `app:<id>`, which echo keeps in
// /var/lib/pyxis/data/<id> and the app sees as `~`. Holding a capability
// here widens it:
//   - user: the user's home, which the app then sees as `~` instead;
//   - system: the whole root filesystem, read-only, reached with absolute
//     paths. /proc, /sys, /dev, /run and /tmp are other mounts, and stay out
//     of reach;
//   - apps: another app's own data, named by the call's `app` field. The
//     shell's desktop search reads Notes, Contacts and Tasks this way, and
//     adds notes and tasks.
// All three are privileged. The Files app holds user and system, and Bloom
// holds apps. Writing outside the home needs a capability that does not
// exist yet: it is to come with a "privileged access" unlock
// (docs/internal/pyxis-privileged-access-plan.md).
//
// Until 2026-10-06 there were no scopes. Echo ran every filesystem call as
// root on whatever path the page sent, so any app holding filesystem:write,
// a third-party .ipk included, could rewrite /etc/shadow, set a setuid bit
// or delete /usr/bin.
const FILESYSTEM_SCOPES = {
  user: "filesystem:user",
  system: "filesystem:system",
  apps: "filesystem:apps",
};

// The capability `table` (BRIDGE_CAPABILITIES or BRIDGE_SIGNALS) names for
// `member` of `service`: a string, `null` when it needs none, or `undefined`
// when the table does not list it, which the bridge refuses. Own properties
// only, so a name like `constructor` or `__proto__` can't find anything on
// Object.prototype.
function tableCapability(table, service, member) {
  if (typeof service !== "string" || typeof member !== "string") {
    return undefined;
  }
  if (!Object.hasOwn(table, service)) {
    return undefined;
  }
  const members = table[service];
  return Object.hasOwn(members, member) ? members[member] : undefined;
}

// Content chooses `service` and `method`, so they are cut short before they
// go into an error message or the log. A non-string is shown only by its
// type. String() would call the value's own toString, and a cloned object
// can carry a toString that is not a function ({ toString: null }), which
// makes String() throw.
function clip(value) {
  const text = typeof value === "string" ? value : `(${typeof value})`;
  return text.length > 64 ? `${text.slice(0, 64)}…` : text;
}

// A refusal is logged, and a refused call is thrown as well. The SDK's
// EchoClient answers a failed native call from its mock, so to the app a
// refusal can look like success, and a refused subscription tells the app
// nothing at all: its listener just never fires. This line is where either
// shows on a device: the shell's vela writes to /var/log/vela.log, and a
// Nova-launched app's to /var/log/echo.log (AppHost's children inherit
// echo-daemon's stdout). Each distinct refusal is logged once, and only the
// first DENIAL_LOG_LIMIT of them, so an app calling in a loop can't fill the
// disk.
const DENIAL_LOG_LIMIT = 200;
const gLoggedDenials = new Set();
function logDenial(detail) {
  if (!gLoggedDenials.has(detail) && gLoggedDenials.size < DENIAL_LOG_LIMIT) {
    gLoggedDenials.add(detail);
    console.warn(`[PyxisBridge] Pyxis capability denied: ${detail}`);
  }
}

function capabilityDenied(detail) {
  logDenial(detail);
  return new Error(`Pyxis capability denied: ${detail}`);
}

export class PyxisBridgeParent extends JSWindowActorParent {
  actorCreated() {
    // "service:event" keys this document subscribed to that passed the
    // check in _handleSubscribe.
    this._subscriptions = new Set();
    gLiveActors.add(this);
  }

  didDestroy() {
    this._destroyed = true;
    this._subscriptions.clear();
    gLiveActors.delete(this);
  }

  async receiveMessage(msg) {
    switch (msg.name) {
      case "PyxisBridge:CallService":
        return this._handleCallService(msg.data);
      case "PyxisBridge:Subscribe":
        return this._handleSubscribe(msg.data);
      default:
        console.error(`[PyxisBridge] unknown message: ${msg.name}`);
        return undefined;
    }
  }

  async _handleCallService(data) {
    const { service, method } = data;
    let { params } = data;
    const appId = this._appIdForThisActor();

    // --- Capability allow-list enforcement (the LS2-role-file check) ---
    // This MUST run before _echoRequest, not after — refusing here means
    // the call never reaches Echo at all, matching webOS's bus-level
    // enforcement model that pyxis-webos-parity-analysis.md and
    // nova-runtime-host-plan.md §5 both call out as the gap to close.
    const capability = this._capabilityFor(service, method);
    const call = `${clip(service)}.${clip(method)}`;
    if (capability === undefined) {
      throw capabilityDenied(`app '${appId}' called ${call}, which no app may call`);
    }
    if (capability !== null) {
      const declared = await this._getDeclaredPermissions(appId);
      if (!declared.includes(capability)) {
        throw capabilityDenied(
          `app '${appId}' did not declare '${capability}' (required for ${call})`
        );
      }
    }

    if (service === "filesystem") {
      params = await this._scopeFilesystemCall(appId, call, capability, params);
    }
    if (service === "deviceInfo" && method === "getStorageInfo") {
      params = {};
    }

    // Stamp who raised a notification. The store trusts `appId` only because
    // this is the one place it gets written, so overwrite whatever the page
    // sent: a page can put any appId it likes in its params, and the centre
    // would then show another app's name and icon over this app's text.
    if (service === "notification" && method === "show") {
      params = { ...params, appId };
    }
    if (service === "privilege") {
      const obj = params && typeof params === "object" ? params : {};
      if (method === "Request" || method === "request") {
        params = { ...obj, appId };
      } else if (
        method === "GetState" ||
        method === "getState" ||
        method === "Revoke" ||
        method === "revoke"
      ) {
        params = obj.appId === undefined ? { ...obj, appId } : obj;
      }
    }

    return this._echoRequest(service, method, params, appId);
  }

  async _hasPrivilegeGrant(appId) {
    try {
      const state = await this._echoRequest(
        "privilege",
        "GetState",
        { appId },
        appId
      );
      return Boolean(state && state.granted);
    } catch {
      return false;
    }
  }

  // Stamps `scope` on a filesystem call (FILESYSTEM_SCOPES): another app's
  // own data when the call names one in `app`, which needs filesystem:apps;
  // `system` when it names an absolute path, which needs filesystem:system
  // and may only read; otherwise the user's home, for an app holding
  // filesystem:user, or else the app's own data. A path that is not a string
  // is refused, so that echo can never read a path this check did not see.
  async _scopeFilesystemCall(appId, call, capability, params) {
    const { app: target, ...fields } =
      params && typeof params === "object" ? params : {};
    let absolute = false;
    for (const key of ["path", "source", "destination"]) {
      if (!Object.hasOwn(fields, key) || fields[key] === undefined) {
        continue;
      }
      if (typeof fields[key] !== "string") {
        throw new Error(`${call}: ${key} must be a string`);
      }
      absolute ||= fields[key].startsWith("/");
    }

    const declared = await this._getDeclaredPermissions(appId);
    let scope;
    if (target !== undefined && target !== appId) {
      const needed = FILESYSTEM_SCOPES.apps;
      if (typeof target !== "string" || !declared.includes(needed)) {
        throw capabilityDenied(
          `app '${appId}' did not declare '${needed}' (required for another app's data in ${call})`
        );
      }
      if (absolute) {
        throw new Error(`${call}: another app's data is reached with ~ paths, not absolute ones`);
      }
      // Echo checks the id is one, and refuses the call if not.
      scope = `app:${target}`;
    } else if (absolute) {
      const needed = FILESYSTEM_SCOPES.system;
      if (!declared.includes(needed)) {
        throw capabilityDenied(
          `app '${appId}' did not declare '${needed}' (required for an absolute path in ${call})`
        );
      }
      if (capability === "filesystem:write") {
        const granted = await this._hasPrivilegeGrant(appId);
        if (!granted) {
          throw capabilityDenied(
            `app '${appId}' called ${call} on an absolute path, and outside the home files are read-only`
          );
        }
      }
      scope = "system";
      return { ...fields, scope, appId };
    } else {
      scope = declared.includes(FILESYSTEM_SCOPES.user) ? "user" : `app:${appId}`;
    }
    return { ...fields, scope };
  }

  // PyxisBridgeChild sends a subscription the first time its page listens
  // to a service:event. It is kept only if BRIDGE_SIGNALS lists the pair and
  // the app holds the capability the table names; a refusal is logged. The
  // child never learns the answer (it sends with sendAsyncMessage), so a
  // refused listener simply never fires. Nothing is registered with Echo,
  // which sends every signal to every connection anyway: the filtering is
  // all in this file.
  async _handleSubscribe({ service, eventName }) {
    let appId;
    try {
      appId = this._appIdForThisActor();
    } catch (e) {
      console.error(`[PyxisBridge] ${e.message}`);
      return;
    }
    const capability = tableCapability(BRIDGE_SIGNALS, service, eventName);
    const signal = `${clip(service)}.${clip(eventName)}`;
    if (capability === undefined) {
      logDenial(`app '${appId}' subscribed to ${signal}, which no app may receive`);
      return;
    }
    if (capability !== null) {
      const declared = await this._getDeclaredPermissions(appId);
      // A failed lookup also answers with an empty list, but is not cached.
      // Refuse only on Nova's actual answer. While that is unknown the
      // subscription is kept, and forwardEventToContent delivers nothing on
      // it until the app's permissions are known: the child sends each
      // subscription once, so refusing now would lose it for good.
      if (!declared.includes(capability) && gPermissionsCache.has(appId)) {
        logDenial(
          `app '${appId}' did not declare '${capability}' (required to receive ${signal})`
        );
        return;
      }
    }
    // The document may have gone while Nova answered.
    if (!this._destroyed) {
      this._subscriptions.add(`${service}:${eventName}`);
    }
  }

  // Called by dispatchSignalToSubscribers() (this file, above) for every
  // signal that arrives. The subscription was checked when it was made; the
  // capability is checked again here, against the permissions Nova last
  // gave for this app. If the cache is missing (e.g. initial lookup failed
  // or RegistryChanged/PermissionsChanged just invalidated it), re-fetch
  // asynchronously before deciding rather than dropping the signal.
  forwardEventToContent(service, eventName, data) {
    if (!this._subscriptions.has(`${service}:${eventName}`)) {
      return;
    }
    const capability = tableCapability(BRIDGE_SIGNALS, service, eventName);
    if (capability === undefined) {
      return;
    }
    if (capability !== null) {
      const appId = this._appIdForThisActor();
      if (!gPermissionsCache.has(appId)) {
        void this._forwardEventAfterResolve(
          appId,
          capability,
          service,
          eventName,
          data
        );
        return;
      }
      const held = gPermissionsCache.get(appId);
      if (!held.includes(capability)) {
        logDenial(
          `app '${appId}' did not declare '${capability}' (required to receive ${service}.${eventName})`
        );
        return;
      }
    }
    this.sendAsyncMessage("PyxisBridge:Event", { service, eventName, data });
  }

  async _forwardEventAfterResolve(
    appId,
    capability,
    service,
    eventName,
    data
  ) {
    try {
      const held = await this._getDeclaredPermissions(appId);
      if (
        this._destroyed ||
        !this._subscriptions.has(`${service}:${eventName}`)
      ) {
        return;
      }
      if (!held.includes(capability)) {
        if (gPermissionsCache.has(appId)) {
          logDenial(
            `app '${appId}' did not declare '${capability}' (required to receive ${service}.${eventName})`
          );
        }
        return;
      }
      this.sendAsyncMessage("PyxisBridge:Event", { service, eventName, data });
    } catch (e) {
      console.error(
        `[PyxisBridge] could not deliver ${service}.${eventName}:`,
        e
      );
    }
  }

  // --- App identity ---
  //
  // The pyxis:// origin IS the app-id (scheme+host, per
  // PyxisProtocolHandler.h's URI_STD + no-ORIGIN_IS_FULL_SPEC design), so
  // deriving app-id from the actor's own document principal is exactly
  // correct and requires no separate lookup table.
  _appIdForThisActor() {
    const principal = this.manager?.documentPrincipal;
    const uri = principal?.URI;
    if (!uri || uri.scheme !== "pyxis") {
      throw new Error("PyxisBridge used from a non-pyxis:// document");
    }
    return uri.host; // the app-id
  }

  _safeAppId() {
    try {
      return this._appIdForThisActor();
    } catch {
      return null;
    }
  }

  /**
   * Toggle compositor-driven background throttling for non-shell app documents
   * when Evo minimizes, restores, focuses, or session-locks their window.
   */
  setCompositorVisible(visible, reason = "") {
    const appId = this._safeAppId();
    if (
      !appId ||
      appId === "system" ||
      appId === "bloom" ||
      appId === "bloom-overlay"
    ) {
      return false;
    }
    const next = Boolean(visible);
    this._compositorVisible = next;
    this._compositorVisibilityReason = String(reason || "");
    try {
      const topBc = this.browsingContext?.top;
      const browser = topBc?.embedderElement || null;
      if (browser && "docShellIsActive" in browser) {
        browser.docShellIsActive = next;
      }
      if (topBc && "isActive" in topBc) {
        topBc.isActive = next;
      }
    } catch (e) {
      tracingLog("setCompositorVisible failed for", appId, e);
    }
    return true;
  }

  // --- Capability allow-list ---
  //
  // The capability an app must hold to call `service.method`: a string
  // from BRIDGE_CAPABILITIES (above this class), `null` when the call needs
  // none, or `undefined` when the bridge does not forward it at all, which
  // _handleCallService refuses.
  _capabilityFor(service, method) {
    return tableCapability(BRIDGE_CAPABILITIES, service, method);
  }

  // Resolves an app-id's declared permissions[] from Nova's registry (now a
  // real in-process service inside echo-daemon, see
  // echo/src/services/nova.rs — `org.geckoOS.nova` -> `GetInfo`, per
  // nova-runtime-host-plan.md §5). Cached per app-id and invalidated/refreshed
  // via `nova.RegistryChanged` / `nova.PermissionsChanged` signals in
  // dispatchSignalToSubscribers(). On any failure to reach Nova (not
  // installed, Echo down), fail closed without caching: an empty array denies
  // every capability-gated call rather than silently allowing them.
  _getDeclaredPermissions(appId) {
    return fetchDeclaredPermissions(appId);
  }

  // --- Echo transport ---
  //
  // Routes the bridge's (service, method, params) call onto Echo's real
  // Unix socket via the shared EchoConnection (see file-level notice for
  // the wire-protocol details this depends on). Errors thrown here
  // (connection failure, Echo-side error response, timeout) propagate as a
  // rejected sendQuery() promise back to PyxisBridgeChild's _callService,
  // exactly like the capability-denial path above already does.
  async _echoRequest(service, method, params, _appId) {
    const connection = getEchoConnection();
    const result = await connection.callMethod(service, method, params ?? {});
    if (service === "evo" && method === "ListWindows" && Array.isArray(result?.windows)) {
      for (const win of result.windows) {
        if (win?.window_id !== undefined && typeof win?.app_id === "string") {
          gWindowIdToAppId.set(Number(win.window_id), win.app_id);
        }
      }
    }
    return result;
  }
}
